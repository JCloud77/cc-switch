# ToDoS
- [x] 供应商多 API Key 支持：每个供应商可存储多个 API Key（meta.api_keys），通过手动选择（meta.selected_key_id）指定当前生效的 Key；适配器 extract_auth() 优先读取选中 Key，Proxy 转发和 Live 配置写入使用选中 Key；前端增加 Key 列表管理 UI。

## 未完成

- [ ] 将本地魔改分支从官方 `v3.19.1` 升级适配到官方 `v3.19.2`（标签提交 `43eaf073`，发布于 2026-08-06）：以 `upgrade/upstream-v3.19.1` 最新提交 `4d1ff494` 为基线创建备份分支 `backup/upstream-v3.19.1-before-v3.19.2` 与升级分支 `upgrade/upstream-v3.19.2`，合并官方 `v3.19.2` 并完整保留全部本地魔改；官方本版无 schema 迁移（上游 `SCHEMA_VERSION` 仍为 16），本地必须继续保持 v17 及 `migrate_v16_to_v17`。
  - [x] 合并前已用 `git merge-tree --write-tree HEAD v3.19.2` 试合并验证：**零文本冲突**；正式合并同样无冲突。官方改动 114 个文件，与本地魔改重叠仅 10 个，其中 6 个为实质代码文件。
  - [x] 合并后逐项语义复核重叠文件：`database/mod.rs` 的 `SCHEMA_VERSION` 保持 17；`database/schema.rs` 仅吸收官方新增的 `qwen3.8-max` 定价种子，本地 v16→v17 迁移与 `assert_eq!(..., SCHEMA_VERSION)` 断言不得被回退为 16；`database/tests.rs` 确认官方将 `V3_8_SCHEMA_V1_SQL` 改为 `pub(super)` 后新测试模块仍可引用；`provider.rs` 确认官方新增的 `claude_uses_api_key_field()` 依赖的 `meta.api_key_field` 字段在本地结构体中仍存在。
  - [x] 重点复核 `proxy/forwarder.rs`：本地 Codex 通配/别名映射先执行，官方新增的 `[1m]` 剥离随后执行；Codex→Anthropic 路径仍按官方要求延后处理，没有绕过新逻辑。
  - [x] 合并四语言 `src/i18n/locales/{zh,en,ja,zh-TW}.json`：官方新增管理面板搜索、批量应用开关、认证中心订阅用量文案，本地新增测试状态图标与一键测试文案；合并后跑 JSON 校验与四语言键一致性检查。
  - [ ] 回归官方 v3.19.2 新行为与本地魔改的交叉点：代理缓冲响应体 128MiB 上限不影响本地 agent 最小真实探测请求；MCP/Skills 批量开关（串行写 live 配置）与本地“一键测试全部供应商”在同一列表页共存且互不阻塞；Codex 用量导入批量提交在本地 v17 数据库上可正常执行一次手动重建。
  - [x] 已评估官方 `main` 上尚未随版本发布的提交 `413c09e`：为保持本次升级严格对应正式标签 `v3.19.2`，暂不混入未发布提交，后续可作为独立修复评估。
  - [x] 已通过 i18n JSON/新增语言键校验、`typecheck`、`format:check`、105个测试文件共713项前端单测及 production renderer build。
  - [ ] 待 GitHub Actions Windows Manual Build（`run_checks=true`、`run_rust_tests=true`）通过后做 Windows 实机回归。

- [x] 修复测试状态图标在切换应用标签页后被重置：当前 `src/App.tsx` 中 `<AnimatePresence mode="wait">` 下的 `<motion.div key={activeApp}>` 会在切换应用标签时整体卸载并重建 `ProviderList`，而 `useStreamCheck` 的 `testStatuses` 与 `checkingIds` 都是组件内 `useState`，因此切走再切回、或进入设置/MCP 等非供应商面板后返回，全部测试结果都会退回“未测试”。
  - [x] 新建 `src/contexts/ProviderTestStatusContext.tsx`，沿用仓库既有 `UpdateContext` 的 Context 模式，在 `src/main.tsx` 中与 `UpdateProvider` 同层挂载于路由/面板切换之上，使状态不随 `ProviderList` 卸载而丢失。
  - [x] 状态键按应用作用域使用 `${appId}:${providerId}` 组合键，避免不同应用中的同名供应商 id 串台。
  - [x] 同时上提进行中的 `checkingIds`，切走再切回时仍能看到加载状态，且异步 `finally` 可更新仍然挂载的 Context。
  - [x] 状态仅存内存、不落库，维持“应用重启后恢复未测试状态”；已删除供应商的陈旧状态不会被当前列表读取或展示。
  - [x] 更新 `useStreamCheck` 测试包装器，并新增覆盖“消费者卸载重挂载后结果及加载状态保留”“跨应用不串台”的 Context 测试；定向 typecheck 与4项测试通过。

- [x] 为“一键测试全部供应商”增加并发限流与提示聚合：修复 `Promise.all` 全并发导致大量同时探测及 toast 刷屏的问题。
  - [x] 新增无依赖 `mapWithConcurrency` worker 池，一键测试固定并发上限为3。
  - [x] 为 `useStreamCheck.checkProvider` 增加 `silent` 批量模式，一键测试期间抑制所有逐供应商 toast；状态图标继续逐项反馈。
  - [x] 批量结束只弹一条汇总 toast，包含成功数、失败数、总耗时，并在失败时列出供应商名称。
  - [x] 新增并同步四语言汇总文案，保留重复触发防护与按钮禁用逻辑。
  - [x] 补充并通过并发上限、静默模式、汇总计数测试；定向 typecheck 与12项测试通过。

- [ ] 将当前本地魔改分支从官方 `v3.16.5` 升级适配到官方最新 `v3.19.1`：以 `upgrade/upstream-v3.16.5` 最新提交为基线创建安全备份和独立升级分支，合并官方 `v3.19.1`，完整保留 agent 风格供应商探测、一键测试全部供应商及状态图标、Codex 模型别名与通配映射、多 API Key 管理、Claude/Codex 跨应用共享 Key 池、Windows 手动构建流程等全部本地魔改；若合并出现需要内容取舍的冲突，暂停并征询用户意见；完成前端检查、Windows runner Rust/编译构建验证及 Windows 实机验证。
  - [x] 已创建安全备份分支 `backup/upstream-v3.16.5-before-v3.19.1` 和升级分支 `upgrade/upstream-v3.19.1`，合并官方 `v3.19.1`；冲突按用户确认采用 Schema v17、xAI OAuth→共享 Key→配置鉴权顺序、保留双阶段真实 Agent 测试及双方功能合并策略处理。
  - [x] 已逐项核查并保留全部本地魔改；共享 Key 迁移调整为幂等 v16→v17，并补充旧本地 v12 升级、Codex Anthropic 共享 Key 鉴权及 xAI OAuth 优先级测试。
  - [x] 已通过四语言 JSON 校验、`typecheck`、89 个测试文件共 594 项前端单测及 renderer production build。
  - [x] GitHub Actions Windows Manual Build 已在提交 `ee72045e` 上通过前端检查、Rust formatting、Clippy、共享 Key 专项测试、完整 Rust 单测及 exe 构建（run `30986918022`，artifact `8923358674`）。
  - [ ] 待 Windows 实机执行数据库 v12→v17 迁移及全部本地魔改回归验证；验证通过后再考虑合并回私有 `main` 和更新现有安装。

- [ ] 新增 Claude/Codex 跨应用共享供应商 Key：当两个应用中的供应商 API 地址主域名相同，或供应商名称忽略首尾空格与大小写后相同时，自动视为同一供应商；使用中央 Key 池只保存一份 Key，首次关联时合并双方现有 Key 并按完整 Key 值精确去重、同值优先保留非空标签；Claude 与 Codex 共享 Key 列表但分别保存当前选中 Key，确保测试、代理转发、Live 配置、导入导出与 WebDAV/S3 云同步继续正确工作，并通过非破坏性数据库迁移兼容已有多 Key 数据。
  - [x] 已新增 Schema v12 中央 Key 池、供应商关联表与非破坏性迁移；兼容旧 `apiKeys`/`selectedKeyId` 及仅存在于当前配置中的活动 Key。
  - [x] 已实现跨应用名称/根域名关联、完整 Key 精确去重、非空标签优先、双方独立选择，以及防止删除另一供应商仍在使用的 Key。
  - [x] 已接入现有供应商读取/保存/删除、认证解析、Live 配置、SQL 导入导出与 WebDAV/S3 完整数据库快照，并增加共享状态提示和四语言文案。
  - [x] 已通过 JSON 校验、`typecheck`、`format:check`、400 项前端单测及 renderer production build。
  - [ ] 待 GitHub Actions Windows runner 执行 Rust formatting、Clippy、单测、编译和 exe 构建，再做 Windows 实机迁移、共享、独立选 Key 与云同步验证。

- [ ] 新增按应用“一键测试全部供应商":在 Claude、Codex、Gemini、OpenCode、OpenClaw、Hermes、Claude Desktop 等应用的供应商列表中提供统一入口，一次触发该应用下所有可测试供应商的现有连通性/Agent 探测逻辑；行为应等同于逐个点击每张供应商卡片的测试按钮，保留逐供应商加载状态与结果提示，跳过原本不显示测试按钮的官方供应商，并在批量或单项测试进行时防止重复触发。
  - [x] 已增加应用级测试全部按钮、批量加载/禁用状态与四语言文案。
  - [x] 已复用 `useStreamCheck` 的单供应商检测，不新增另一套探测判定或触碰故障转移熔断器。
  - [x] 已在每张供应商卡片的供应商图标前增加测试状态图标：未测试显示中性状态、测试中显示加载状态、最近一次测试成功显示绿色图标、失败或异常显示红色图标；单项测试和一键测试共享同一份当前运行期间状态，应用重启后恢复未测试状态。
  - [x] 已补充批量触发、官方供应商跳过、防重复和状态图标覆盖，并通过 `typecheck`、`format:check`、395 项前端单测、renderer production build，以及升级分支 `affcdb63` 的 GitHub Actions Windows 检查与 exe 构建。
  - [ ] 待 Windows 实机验证一键测试、逐供应商结果提示以及中性/加载/绿色成功/红色失败状态图标。

- [ ] 新增 Codex 通配模型映射：在供应商模型映射中，如果唯一一条配置填写了“实际请求模型”但将“菜单显示名”留空，则把经过该供应商代理的所有带模型请求统一改写为该实际请求模型；显式空值需与旧配置中缺失显示名区分，多个空显示名属于歧义配置并禁止保存，后端对导入的歧义配置采取安全的不启用通配策略；同步四语言说明并补充前后端测试。
  - [x] 已实现显式空显示名持久化、旧配置兼容、全请求通配改写、多个通配前端校验及后端安全降级，并更新四语言说明与输入提示。
  - [x] 已补充前端归一化/旧配置读取测试和 Rust 通配/旧配置/歧义配置单测；通过 `typecheck`、`format:check`、397 项前端单测及 renderer production build。
  - [ ] 待 GitHub Actions Windows runner 执行 Rust formatting、Clippy、编译和 exe 构建，并在 Windows 实机验证任意请求模型均改写到通配目标。

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

