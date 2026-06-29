//! 供应商连通性检查服务（reachability）
//!
//! 先探测供应商 `base_url` 是否可达，再对 Claude / Codex 供应商发送极小的
//! agent 风格真实请求：
//! - base_url 探测阶段收到任意 HTTP 响应（200/4xx/5xx）即判定"可达"；
//! - 仅 DNS / 连接被拒 / TLS / 超时等网络级错误判定"不可达"；
//! - Claude / Codex 会继续发送 `max_tokens/max_output_tokens = 1` 的最小请求，
//!   以识别鉴权、模型名、UA/agent 限制等真实请求问题。
//!
//! ## 设计取舍：可达 ≠ 可用
//!
//! base_url 可达只说明端口和网关存活；agent 探测才验证当前 provider 配置是否能
//! 通过类似 Claude Code / Codex 的真实请求。该请求可能产生极少量 token 费用。
//!
//! ## 与故障转移的关系（重要不变量）
//!
//! 连通性检查 **绝不** 触碰故障转移熔断器：即使 agent 探测返回 403/401/5xx，
//! 也只影响本次检测结果，不会把供应商标记进熔断状态。熔断器只由
//! `proxy/forwarder.rs` 转发真实业务流量的成败驱动（被动）。

use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, ACCEPT_ENCODING, CONTENT_TYPE, USER_AGENT};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::time::Instant;

use crate::app_config::AppType;
use crate::error::AppError;
use crate::provider::Provider;
use crate::proxy::providers::{get_adapter, AuthStrategy, ClaudeAdapter, ProviderAdapter};

const CLAUDE_AGENT_USER_AGENT: &str = "claude-cli/2.1.161 (external, cli)";
const CODEX_AGENT_USER_AGENT: &str =
    "codex_cli_rs/0.77.0 (Windows 10.0.26100; x86_64) WindowsTerminal";
const CLAUDE_CODE_BETA: &str = "claude-code-20250219";
const STREAM_CHECK_SESSION_ID: &str = "cc-switch-stream-check";
const DEFAULT_CLAUDE_AGENT_PROBE_MODEL: &str = "claude-opus-4-8";
const DEFAULT_CODEX_AGENT_PROBE_MODEL: &str = "gpt-5.5";

/// 健康状态枚举
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum HealthStatus {
    Operational,
    Degraded,
    Failed,
}

/// 连通性检查配置
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StreamCheckConfig {
    /// 单次探测超时（秒）
    pub timeout_secs: u64,
    /// 超时类失败的最大重试次数
    pub max_retries: u32,
    /// 降级阈值（毫秒）：可达但 TTFB 超过该值判定为"较慢"
    pub degraded_threshold_ms: u64,
}

impl Default for StreamCheckConfig {
    fn default() -> Self {
        // 可达性探测打的是 base_url 的小请求（仅读响应头），不等待模型生成，故超时远小于
        // 旧的真实请求检查（45s → 8s）；降级阈值沿用旧尺度 6000ms——探测 TTFB 一般远低于
        // 此，仅在确实很慢时才标"较慢"，避免把 1 秒多的正常延迟误判为降级。
        Self {
            timeout_secs: 8,
            max_retries: 1,
            degraded_threshold_ms: 6000,
        }
    }
}

/// 连通性检查结果
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StreamCheckResult {
    pub status: HealthStatus,
    pub success: bool,
    pub message: String,
    pub response_time_ms: Option<u64>,
    pub http_status: Option<u16>,
    /// Agent 探测实际使用的模型；仅 base_url 可达性检查时为空串。
    pub model_used: String,
    pub tested_at: i64,
    pub retry_count: u32,
    /// 细粒度错误分类；agent 探测失败时用于前端区分“base 可达但真实请求失败”。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_category: Option<String>,
}

struct AgentProbeRequest {
    url: String,
    headers: HeaderMap,
    body: Value,
    model: String,
    probe_kind: &'static str,
}

struct AgentProbeResponse {
    status: u16,
    body_snippet: Option<String>,
}

/// 连通性检查服务
pub struct StreamCheckService;

impl StreamCheckService {
    /// 执行连通性检查（仅对超时类失败重试）。
    ///
    /// `base_url_override`：用于 Copilot 等需要从 OAuth 管理器动态解析端点的供应商，
    /// 由命令层预先解析后传入；其余供应商传 `None`，由本服务从 `settings_config` 提取。
    pub async fn check_with_retry(
        app_type: &AppType,
        provider: &Provider,
        config: &StreamCheckConfig,
        base_url_override: Option<String>,
    ) -> Result<StreamCheckResult, AppError> {
        let effective = Self::merge_provider_config(provider, config);

        let mut last_result: Option<StreamCheckResult> = None;
        for attempt in 0..=effective.max_retries {
            let start = Instant::now();
            let result = Self::check_once(
                app_type,
                provider,
                &effective,
                base_url_override.clone(),
                start,
            )
            .await?;

            if result.success {
                return Ok(StreamCheckResult {
                    retry_count: attempt,
                    ..result
                });
            }

            // 仅超时 / abort 类网络抖动值得重试；连接被拒、DNS 失败等立即返回。
            if Self::should_retry(&result.message) && attempt < effective.max_retries {
                last_result = Some(result);
                continue;
            }
            return Ok(StreamCheckResult {
                retry_count: attempt,
                ..result
            });
        }

        Ok(last_result.unwrap_or_else(|| StreamCheckResult {
            status: HealthStatus::Failed,
            success: false,
            message: "Check failed".to_string(),
            response_time_ms: None,
            http_status: None,
            model_used: String::new(),
            tested_at: chrono::Utc::now().timestamp(),
            retry_count: effective.max_retries,
            error_category: None,
        }))
    }

    /// 合并供应商单独配置（`meta.testConfig`，仅当 `enabled`）与全局配置。
    fn merge_provider_config(provider: &Provider, global: &StreamCheckConfig) -> StreamCheckConfig {
        let tc = provider
            .meta
            .as_ref()
            .and_then(|m| m.test_config.as_ref())
            .filter(|tc| tc.enabled);

        match tc {
            Some(tc) => StreamCheckConfig {
                timeout_secs: tc.timeout_secs.unwrap_or(global.timeout_secs),
                max_retries: tc.max_retries.unwrap_or(global.max_retries),
                degraded_threshold_ms: tc
                    .degraded_threshold_ms
                    .unwrap_or(global.degraded_threshold_ms),
            },
            None => global.clone(),
        }
    }

    /// 单次连通性探测。
    async fn check_once(
        app_type: &AppType,
        provider: &Provider,
        config: &StreamCheckConfig,
        base_url_override: Option<String>,
        start: Instant,
    ) -> Result<StreamCheckResult, AppError> {
        let base_url = match base_url_override {
            Some(b) => b,
            None => Self::resolve_base_url(app_type, provider)?,
        };

        let client = crate::proxy::http_client::get();
        let timeout = std::time::Duration::from_secs(config.timeout_secs);
        let ua = Self::custom_user_agent(provider);

        let reachability = Self::probe_reachability(&client, &base_url, timeout, ua.clone()).await;
        let reachability_status = reachability.as_ref().ok().copied();
        let response_time = start.elapsed().as_millis() as u64;
        let reachability_result =
            Self::build_result(reachability, response_time, config.degraded_threshold_ms);

        if !reachability_result.success {
            return Ok(reachability_result);
        }

        let probe_request = match Self::build_agent_probe_request(app_type, provider, &base_url, ua)
        {
            Ok(Some(request)) => request,
            Ok(None) => return Ok(reachability_result),
            Err(err) => {
                return Ok(Self::build_agent_config_failure_result(
                    err,
                    start.elapsed().as_millis() as u64,
                    reachability_status,
                ));
            }
        };

        let probe_result = Self::probe_agent_request(&client, probe_request, timeout).await;
        let response_time = start.elapsed().as_millis() as u64;
        Ok(Self::build_agent_result(
            probe_result,
            response_time,
            config.degraded_threshold_ms,
            reachability_status,
        ))
    }

    /// 解析供应商 `base_url`。
    ///
    /// 连通性探测只需打到 base（origin 或用户配置的 base 路径）即可——任何 HTTP
    /// 响应都证明端口可达，因此无需像旧的真实请求检查那样解析具体 API 路径
    /// （`/v1/messages` vs `/chat/completions` vs `:streamGenerateContent`）。
    ///
    /// 官方供应商（`category == "official"`）base_url 故意留空（走客户端默认/OAuth 端点），
    /// 没有 cc-switch 能可靠探测的目标——这类供应商的连通检测按钮在前端已隐藏
    /// （见 `ProviderCard.tsx`），故此处对其提取失败直接报错即可，不做官方端点回退。
    fn resolve_base_url(app_type: &AppType, provider: &Provider) -> Result<String, AppError> {
        match app_type {
            // 累加模式应用的 settings_config 结构与 Claude/Codex/Gemini 不同，
            // 不走 adapter，直接按各自约定提取 base_url。
            AppType::OpenCode => {
                let npm = Self::extract_opencode_npm(provider);
                Self::resolve_opencode_base_url(provider, npm.as_deref())
            }
            AppType::OpenClaw => Self::extract_openclaw_base_url(provider),
            AppType::Hermes => Self::extract_hermes_base_url(provider),
            AppType::ClaudeDesktop => ClaudeAdapter::new()
                .extract_base_url(provider)
                .map_err(|e| AppError::Message(format!("Failed to extract base_url: {e}"))),
            _ => get_adapter(app_type)
                .extract_base_url(provider)
                .map_err(|e| AppError::Message(format!("Failed to extract base_url: {e}"))),
        }
    }

    /// 轻量可达性探测：GET `base_url`，收到任意 HTTP 响应即可达。
    ///
    /// - `send()` 在收到响应头时即返回，故计时天然是 TTFB；不读 body。
    /// - reqwest 对任何 HTTP 状态码都返回 `Ok`，只有网络级错误进 `Err`——
    ///   这正是"任何响应都算可达、只有连不上才算失败"的语义。
    async fn probe_reachability(
        client: &Client,
        base_url: &str,
        timeout: std::time::Duration,
        custom_ua: Option<HeaderValue>,
    ) -> Result<u16, AppError> {
        let url = base_url.trim();
        if url.is_empty() {
            return Err(AppError::Message("base_url 为空".to_string()));
        }

        let mut req = client
            .get(url)
            .timeout(timeout)
            .header("accept", "*/*")
            .header("accept-encoding", "identity");
        // 复用供应商自定义 UA（部分网关按 UA 白名单放行），与转发路径口径一致。
        if let Some(ua) = custom_ua {
            req = req.header("user-agent", ua);
        }

        match req.send().await {
            Ok(resp) => Ok(resp.status().as_u16()),
            Err(e) => Err(Self::map_request_error(e)),
        }
    }

    /// 将 base_url 可达性探测原始结果包装成 `StreamCheckResult`。
    fn build_result(
        result: Result<u16, AppError>,
        response_time: u64,
        degraded_threshold_ms: u64,
    ) -> StreamCheckResult {
        let tested_at = chrono::Utc::now().timestamp();
        match result {
            Ok(status) => StreamCheckResult {
                status: Self::determine_status(response_time, degraded_threshold_ms),
                success: true,
                message: "Reachable".to_string(),
                response_time_ms: Some(response_time),
                http_status: Some(status),
                model_used: String::new(),
                tested_at,
                retry_count: 0,
                error_category: None,
            },
            Err(e) => StreamCheckResult {
                status: HealthStatus::Failed,
                success: false,
                message: e.to_string(),
                response_time_ms: Some(response_time),
                http_status: None,
                model_used: String::new(),
                tested_at,
                retry_count: 0,
                error_category: None,
            },
        }
    }

    fn build_agent_config_failure_result(
        error: AppError,
        response_time: u64,
        reachability_status: Option<u16>,
    ) -> StreamCheckResult {
        StreamCheckResult {
            status: HealthStatus::Failed,
            success: false,
            message: format!("Reachable, but agent probe could not run: {error}"),
            response_time_ms: Some(response_time),
            http_status: reachability_status,
            model_used: String::new(),
            tested_at: chrono::Utc::now().timestamp(),
            retry_count: 0,
            error_category: Some("agent_probe_config".to_string()),
        }
    }

    fn build_agent_result(
        result: Result<(AgentProbeRequest, AgentProbeResponse), AppError>,
        response_time: u64,
        degraded_threshold_ms: u64,
        reachability_status: Option<u16>,
    ) -> StreamCheckResult {
        let tested_at = chrono::Utc::now().timestamp();
        match result {
            Ok((request, response)) if (200..300).contains(&response.status) => StreamCheckResult {
                status: Self::determine_status(response_time, degraded_threshold_ms),
                success: true,
                message: format!("Reachable; {} agent probe succeeded", request.probe_kind),
                response_time_ms: Some(response_time),
                http_status: Some(response.status),
                model_used: request.model,
                tested_at,
                retry_count: 0,
                error_category: None,
            },
            Ok((request, response)) => {
                let (category, reason) = Self::classify_agent_http_failure(
                    response.status,
                    response.body_snippet.as_deref(),
                );
                StreamCheckResult {
                    status: HealthStatus::Failed,
                    success: false,
                    message: Self::format_agent_http_failure(
                        request.probe_kind,
                        response.status,
                        &reason,
                        response.body_snippet.as_deref(),
                    ),
                    response_time_ms: Some(response_time),
                    http_status: Some(response.status),
                    model_used: request.model,
                    tested_at,
                    retry_count: 0,
                    error_category: Some(category.to_string()),
                }
            }
            Err(e) => StreamCheckResult {
                status: HealthStatus::Failed,
                success: false,
                message: format!("Reachable, but agent probe failed: {e}"),
                response_time_ms: Some(response_time),
                http_status: reachability_status,
                model_used: String::new(),
                tested_at,
                retry_count: 0,
                error_category: Some("agent_probe_network".to_string()),
            },
        }
    }

    fn determine_status(latency_ms: u64, threshold: u64) -> HealthStatus {
        if latency_ms <= threshold {
            HealthStatus::Operational
        } else {
            HealthStatus::Degraded
        }
    }

    fn should_retry(msg: &str) -> bool {
        let lower = msg.to_lowercase();
        lower.contains("timeout") || lower.contains("abort") || lower.contains("timed out")
    }

    fn map_request_error(e: reqwest::Error) -> AppError {
        if e.is_timeout() {
            AppError::Message("Request timeout".to_string())
        } else if e.is_connect() {
            AppError::Message(format!("Connection failed: {e}"))
        } else {
            AppError::Message(e.to_string())
        }
    }

    fn build_agent_probe_request(
        app_type: &AppType,
        provider: &Provider,
        base_url: &str,
        custom_ua: Option<HeaderValue>,
    ) -> Result<Option<AgentProbeRequest>, AppError> {
        match app_type {
            AppType::Claude => Self::build_claude_agent_probe(provider, base_url, custom_ua),
            AppType::Codex => {
                Self::build_codex_agent_probe(provider, base_url, custom_ua).map(Some)
            }
            _ => Ok(None),
        }
    }

    fn build_claude_agent_probe(
        provider: &Provider,
        base_url: &str,
        custom_ua: Option<HeaderValue>,
    ) -> Result<Option<AgentProbeRequest>, AppError> {
        use crate::proxy::providers::{
            claude_api_format_needs_transform, get_claude_api_format,
            transform_claude_request_for_api_format,
        };

        let adapter = ClaudeAdapter::new();
        let auth = adapter.extract_auth(provider).ok_or_else(|| {
            AppError::localized(
                "stream_check.auth_missing",
                "缺少 API Key，无法发送 agent 风格测试请求",
                "API key is missing; cannot send an agent-style test request",
            )
        })?;
        if matches!(
            auth.strategy,
            AuthStrategy::GitHubCopilot | AuthStrategy::CodexOAuth
        ) {
            // 这两类供应商需要命令层的 OAuth 管理器动态换 token；当前连通检测保持原有
            // base_url 可达性语义，避免用占位 token 产生误报。
            return Ok(None);
        }

        let requested_model = Self::resolve_claude_model(provider)?;
        let mut body = json!({
            "model": requested_model,
            "max_tokens": 1,
            "stream": false,
            "metadata": {
                "user_id": format!("cc-switch_session_{STREAM_CHECK_SESSION_ID}")
            },
            "messages": [
                {
                    "role": "user",
                    "content": [
                        { "type": "text", "text": "ping" }
                    ]
                }
            ]
        });

        let api_format = get_claude_api_format(provider);
        let endpoint = if claude_api_format_needs_transform(api_format) {
            body = transform_claude_request_for_api_format(
                body,
                provider,
                api_format,
                Some(STREAM_CHECK_SESSION_ID),
                None,
            )
            .map_err(|e| AppError::Message(format!("Failed to transform Claude probe: {e}")))?;
            match api_format {
                "openai_responses" => "/v1/responses".to_string(),
                "gemini_native" => {
                    let model =
                        crate::proxy::providers::transform_gemini::extract_gemini_model(&body)
                            .map(crate::proxy::gemini_url::normalize_gemini_model_id)
                            .unwrap_or("unknown");
                    format!("/v1beta/models/{model}:generateContent")
                }
                _ => "/v1/chat/completions".to_string(),
            }
        } else {
            "/v1/messages".to_string()
        };

        let model = body
            .get("model")
            .and_then(Value::as_str)
            .map(|model| {
                if api_format == "gemini_native" {
                    crate::proxy::gemini_url::normalize_gemini_model_id(model).to_string()
                } else {
                    model.to_string()
                }
            })
            .unwrap_or_default();

        let url = if api_format == "gemini_native" {
            crate::proxy::gemini_url::resolve_gemini_native_url(
                base_url,
                &endpoint,
                Self::is_full_url(provider),
            )
        } else if Self::is_full_url(provider) {
            base_url.to_string()
        } else {
            adapter.build_url(base_url, &endpoint)
        };
        let mut headers = Self::auth_headers(&adapter, &auth)?;
        headers.insert(ACCEPT, HeaderValue::from_static("application/json"));
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers.insert(ACCEPT_ENCODING, HeaderValue::from_static("identity"));
        if api_format == "anthropic" {
            headers.insert("anthropic-version", HeaderValue::from_static("2023-06-01"));
            headers.insert("anthropic-beta", HeaderValue::from_static(CLAUDE_CODE_BETA));
        }
        headers.insert(
            USER_AGENT,
            custom_ua.unwrap_or_else(|| HeaderValue::from_static(CLAUDE_AGENT_USER_AGENT)),
        );

        Ok(Some(AgentProbeRequest {
            url,
            headers,
            body,
            model,
            probe_kind: "Claude Code",
        }))
    }

    fn build_codex_agent_probe(
        provider: &Provider,
        base_url: &str,
        custom_ua: Option<HeaderValue>,
    ) -> Result<AgentProbeRequest, AppError> {
        use crate::proxy::providers::transform_codex_chat::responses_to_chat_completions_with_reasoning;

        let adapter = crate::proxy::providers::CodexAdapter::new();
        let auth = adapter.extract_auth(provider).ok_or_else(|| {
            AppError::localized(
                "stream_check.auth_missing",
                "缺少 API Key，无法发送 agent 风格测试请求",
                "API key is missing; cannot send an agent-style test request",
            )
        })?;
        let model = Self::resolve_codex_model(provider)?;
        let mut body = json!({
            "model": model,
            "input": [
                {
                    "role": "user",
                    "content": [
                        { "type": "input_text", "text": "ping" }
                    ]
                }
            ],
            "max_output_tokens": 1,
            "stream": false,
            "store": false
        });

        let endpoint = if crate::proxy::providers::should_convert_codex_responses_to_chat(
            provider,
            "/responses",
        ) {
            crate::proxy::providers::apply_codex_chat_upstream_model(provider, &mut body);
            let reasoning =
                crate::proxy::providers::resolve_codex_chat_reasoning_config(provider, &body);
            body = responses_to_chat_completions_with_reasoning(body, reasoning.as_ref())
                .map_err(|e| AppError::Message(format!("Failed to transform Codex probe: {e}")))?;
            "/chat/completions"
        } else {
            "/responses"
        };
        let model = body
            .get("model")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let url = if Self::is_full_url(provider) {
            base_url.to_string()
        } else {
            adapter.build_url(base_url, endpoint)
        };
        let mut headers = Self::auth_headers(&adapter, &auth)?;
        headers.insert(ACCEPT, HeaderValue::from_static("application/json"));
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers.insert(ACCEPT_ENCODING, HeaderValue::from_static("identity"));
        headers.insert(
            USER_AGENT,
            custom_ua.unwrap_or_else(|| HeaderValue::from_static(CODEX_AGENT_USER_AGENT)),
        );

        Ok(AgentProbeRequest {
            url,
            headers,
            body,
            model,
            probe_kind: "Codex",
        })
    }

    async fn probe_agent_request(
        client: &Client,
        request: AgentProbeRequest,
        timeout: std::time::Duration,
    ) -> Result<(AgentProbeRequest, AgentProbeResponse), AppError> {
        let response = client
            .post(&request.url)
            .timeout(timeout)
            .headers(request.headers.clone())
            .json(&request.body)
            .send()
            .await
            .map_err(Self::map_request_error)?;

        let status = response.status().as_u16();
        let body_snippet = response
            .text()
            .await
            .ok()
            .and_then(|body| Self::truncate_response_body(&body));

        Ok((
            request,
            AgentProbeResponse {
                status,
                body_snippet,
            },
        ))
    }

    fn auth_headers(
        adapter: &dyn ProviderAdapter,
        auth: &crate::proxy::providers::AuthInfo,
    ) -> Result<HeaderMap, AppError> {
        let mut headers = HeaderMap::new();
        for (name, value) in adapter
            .get_auth_headers(auth)
            .map_err(|e| AppError::Message(format!("Invalid auth header: {e}")))?
        {
            headers.append(name, value);
        }
        Ok(headers)
    }

    fn is_full_url(provider: &Provider) -> bool {
        provider
            .meta
            .as_ref()
            .and_then(|meta| meta.is_full_url)
            .unwrap_or(false)
    }

    fn truncate_response_body(body: &str) -> Option<String> {
        let compact = body.split_whitespace().collect::<Vec<_>>().join(" ");
        if compact.is_empty() {
            return None;
        }
        const MAX: usize = 280;
        if compact.chars().count() <= MAX {
            Some(compact)
        } else {
            Some(format!(
                "{}…",
                compact.chars().take(MAX).collect::<String>()
            ))
        }
    }

    fn classify_agent_http_failure(status: u16, body: Option<&str>) -> (&'static str, String) {
        let lower = body.unwrap_or_default().to_ascii_lowercase();
        match status {
            400 => (
                "agent_bad_request",
                "invalid request, unsupported parameter, or model mismatch".to_string(),
            ),
            401 => ("agent_auth_failed", "authentication failed".to_string()),
            403 => {
                let reason = if lower.contains("agent")
                    || lower.contains("claude")
                    || lower.contains("codex")
                    || lower.contains("user-agent")
                    || lower.contains("not allowed")
                    || lower.contains("forbidden")
                {
                    "request rejected; possible agent/User-Agent restriction"
                } else {
                    "permission denied"
                };
                ("agent_forbidden", reason.to_string())
            }
            404 => (
                "agent_not_found",
                "endpoint or model was not found".to_string(),
            ),
            408 | 409 | 425 | 429 => (
                "agent_rate_limited",
                "rate limited or temporarily unavailable".to_string(),
            ),
            500..=599 => (
                "agent_upstream_error",
                "upstream returned a server error".to_string(),
            ),
            _ => ("agent_http_error", format!("HTTP {status}")),
        }
    }

    fn format_agent_http_failure(
        probe_kind: &str,
        status: u16,
        reason: &str,
        body: Option<&str>,
    ) -> String {
        match body {
            Some(body) => format!(
                "Reachable, but {probe_kind} agent probe failed with HTTP {status}: {reason}. Response: {body}"
            ),
            None => format!(
                "Reachable, but {probe_kind} agent probe failed with HTTP {status}: {reason}"
            ),
        }
    }

    /// Provider 级自定义 User-Agent（`meta.customUserAgent`），与转发路径共用单一口径：
    /// trim、空串视为未设置、非法值静默忽略（返回 `None`）。
    fn custom_user_agent(provider: &Provider) -> Option<HeaderValue> {
        provider
            .meta
            .as_ref()
            .and_then(|meta| meta.custom_user_agent_header().ok().flatten())
    }

    fn resolve_claude_model(provider: &Provider) -> Result<String, AppError> {
        let candidate = provider
            .settings_config
            .get("env")
            .and_then(|env| {
                [
                    "ANTHROPIC_MODEL",
                    "ANTHROPIC_DEFAULT_SONNET_MODEL",
                    "ANTHROPIC_DEFAULT_OPUS_MODEL",
                    "ANTHROPIC_DEFAULT_HAIKU_MODEL",
                ]
                .into_iter()
                .find_map(|key| env.get(key).and_then(Value::as_str))
            })
            .or_else(|| {
                provider
                    .settings_config
                    .get("model")
                    .and_then(Value::as_str)
            });

        Ok(candidate
            .and_then(Self::normalize_model_candidate)
            .unwrap_or_else(|| DEFAULT_CLAUDE_AGENT_PROBE_MODEL.to_string()))
    }

    fn resolve_codex_model(provider: &Provider) -> Result<String, AppError> {
        Ok(crate::proxy::providers::codex_provider_upstream_model(provider)
            .and_then(|model| Self::normalize_model_candidate(&model))
            .unwrap_or_else(|| DEFAULT_CODEX_AGENT_PROBE_MODEL.to_string()))
    }

    fn normalize_model_candidate(value: &str) -> Option<String> {
        let stripped = crate::proxy::model_mapper::strip_one_m_suffix_for_upstream(value);
        let trimmed = stripped.trim();
        (!trimmed.is_empty()).then(|| trimmed.to_string())
    }

    // ===== 各应用 base_url 提取（settings_config 结构互不相同）=====

    /// OpenClaw: `{ baseUrl, apiKey, api, ... }`（camelCase）
    fn extract_openclaw_base_url(provider: &Provider) -> Result<String, AppError> {
        provider
            .settings_config
            .get("baseUrl")
            .and_then(|v| v.as_str())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                AppError::localized(
                    "openclaw_base_url_missing",
                    "OpenClaw 供应商缺少 baseUrl",
                    "OpenClaw provider is missing `baseUrl`",
                )
            })
    }

    /// Hermes: `{ base_url, api_key, api_mode }`（snake_case）
    fn extract_hermes_base_url(provider: &Provider) -> Result<String, AppError> {
        provider
            .settings_config
            .get("base_url")
            .and_then(|v| v.as_str())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                AppError::localized(
                    "hermes_base_url_missing",
                    "Hermes 供应商缺少 base_url",
                    "Hermes provider is missing `base_url`",
                )
            })
    }

    /// OpenCode: `{ npm, options: { baseURL, apiKey }, ... }`
    ///
    /// 用户未显式填 `options.baseURL` 时，按 `npm`（AI SDK 包）回退到包自带默认端点。
    /// `@ai-sdk/openai-compatible` 无默认端点，必须显式填。
    fn resolve_opencode_base_url(
        provider: &Provider,
        npm: Option<&str>,
    ) -> Result<String, AppError> {
        if let Some(explicit) = Self::extract_opencode_base_url(provider) {
            return Ok(explicit);
        }

        let fallback = match npm {
            Some("@ai-sdk/openai") => Some("https://api.openai.com/v1"),
            Some("@ai-sdk/anthropic") => Some("https://api.anthropic.com"),
            Some("@ai-sdk/google") => Some("https://generativelanguage.googleapis.com"),
            _ => None,
        };

        fallback.map(|s| s.to_string()).ok_or_else(|| {
            AppError::localized(
                "opencode_base_url_missing",
                "OpenCode 供应商缺少 options.baseURL，且当前 SDK 包没有默认端点",
                "OpenCode provider is missing `options.baseURL` and the SDK package has no default endpoint",
            )
        })
    }

    fn extract_opencode_base_url(provider: &Provider) -> Option<String> {
        provider
            .settings_config
            .get("options")
            .and_then(|v| v.get("baseURL"))
            .and_then(|v| v.as_str())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    }

    fn extract_opencode_npm(provider: &Provider) -> Option<String> {
        provider
            .settings_config
            .get("npm")
            .and_then(|v| v.as_str())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_provider(settings_config: serde_json::Value) -> Provider {
        Provider::with_id(
            "test".to_string(),
            "Test".to_string(),
            settings_config,
            None,
        )
    }

    #[test]
    fn test_default_config_uses_reachability_friendly_values() {
        let config = StreamCheckConfig::default();
        assert_eq!(config.timeout_secs, 8);
        assert_eq!(config.max_retries, 1);
        // 降级阈值沿用旧尺度，避免把 1 秒多的正常延迟误判为"较慢"
        assert_eq!(config.degraded_threshold_ms, 6000);
    }

    #[test]
    fn test_determine_status() {
        assert_eq!(
            StreamCheckService::determine_status(1000, 1500),
            HealthStatus::Operational
        );
        assert_eq!(
            StreamCheckService::determine_status(1500, 1500),
            HealthStatus::Operational
        );
        assert_eq!(
            StreamCheckService::determine_status(1501, 1500),
            HealthStatus::Degraded
        );
    }

    #[test]
    fn test_should_retry_only_on_timeout_like_errors() {
        assert!(StreamCheckService::should_retry("Request timeout"));
        assert!(StreamCheckService::should_retry("request timed out"));
        assert!(StreamCheckService::should_retry("connection abort"));
        // 连接被拒 / DNS 失败不重试
        assert!(!StreamCheckService::should_retry(
            "Connection failed: dns error"
        ));
        assert!(!StreamCheckService::should_retry("Reachable"));
    }

    #[test]
    fn test_build_result_any_http_status_is_reachable() {
        // 任何 HTTP 状态码都算可达（success=true）
        for status in [200u16, 401, 403, 404, 429, 500, 503] {
            let r = StreamCheckService::build_result(Ok(status), 100, 1500);
            assert!(r.success, "status {status} should be reachable");
            assert_eq!(r.status, HealthStatus::Operational);
            assert_eq!(r.http_status, Some(status));
            assert!(r.model_used.is_empty());
            assert!(r.error_category.is_none());
        }
    }

    #[test]
    fn test_build_result_network_error_is_unreachable() {
        let r = StreamCheckService::build_result(
            Err(AppError::Message("Connection failed: refused".to_string())),
            5,
            1500,
        );
        assert!(!r.success);
        assert_eq!(r.status, HealthStatus::Failed);
        assert!(r.http_status.is_none());
    }

    #[test]
    fn test_build_result_slow_response_is_degraded() {
        let r = StreamCheckService::build_result(Ok(200), 3000, 1500);
        assert!(r.success);
        assert_eq!(r.status, HealthStatus::Degraded);
    }

    #[test]
    fn test_resolve_claude_model_from_provider_config() {
        let p = make_provider(serde_json::json!({
            "env": {
                "ANTHROPIC_DEFAULT_SONNET_MODEL": "kimi-k2[1M]"
            }
        }));
        assert_eq!(
            StreamCheckService::resolve_claude_model(&p).unwrap(),
            "kimi-k2"
        );
    }

    #[test]
    fn test_resolve_codex_model_from_toml() {
        let p = make_provider(serde_json::json!({
            "config": "model_provider = \"custom\"\nmodel = \"deepseek-v4-pro\"\n[model_providers.custom]\nbase_url = \"https://example.com/v1\"\n"
        }));
        assert_eq!(
            StreamCheckService::resolve_codex_model(&p).unwrap(),
            "deepseek-v4-pro"
        );
    }

    #[test]
    fn test_resolve_claude_model_defaults_to_agent_model() {
        let p = make_provider(serde_json::json!({ "env": {} }));
        assert_eq!(
            StreamCheckService::resolve_claude_model(&p).unwrap(),
            "claude-opus-4-8"
        );
    }

    #[test]
    fn test_resolve_codex_model_defaults_to_agent_model() {
        let p = make_provider(serde_json::json!({}));
        assert_eq!(
            StreamCheckService::resolve_codex_model(&p).unwrap(),
            "gpt-5.5"
        );
    }

    #[test]
    fn test_classify_agent_forbidden_mentions_agent_restriction() {
        let (category, reason) = StreamCheckService::classify_agent_http_failure(
            403,
            Some("This endpoint is only allowed for Claude Code agents"),
        );
        assert_eq!(category, "agent_forbidden");
        assert!(reason.contains("agent"));
    }

    #[test]
    fn test_merge_provider_config_override_and_default() {
        use crate::provider::{ProviderMeta, ProviderTestConfig};

        let global = StreamCheckConfig::default();

        // 无 testConfig → 用全局
        let p = make_provider(serde_json::json!({}));
        let merged = StreamCheckService::merge_provider_config(&p, &global);
        assert_eq!(merged.timeout_secs, global.timeout_secs);

        // testConfig 启用并覆盖部分字段
        let mut p2 = make_provider(serde_json::json!({}));
        p2.meta = Some(ProviderMeta {
            test_config: Some(ProviderTestConfig {
                enabled: true,
                timeout_secs: Some(20),
                degraded_threshold_ms: Some(3000),
                max_retries: None,
            }),
            ..Default::default()
        });
        let merged2 = StreamCheckService::merge_provider_config(&p2, &global);
        assert_eq!(merged2.timeout_secs, 20);
        assert_eq!(merged2.degraded_threshold_ms, 3000);
        assert_eq!(merged2.max_retries, global.max_retries); // 未覆盖 → 全局

        // testConfig 存在但未启用 → 忽略，用全局
        let mut p3 = make_provider(serde_json::json!({}));
        p3.meta = Some(ProviderMeta {
            test_config: Some(ProviderTestConfig {
                enabled: false,
                timeout_secs: Some(99),
                degraded_threshold_ms: None,
                max_retries: None,
            }),
            ..Default::default()
        });
        let merged3 = StreamCheckService::merge_provider_config(&p3, &global);
        assert_eq!(merged3.timeout_secs, global.timeout_secs);
    }

    #[test]
    fn test_resolve_opencode_base_url_explicit_wins() {
        let p = make_provider(serde_json::json!({
            "npm": "@ai-sdk/openai",
            "options": { "baseURL": "https://proxy.local/v1", "apiKey": "k" },
            "models": {},
        }));
        let resolved =
            StreamCheckService::resolve_opencode_base_url(&p, Some("@ai-sdk/openai")).unwrap();
        assert_eq!(resolved, "https://proxy.local/v1");
    }

    #[test]
    fn test_resolve_opencode_base_url_falls_back_for_known_npm() {
        let p = make_provider(serde_json::json!({
            "npm": "@ai-sdk/anthropic",
            "options": { "apiKey": "k" },
            "models": {},
        }));
        let resolved =
            StreamCheckService::resolve_opencode_base_url(&p, Some("@ai-sdk/anthropic")).unwrap();
        assert_eq!(resolved, "https://api.anthropic.com");
    }

    #[test]
    fn test_resolve_opencode_base_url_errors_for_openai_compatible_without_url() {
        let p = make_provider(serde_json::json!({
            "npm": "@ai-sdk/openai-compatible",
            "options": { "apiKey": "k" },
            "models": {},
        }));
        let result =
            StreamCheckService::resolve_opencode_base_url(&p, Some("@ai-sdk/openai-compatible"));
        assert!(result.is_err());
    }

    #[test]
    fn test_extract_openclaw_base_url_missing_errors() {
        let p = make_provider(serde_json::json!({ "apiKey": "k", "api": "openai-completions" }));
        assert!(StreamCheckService::extract_openclaw_base_url(&p).is_err());

        let p2 = make_provider(serde_json::json!({ "baseUrl": "https://api.deepseek.com/v1" }));
        assert_eq!(
            StreamCheckService::extract_openclaw_base_url(&p2).unwrap(),
            "https://api.deepseek.com/v1"
        );
    }

    #[test]
    fn test_resolve_base_url_uses_explicit_url_or_errors_when_missing() {
        // 有显式 base_url → 直接用
        let p = make_provider(
            serde_json::json!({ "env": { "ANTHROPIC_BASE_URL": "https://relay.example/v1" } }),
        );
        assert_eq!(
            StreamCheckService::resolve_base_url(&AppType::Claude, &p).unwrap(),
            "https://relay.example/v1"
        );

        // 缺 base_url（官方留空 / 用户忘填）→ 报错。官方供应商的检测按钮在前端已隐藏，
        // 不会走到这里；不做官方端点回退（避免给忘填地址的第三方误显绿灯）。
        let empty = make_provider(serde_json::json!({ "env": {} }));
        assert!(StreamCheckService::resolve_base_url(&AppType::Claude, &empty).is_err());
    }
}
