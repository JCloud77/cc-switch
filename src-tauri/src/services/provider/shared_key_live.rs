//! 中央共享 Key 池 → live 有效配置的单点物化。
//!
//! 官方 v3.20.x 把 Codex 切换改为 config-only 写入，并新增了直接读
//! `settings_config.auth` / TOML bearer token 的写入安全闸。这些新链路不认识本地的
//! 中央共享 Key 池（池只保存在 `shared_api_keys` / `provider_shared_key_links`，
//! 卡片自身配置里可能没有 Key），于是「池里选了 Key 但卡片配置未同步」的供应商会被
//! 闸门拒绝或写入错误凭据。
//!
//! 单点修法：在 `build_effective_settings_with_common_config` 里把池中当前选中的 Key
//! 物化进**内存** effective settings，让所有下游链路（普通 live 写入、Codex 写入预检、
//! 切换事务、Claude 接管同步、接管备份重建）都能看见当前生效凭据。**数据库不动**，
//! 池仍然是唯一真相，卡片配置不会被固化。

use crate::error::AppError;
use crate::provider::Provider;
use crate::AppType;
use serde_json::Value;

/// Claude settings 的两个互斥认证字段名。
const CLAUDE_AUTH_TOKEN_FIELD: &str = "ANTHROPIC_AUTH_TOKEN";
const CLAUDE_API_KEY_FIELD: &str = "ANTHROPIC_API_KEY";

/// 把中央池当前选中的 Key 物化进内存 effective settings。
///
/// 无池关联、无选中 Key 或 Key 不合法时不做任何改动，保留调用点原有的取 Key 回退
/// 语义（例如池里没有条目时仍用配置里已有的凭据）。
///
/// 本轮保持既有优先级差异（见 `.pi/PLAN.md` D3，属于升级前就存在的本地行为，不在
/// 本轮重构）：
/// - Codex：代理注入 OAuth（xAI / GitHub Copilot）与官方 OAuth 卡跳过池 Key，保留上游
///   的 live 凭据管理；
/// - Claude：保留「选中的手工 Key 优先」行为。
pub(crate) fn materialize_selected_shared_key(
    app_type: &AppType,
    provider: &Provider,
    settings: &mut Value,
) -> Result<(), AppError> {
    if !matches!(app_type, AppType::Claude | AppType::Codex) {
        return Ok(());
    }

    if matches!(app_type, AppType::Codex)
        && (provider.uses_proxy_injected_oauth()
            || provider.is_codex_oauth()
            || crate::proxy::providers::is_codex_official_provider(provider))
    {
        return Ok(());
    }

    let Some((key, strategy)) = provider
        .meta
        .as_ref()
        .and_then(|meta| meta.resolve_selected_key())
    else {
        return Ok(());
    };

    match app_type {
        AppType::Codex => materialize_codex_key(settings, key),
        AppType::Claude => {
            let field = claude_auth_field(provider, strategy);
            materialize_claude_key(settings, key, field)
        }
        _ => Ok(()),
    }
}

/// Codex：写 `auth.OPENAI_API_KEY`（`extract_codex_api_key` 先读 auth，再回退 TOML
/// bearer token；物化保证新闸看到池中凭据）。缺 `auth` 时创建对象，`auth` 存在但不是
/// 对象时返回明确错误而不是覆盖整个配置。
fn materialize_codex_key(settings: &mut Value, key: &str) -> Result<(), AppError> {
    let Some(object) = settings.as_object_mut() else {
        return Err(AppError::Config(
            "Codex 供应商配置必须是 JSON 对象，无法写入共享 Key".to_string(),
        ));
    };
    match object.get_mut("auth") {
        Some(Value::Object(auth)) => {
            auth.insert("OPENAI_API_KEY".to_string(), Value::String(key.to_string()));
            Ok(())
        }
        Some(_) => Err(AppError::Config(
            "Codex 供应商配置的 'auth' 字段不是 JSON 对象，无法写入共享 Key".to_string(),
        )),
        None => {
            object.insert(
                "auth".to_string(),
                serde_json::json!({ "OPENAI_API_KEY": key }),
            );
            Ok(())
        }
    }
}

/// Claude：按池条目 strategy 决定落点，并移除另一个字段以避免网关同时收到两种认证头。
fn materialize_claude_key(settings: &mut Value, key: &str, field: &str) -> Result<(), AppError> {
    let Some(object) = settings.as_object_mut() else {
        return Err(AppError::Config(
            "Claude 供应商配置必须是 JSON 对象，无法写入共享 Key".to_string(),
        ));
    };
    match object.get_mut("env") {
        Some(Value::Object(env)) => {
            env.insert(field.to_string(), Value::String(key.to_string()));
            let other = if field == CLAUDE_AUTH_TOKEN_FIELD {
                CLAUDE_API_KEY_FIELD
            } else {
                CLAUDE_AUTH_TOKEN_FIELD
            };
            env.remove(other);
            Ok(())
        }
        Some(_) => Err(AppError::Config(
            "Claude 供应商配置的 'env' 字段不是 JSON 对象，无法写入共享 Key".to_string(),
        )),
        None => {
            object.insert("env".to_string(), serde_json::json!({ field: key }));
            Ok(())
        }
    }
}

/// 池条目 strategy → Claude 认证字段；没有 strategy 时回退到 `meta.apiKeyField`。
///
/// 与 `shared_keys::shared_key_strategy` 的取值一一对应：`claude_auth` → Bearer
/// （`ANTHROPIC_AUTH_TOKEN`），其余（含从未出现的未知值）按 x-api-key 处理。
fn claude_auth_field(provider: &Provider, strategy: Option<&str>) -> &'static str {
    match strategy {
        Some("claude_auth") => CLAUDE_AUTH_TOKEN_FIELD,
        Some(_) => CLAUDE_API_KEY_FIELD,
        None => {
            let explicit = provider
                .meta
                .as_ref()
                .and_then(|meta| meta.api_key_field.as_deref())
                .unwrap_or(CLAUDE_AUTH_TOKEN_FIELD);
            if explicit.eq_ignore_ascii_case(CLAUDE_API_KEY_FIELD) {
                CLAUDE_API_KEY_FIELD
            } else {
                CLAUDE_AUTH_TOKEN_FIELD
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::{ApiKeyEntry, ProviderMeta};
    use serde_json::json;

    fn key_entry(id: &str, value: &str) -> ApiKeyEntry {
        ApiKeyEntry {
            id: id.to_string(),
            label: String::new(),
            key: value.to_string(),
            strategy: None,
        }
    }

    fn codex_provider(settings_config: Value, selected: Option<&str>) -> Provider {
        let mut provider = Provider::with_id(
            "p-codex".to_string(),
            "Vendor".to_string(),
            settings_config,
            None,
        );
        provider.meta = Some(ProviderMeta {
            api_keys: vec![key_entry("pool-key", "sk-pool")],
            selected_key_id: selected.map(ToString::to_string),
            ..ProviderMeta::default()
        });
        provider
    }

    fn claude_provider(settings_config: Value, strategy: &str) -> Provider {
        let mut provider = Provider::with_id(
            "p-claude".to_string(),
            "Vendor".to_string(),
            settings_config,
            None,
        );
        provider.meta = Some(ProviderMeta {
            api_keys: vec![ApiKeyEntry {
                id: "pool-key".to_string(),
                label: String::new(),
                key: "sk-pool".to_string(),
                strategy: Some(strategy.to_string()),
            }],
            selected_key_id: Some("pool-key".to_string()),
            ..ProviderMeta::default()
        });
        provider
    }

    #[test]
    fn materialize_shared_keys_writes_selected_pool_key_into_codex_auth() -> Result<(), AppError> {
        let provider = codex_provider(
            json!({"auth": {"OPENAI_API_KEY": "sk-old"}, "config": ""}),
            Some("pool-key"),
        );
        let mut settings = provider.settings_config.clone();
        materialize_selected_shared_key(&AppType::Codex, &provider, &mut settings)?;
        assert_eq!(settings["auth"]["OPENAI_API_KEY"], json!("sk-pool"));
        Ok(())
    }

    #[test]
    fn materialize_shared_keys_creates_codex_auth_when_missing() -> Result<(), AppError> {
        let provider = codex_provider(json!({"config": ""}), Some("pool-key"));
        let mut settings = provider.settings_config.clone();
        materialize_selected_shared_key(&AppType::Codex, &provider, &mut settings)?;
        assert_eq!(settings["auth"]["OPENAI_API_KEY"], json!("sk-pool"));
        Ok(())
    }

    #[test]
    fn materialize_shared_keys_keeps_claude_fields_mutually_exclusive() -> Result<(), AppError> {
        let provider = claude_provider(
            json!({"env": {"ANTHROPIC_AUTH_TOKEN": "sk-old", "ANTHROPIC_BASE_URL": "https://x"}}),
            "anthropic",
        );
        let mut settings = provider.settings_config.clone();
        materialize_selected_shared_key(&AppType::Claude, &provider, &mut settings)?;
        assert_eq!(settings["env"]["ANTHROPIC_API_KEY"], json!("sk-pool"));
        assert!(settings["env"].get("ANTHROPIC_AUTH_TOKEN").is_none());
        assert_eq!(settings["env"]["ANTHROPIC_BASE_URL"], json!("https://x"));
        Ok(())
    }

    #[test]
    fn materialize_shared_keys_uses_auth_token_for_claude_auth_strategy() -> Result<(), AppError> {
        let provider = claude_provider(
            json!({"env": {"ANTHROPIC_API_KEY": "sk-old"}}),
            "claude_auth",
        );
        let mut settings = provider.settings_config.clone();
        materialize_selected_shared_key(&AppType::Claude, &provider, &mut settings)?;
        assert_eq!(settings["env"]["ANTHROPIC_AUTH_TOKEN"], json!("sk-pool"));
        assert!(settings["env"].get("ANTHROPIC_API_KEY").is_none());
        Ok(())
    }

    #[test]
    fn materialize_shared_keys_skips_providers_without_selection() -> Result<(), AppError> {
        let provider = codex_provider(json!({"auth": {"OPENAI_API_KEY": "sk-old"}}), None);
        let mut settings = provider.settings_config.clone();
        materialize_selected_shared_key(&AppType::Codex, &provider, &mut settings)?;
        assert_eq!(settings["auth"]["OPENAI_API_KEY"], json!("sk-old"));
        Ok(())
    }

    #[test]
    fn materialize_shared_keys_ignores_unrelated_apps() -> Result<(), AppError> {
        let provider = codex_provider(
            json!({"auth": {"OPENAI_API_KEY": "sk-old"}}),
            Some("pool-key"),
        );
        let mut settings = provider.settings_config.clone();
        materialize_selected_shared_key(&AppType::Gemini, &provider, &mut settings)?;
        assert_eq!(settings["auth"]["OPENAI_API_KEY"], json!("sk-old"));
        Ok(())
    }

    #[test]
    fn materialize_shared_keys_skips_oauth_codex_providers() -> Result<(), AppError> {
        // xAI OAuth 卡由代理按请求注入真实 token，不能被池里的手工 Key 顶替。
        let mut provider = codex_provider(json!({"auth": {}}), Some("pool-key"));
        provider.meta = Some(ProviderMeta {
            provider_type: Some("xai_oauth".to_string()),
            api_keys: vec![key_entry("pool-key", "sk-pool")],
            selected_key_id: Some("pool-key".to_string()),
            ..ProviderMeta::default()
        });
        let mut settings = provider.settings_config.clone();
        materialize_selected_shared_key(&AppType::Codex, &provider, &mut settings)?;
        assert!(settings["auth"].get("OPENAI_API_KEY").is_none());
        Ok(())
    }

    #[test]
    fn materialize_shared_keys_reports_non_object_settings() {
        let provider = codex_provider(json!("not-an-object"), Some("pool-key"));
        let mut settings = provider.settings_config.clone();
        let error = materialize_selected_shared_key(&AppType::Codex, &provider, &mut settings)
            .expect_err("非对象配置必须报错");
        assert!(error.to_string().contains("JSON 对象"));
    }

    /// Codex 侧的四类「凭据由上游/代理托管」卡片：池 Key 不得顶替它们的凭据管理。
    #[test]
    fn materialize_shared_keys_skips_managed_codex_cards() {
        // codex_oauth（官方登录态本身就是凭据）
        let mut codex_oauth = codex_provider(json!({"auth": {}}), Some("pool-key"));
        codex_oauth.meta = Some(ProviderMeta {
            provider_type: Some("codex_oauth".to_string()),
            api_keys: vec![key_entry("pool-key", "sk-pool")],
            selected_key_id: Some("pool-key".to_string()),
            ..ProviderMeta::default()
        });

        // github_copilot（代理按请求注入 token）
        let mut copilot = codex_provider(json!({"auth": {}}), Some("pool-key"));
        copilot.meta = Some(ProviderMeta {
            provider_type: Some("github_copilot".to_string()),
            api_keys: vec![key_entry("pool-key", "sk-pool")],
            selected_key_id: Some("pool-key".to_string()),
            ..ProviderMeta::default()
        });

        // 官方 Codex 卡（id + category=official）
        let mut official = Provider::with_id(
            crate::database::CODEX_OFFICIAL_PROVIDER_ID.to_string(),
            "ChatGPT".to_string(),
            json!({"auth": {}, "config": "model_provider = \"openai\"\n"}),
            None,
        );
        official.category = Some("official".to_string());
        official.meta = Some(ProviderMeta {
            api_keys: vec![key_entry("pool-key", "sk-pool")],
            selected_key_id: Some("pool-key".to_string()),
            ..ProviderMeta::default()
        });

        for (label, provider) in [
            ("codex_oauth", codex_oauth),
            ("github_copilot", copilot),
            ("official", official),
        ] {
            let mut settings = provider.settings_config.clone();
            materialize_selected_shared_key(&AppType::Codex, &provider, &mut settings)
                .unwrap_or_else(|e| panic!("{label} 不应报错: {e}"));
            let auth = settings.get("auth").and_then(Value::as_object);
            let injected = auth
                .and_then(|obj| obj.get("OPENAI_API_KEY"))
                .and_then(Value::as_str)
                .unwrap_or_default();
            assert_ne!(
                injected, "sk-pool",
                "{label} 卡不得被池 Key 顶替（其凭据由上游或代理管理）"
            );
        }
    }

    /// 本轮保持现状的行为：Claude 侧的 Copilot/xAI 卡依旧让选中的手工 Key 优先。
    ///
    /// 这是升级前就存在的本地不一致（见 `.pi/PLAN.md` D3），只记录不重构；如果将来
    /// 统一为「OAuth 卡忽略手工 Key」，本测试是必须先改的那一处。
    #[test]
    fn materialize_shared_keys_keeps_claude_manual_key_priority() {
        let mut provider = claude_provider(
            json!({"env": {"ANTHROPIC_BASE_URL": "https://copilot.example"}}),
            "anthropic",
        );
        provider.meta = Some(ProviderMeta {
            provider_type: Some("github_copilot".to_string()),
            api_keys: vec![ApiKeyEntry {
                id: "pool-key".to_string(),
                label: String::new(),
                key: "sk-pool".to_string(),
                strategy: Some("anthropic".to_string()),
            }],
            selected_key_id: Some("pool-key".to_string()),
            ..ProviderMeta::default()
        });
        let mut settings = provider.settings_config.clone();
        materialize_selected_shared_key(&AppType::Claude, &provider, &mut settings)
            .expect("claude provider");
        assert_eq!(
            settings["env"]["ANTHROPIC_API_KEY"],
            json!("sk-pool"),
            "Claude 侧保留选中手工 Key 优先（现状记录）"
        );
    }

    // -----------------------------------------------------------------------
    // 与真实数据库的衔接：hydrate + 物化（进入 CI 的 `--lib shared_keys` 过滤）
    // -----------------------------------------------------------------------

    use crate::database::Database;
    use crate::services::provider::build_effective_settings_with_common_config;

    /// 建库并写入一个「池里已有 Key、卡片配置里没有」的供应商。
    ///
    /// 返回读回的（已归一化的）provider id，供调用方按需重新构造请求对象。
    fn seed_pool_provider(db: &Database, app_type: &str, config: Value) -> (String, String) {
        let mut provider =
            Provider::with_id("pooled".to_string(), "Pooled".to_string(), config, None);
        provider.meta = Some(ProviderMeta {
            api_keys: vec![ApiKeyEntry {
                id: "choice".to_string(),
                label: "池内 Key".to_string(),
                key: "sk-from-pool".to_string(),
                strategy: None,
            }],
            selected_key_id: Some("choice".to_string()),
            ..ProviderMeta::default()
        });
        db.save_provider(app_type, &provider)
            .expect("save provider with pool");
        let persisted = db
            .get_provider_by_id("pooled", app_type)
            .expect("read provider")
            .expect("provider exists");
        let selected = persisted
            .meta
            .as_ref()
            .and_then(|meta| meta.selected_key_id.clone())
            .expect("selected key id after normalization");
        (persisted.id, selected)
    }

    /// 直读 `providers.meta` 原始值，绕开 DAO 的池 hydrate，用于判断「是否写回数据库」。
    fn raw_provider_meta(
        db: &Database,
        provider_id: &str,
        app_type: &str,
    ) -> Result<String, AppError> {
        let conn = crate::database::lock_conn!(db.conn);
        conn.query_row(
            "SELECT meta FROM providers WHERE id = ?1 AND app_type = ?2",
            rusqlite::params![provider_id, app_type],
            |row| row.get(0),
        )
        .map_err(|e| AppError::Database(e.to_string()))
    }

    /// 模拟 provider add/update 的请求对象：只有 `selectedKeyId`，没有 `apiKeys`。
    fn request_provider(id: &str, config: Value, selected: &str) -> Provider {
        let mut provider = Provider::with_id(id.to_string(), "Pooled".to_string(), config, None);
        provider.meta = Some(ProviderMeta {
            selected_key_id: Some(selected.to_string()),
            ..ProviderMeta::default()
        });
        provider
    }

    #[test]
    fn materialize_shared_keys_hydrates_pool_for_request_provider() -> Result<(), AppError> {
        let db = Database::memory()?;
        let (id, selected) = seed_pool_provider(
            &db,
            "claude",
            serde_json::json!({"env": {"ANTHROPIC_BASE_URL": "https://pool.example"}}),
        );
        // 请求对象不含 apiKeys：只有 hydrate 之后才能解析出选中的池 Key。
        let request = request_provider(
            &id,
            serde_json::json!({"env": {"ANTHROPIC_BASE_URL": "https://pool.example"}}),
            &selected,
        );

        let effective =
            build_effective_settings_with_common_config(&db, &AppType::Claude, &request)?;

        assert_eq!(
            effective["env"]["ANTHROPIC_API_KEY"],
            serde_json::json!("sk-from-pool"),
            concat!(
                "池中选中的 Key 必须物化进内存 effective settings",
                "（配置无 ANTHROPIC_AUTH_TOKEN，hydrate 推导为 anthropic 策略，",
                "落点 ANTHROPIC_API_KEY）"
            )
        );
        assert_eq!(
            effective["env"]["ANTHROPIC_BASE_URL"],
            serde_json::json!("https://pool.example")
        );
        // 数据库不动：provider meta 不得被写回 Key 列表。
        let stored = db
            .get_provider_by_id(&id, "claude")?
            .expect("provider still there");
        // hydrate 会按池填充内存视图（这是它的职责），所以只能查数据库原始行来判断
        // 「物化是否写回」：providers.meta 既不存 Key 列表，也不存池 Key 值。
        let raw_meta = raw_provider_meta(&db, "pooled", "claude")?;
        assert!(
            !raw_meta.contains("apiKeys") && !raw_meta.contains("sk-from-pool"),
            "物化只作用于内存，不得写回数据库: {raw_meta}"
        );
        Ok(())
    }

    #[test]
    fn materialize_shared_keys_reaches_codex_preflight_with_pool_key() -> Result<(), AppError> {
        let db = Database::memory()?;
        let config = "[model_providers.custom]\nname = \"custom\"\nbase_url = \"https://pool.example/v1\"\nwire_api = \"responses\"\n";
        let (id, selected) = seed_pool_provider(
            &db,
            "codex",
            serde_json::json!({"auth": {}, "config": config, "category": "custom"}),
        );
        let request = request_provider(
            &id,
            serde_json::json!({"auth": {}, "config": config}),
            &selected,
        );

        let effective =
            build_effective_settings_with_common_config(&db, &AppType::Codex, &request)?;
        let auth = effective.get("auth").expect("auth present");
        let config_text = effective.get("config").and_then(Value::as_str);
        assert_eq!(auth["OPENAI_API_KEY"], serde_json::json!("sk-from-pool"));

        // 官方写入预检闸必须放行，且配置投影里的 bearer token 就是池中的 Key。
        crate::codex_config::preflight_codex_live_write(Some("custom"), auth, config_text)
            .expect("池中有 Key 的第三方配置必须通过写入预检");

        let projected_text = crate::codex_config::prepare_codex_provider_live_config(
            auth,
            config_text.unwrap_or_default(),
        )
        .expect("config projection");
        assert!(
            projected_text.contains("sk-from-pool"),
            "config.toml 投影必须使用池中 Key: {projected_text}"
        );
        Ok(())
    }

    #[test]
    fn materialize_shared_keys_never_fabricates_codex_credentials() -> Result<(), AppError> {
        // 无池关联、无 Key 的空配置：上游对空 config 明确放行（见 plan_codex_live_write
        // 的 `other =>` 分支），本轮的物化不得凭空塞进任何凭据改变这个语义。
        let db = Database::memory()?;
        let provider = Provider::with_id(
            "bare".to_string(),
            "Bare".to_string(),
            serde_json::json!({"auth": {}, "config": ""}),
            None,
        );
        let effective =
            build_effective_settings_with_common_config(&db, &AppType::Codex, &provider)?;
        let auth = effective.get("auth").expect("auth present");
        let key = auth.get("OPENAI_API_KEY").and_then(Value::as_str);
        assert!(
            key.map(str::trim).unwrap_or_default().is_empty(),
            "无池无 Key 时不得凭空物化凭据: {auth}"
        );
        let config_text = effective.get("config").and_then(Value::as_str);
        assert!(
            !config_text
                .unwrap_or_default()
                .contains("experimental_bearer_token"),
            "空配置不得被物化出 bearer token: {config_text:?}"
        );

        // 反向锚点：此时若配置要求回退官方登录凭据，闸门必须拒绝。
        let fallback = "model_provider = \"custom\"\n\n[model_providers.custom]\nname = \"custom\"\nbase_url = \"https://third.example/v1\"\nwire_api = \"responses\"\nrequires_openai_auth = true\n";
        crate::codex_config::preflight_codex_live_write(Some("custom"), auth, Some(fallback))
            .expect_err("没有可用 Key 的第三方配置必须被写入预检拒绝");
        Ok(())
    }

    #[test]
    fn materialize_shared_keys_uses_pool_strategy_for_claude_fields() -> Result<(), AppError> {
        // hydrate 会按「当前配置」重新推导 strategy（见 shared_keys.rs 的
        // shared_key_strategy），所以两条落点必须用配置形态来区分，而不是手写 strategy。
        let db = Database::memory()?;

        // 场景 A：配置带非空 ANTHROPIC_AUTH_TOKEN → claude_auth 策略 → 落 TOKEN 并清 API_KEY。
        let mut bearer_card = Provider::with_id(
            "claude-token".to_string(),
            "TokenCard".to_string(),
            serde_json::json!({"env": {
                "ANTHROPIC_AUTH_TOKEN": "sk-stale-token",
                "ANTHROPIC_API_KEY": "sk-stale-key"
            }}),
            None,
        );
        bearer_card.meta = Some(ProviderMeta {
            api_keys: vec![ApiKeyEntry {
                id: "t1".to_string(),
                label: String::new(),
                key: "sk-token".to_string(),
                strategy: None,
            }],
            selected_key_id: Some("t1".to_string()),
            ..ProviderMeta::default()
        });
        db.save_provider("claude", &bearer_card)?;
        let stored = db
            .get_provider_by_id("claude-token", "claude")?
            .expect("provider");
        let effective =
            build_effective_settings_with_common_config(&db, &AppType::Claude, &stored)?;
        assert_eq!(
            effective["env"]["ANTHROPIC_AUTH_TOKEN"],
            serde_json::json!("sk-token"),
            "Bearer 类供应商必须把池 Key 写进 ANTHROPIC_AUTH_TOKEN"
        );
        assert!(
            effective["env"].get("ANTHROPIC_API_KEY").is_none(),
            "两种认证字段必须互斥"
        );

        // 场景 B：meta.apiKeyField 显式要求 ANTHROPIC_API_KEY → anthropic 策略 → 落 API_KEY。
        let mut api_key_card = Provider::with_id(
            "claude-api-key".to_string(),
            "ApiKeyCard".to_string(),
            serde_json::json!({"env": {"ANTHROPIC_BASE_URL": "https://direct.example"}}),
            None,
        );
        api_key_card.meta = Some(ProviderMeta {
            api_key_field: Some("ANTHROPIC_API_KEY".to_string()),
            api_keys: vec![ApiKeyEntry {
                id: "a1".to_string(),
                label: String::new(),
                key: "sk-x-api-key".to_string(),
                strategy: None,
            }],
            selected_key_id: Some("a1".to_string()),
            ..ProviderMeta::default()
        });
        db.save_provider("claude", &api_key_card)?;
        let stored = db
            .get_provider_by_id("claude-api-key", "claude")?
            .expect("provider");
        let effective =
            build_effective_settings_with_common_config(&db, &AppType::Claude, &stored)?;
        assert_eq!(
            effective["env"]["ANTHROPIC_API_KEY"],
            serde_json::json!("sk-x-api-key"),
            "apiKeyField 指向 ANTHROPIC_API_KEY 时必须落 API_KEY"
        );
        assert!(
            effective["env"].get("ANTHROPIC_AUTH_TOKEN").is_none(),
            "两种认证字段必须互斥"
        );
        Ok(())
    }

    #[test]
    fn materialize_shared_keys_unaffected_for_other_apps() -> Result<(), AppError> {
        let db = Database::memory()?;
        // Gemini 不在共享池支持范围：config 原样返回。
        let provider = Provider::with_id(
            "gemini-provider".to_string(),
            "Gemini".to_string(),
            serde_json::json!({"env": {"GEMINI_API_KEY": "sk-gemini"}}),
            None,
        );
        let effective =
            build_effective_settings_with_common_config(&db, &AppType::Gemini, &provider)?;
        assert_eq!(
            effective["env"]["GEMINI_API_KEY"],
            serde_json::json!("sk-gemini")
        );
        Ok(())
    }
}
