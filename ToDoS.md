# ToDoS

## 未完成

- [ ] 修改 CC Switch 的供应商测试功能：在现有仅测试 Base URL 连通性的基础上，新增“模拟 Agent 请求”的测试模式，用于识别供应商限制模型只能被 Claude Code、Codex 等 agents 使用时的真实可用性。第一版优先覆盖 Claude Code 与 Codex；测试流程采用“双阶段真实测”（先保留现有 Base URL/健康检查，再发送极低 token 的 agent 风格最小推理请求）；模型名优先读取当前 provider 配置；前端尽量不改 UI，只改判定逻辑与错误解释，避免简单把 400/403/500/502/503 都视为 Base URL 不通。
  - [x] 后端已实现 Base URL 可达性 + Claude/Codex agent 风格最小真实请求探测，并保留不触碰故障转移熔断器的不变量。
  - [x] 前端已同步结果类型、toast 判定文案与 i18n 说明，尽量不改变现有 UI。
  - [x] 已通过前端 `typecheck`、`format:check`、`test:unit`、`build:renderer`。
  - [ ] Rust `cargo fmt` / 编译 / 测试待在具备 Cargo 工具链的环境执行；当前 WSL 环境 `cargo: command not found`。

- [ ] 修复 Codex 本地代理模型别名映射：当 pi 等 API 客户端直接发送 Codex `modelCatalog` 的菜单显示名（如 `gpt-5.5`）时，CC Switch 应在转发前映射为该供应商配置的实际上游模型（如 `deepseek-v4-pro`），而不是仅依赖 Codex CLI 读取生成的模型目录。

## 已完成

- [x] 新增 GitHub Actions 手动 Windows 构建流程：允许在不安装本机 Rust 工具链的情况下，把已推送到 GitHub 的当前分支在 Windows runner 上构建，上传 `cc-switch.exe` 作为 artifact；产物仅用于 Windows 测试/更新，替换本机安装前仍需备份用户数据目录。

