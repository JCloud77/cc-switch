# ToDoS
- [x] 供应商多 API Key 支持：每个供应商可存储多个 API Key（meta.api_keys），通过手动选择（meta.selected_key_id）指定当前生效的 Key；适配器 extract_auth() 优先读取选中 Key，Proxy 转发和 Live 配置写入使用选中 Key；前端增加 Key 列表管理 UI。

## 未完成

- [ ] 新增按应用“一键测试全部供应商”：在 Claude、Codex、Gemini、OpenCode、OpenClaw、Hermes、Claude Desktop 等应用的供应商列表中提供统一入口，一次触发该应用下所有可测试供应商的现有连通性/Agent 探测逻辑；行为应等同于逐个点击每张供应商卡片的测试按钮，保留逐供应商加载状态与结果提示，跳过原本不显示测试按钮的官方供应商，并在批量或单项测试进行时防止重复触发。
  - [x] 已增加应用级测试全部按钮、批量加载/禁用状态与四语言文案。
  - [x] 已复用 `useStreamCheck` 的单供应商检测，不新增另一套探测判定或触碰故障转移熔断器。
  - [x] 已在每张供应商卡片的供应商图标前增加测试状态图标：未测试显示中性状态、测试中显示加载状态、最近一次测试成功显示绿色图标、失败或异常显示红色图标；单项测试和一键测试共享同一份当前运行期间状态，应用重启后恢复未测试状态。
  - [ ] 已补充批量触发、官方供应商跳过、防重复和状态图标覆盖，并通过 `typecheck`、`format:check`、395 项前端单测与 renderer production build；待 GitHub Actions Windows 构建验证。

- [ ] 将当前本地魔改分支升级到官方 `v3.16.5`：从当前 `main` 创建安全备份与独立升级分支，合并官方 release tag，保留 agent 风格供应商探测、Codex 模型别名映射、多 API Key 管理及 Windows 手动构建流程；解决冲突并完成前端检查与 GitHub Actions Windows 构建验证。
  - [x] 已创建安全分支 `backup/custom-main-before-upstream-v3.16.5` 与升级分支 `upgrade/upstream-v3.16.5`，并完成官方 `v3.16.5` 合并。
  - [x] 已核查本地魔改保留情况，并通过 i18n JSON、`typecheck`、`format:check`、388 项前端单测与 renderer production build。
  - [x] GitHub Actions Windows Manual Build 已在升级分支 `fc8a4431` 上通过 Rust formatting、Clippy 与 exe 构建。
  - [ ] Windows 手动验证通过后，再将升级分支合并回私有 `main`。

- [x] 修复多 API Key 选择后的表单同步：点击备用 Key 列表中的某个 Key 后，应立即同步更新上方 API Key 输入框与下方配置 JSON（Claude/Codex/Gemini），删除当前选中 Key 后也应同步切换到下一个 Key 或清空，避免 UI 显示与实际保存内容不一致。

- [ ] 修改 CC Switch 的供应商测试功能：在现有仅测试 Base URL 连通性的基础上，新增“模拟 Agent 请求”的测试模式，用于识别供应商限制模型只能被 Claude Code、Codex 等 agents 使用时的真实可用性。第一版优先覆盖 Claude Code 与 Codex；测试流程采用“双阶段真实测”（先保留现有 Base URL/健康检查，再发送极低 token 的 agent 风格最小推理请求）；模型名优先读取当前 provider 配置；前端尽量不改 UI，只改判定逻辑与错误解释，避免简单把 400/403/500/502/503 都视为 Base URL 不通。
  - [x] 后端已实现 Base URL 可达性 + Claude/Codex agent 风格最小真实请求探测，并保留不触碰故障转移熔断器的不变量。
  - [x] 前端已同步结果类型、toast 判定文案与 i18n 说明，尽量不改变现有 UI。
  - [x] 已通过前端 `typecheck`、`format:check`、`test:unit`、`build:renderer`。
  - [ ] Rust `cargo fmt` / 编译 / 测试待在具备 Cargo 工具链的环境执行；当前 WSL 环境 `cargo: command not found`。

## 已完成

- [x] 新增 GitHub Actions 手动 Windows 构建流程：允许在不安装本机 Rust 工具链的情况下，把已推送到 GitHub 的当前分支在 Windows runner 上构建，上传 `cc-switch.exe` 作为 artifact；产物仅用于 Windows 测试/更新，替换本机安装前仍需备份用户数据目录。
- [x] 修复 Codex 本地代理模型别名映射：当 pi 等 API 客户端直接发送 Codex `modelCatalog` 的菜单显示名（如 `gpt-5.5`）时，CC Switch 会在转发前映射为该供应商配置的实际上游模型（如 `deepseek-v4-pro`），而不是仅依赖 Codex CLI 读取生成的模型目录。
- [x] 补齐 agent 探测默认模型：当 Claude/Codex 供应商没有显式配置模型时，不再报“缺少模型名”；Claude Code 探测默认使用 `claude-opus-4-8`，Codex 探测默认使用 `gpt-5.5`，与对应 agent 默认行为一致。

