# ToDoS
- [x] 供应商多 API Key 支持：每个供应商可存储多个 API Key（meta.api_keys），通过手动选择（meta.selected_key_id）指定当前生效的 Key；适配器 extract_auth() 优先读取选中 Key，Proxy 转发和 Live 配置写入使用选中 Key；前端增加 Key 列表管理 UI。

## 未完成

- [ ] 将本地魔改分支从官方 `v3.20.0` 升级适配到官方 `v3.20.3`（本轮计划见 `.pi/PLAN.md`）：目标 ref 为严格官方标签 `v3.20.3`（2026-09-11），中间含 `v3.20.1`/`v3.20.2`；`git diff v3.20.0 v3.20.3` 实测 152 文件 / +24,262 / −2,232（Rust 48 文件 / +13,332 / −1,430）。三版主线：3.20.1 = Codex config-only 切换重构 + 账号/数据安全堵漏 + 会话扫描字节游标（含 schema v17→v18）；3.20.2 = Grok 经 xAI 原生 Responses + 一族修复 + 定价/预设；3.20.3 = Kimi 预设改原生 Responses 直连 + 代理正确性修复。以 `upgrade/upstream-v3.20.0` 最新提交 `b033f563` 为基线建备份分支 `backup/upstream-v3.20.0-before-v3.20.3` 与升级分支 `upgrade/upstream-v3.20.3`。
  - [ ] 实测已确认的合并形态：两侧共同修改文件 14 个；唯一真实文本冲突在 `database/schema.rs` 的 `17 => {...}` 迁移臂；`database/mod.rs` 两侧都把 `SCHEMA_VERSION` 提到 18，**取值相同、静默干净合并**（最危险点）；暴力解冲突会产生两个同名 `migrate_v17_to_v18` → E0428 响亮失败；`proxy/forwarder.rs`、`proxy/providers/{codex,claude,mod}.rs`、`provider.rs`、`database/tests.rs`、`useCodexConfigState.ts`、`types.ts`、四语言 JSON 均干净合并；本地 WSL 原子写修复的 `config.rs` 官方这三版一行未动，零风险保留。
  - [ ] **Schema 定为「单一 v18 = 官方 v18 ∪ 本地 v18」**：单一 `migrate_v17_to_v18` 同时补官方字节游标两列（`last_byte_offset`/`last_tail_fingerprint`）与本地共享 Key 池；`SCHEMA_VERSION` 保持 18，不占用官方未来 19。
  - [ ] **版本无关可重入的 `ensure_v18_structures`**：实机库 `/mnt/c/Users/yun/.cc-switch/cc-switch.db` 实测 `user_version = 18`（本地语义）且 `session_log_sync` 缺两列，版本循环一步都不会进，因此必须由幂等结构确保函数兜底；`17 =>` 迁移臂与 while 循环结束后 savepoint 提交前各调用一次。
  - [ ] **纯结构确保与共享 Key 数据迁移分离**：`create_shared_key_tables_on_conn` 后按 `classify_shared_key_pool_state` 分类处理 —— `CompleteWithMarker`（不重写）/ `CompleteWithoutMarker`（仅补 marker，逐值保留 Key、label、sort_index、group、link、provider 原始 meta）/ `LegacyUninitialized`（复用初次迁移 + 校验 + 写 marker）/ `Inconsistent`（明确报错，不重建或覆盖用户池）；marker 使用 settings 保留键 `shared_key_pool_migrated_v18`，与迁移同属一个 savepoint；settings 表缺失时不写 marker，不可解析 marker 值报错；无 `providers` 表的部分 schema 夹具不写 marker。
  - [ ] **equal-version repair 的备份预检与初始化顺序**：`Database::init` 必须在 `create_tables()` **之前**读取 `user_version` 与结构状态，识别 `schema_needs_v18_repair`（缺 `session_log_sync`/游标列/dedup/共享池表或必需列/有效 marker、池状态异常）；有用户表的存量库一律先做严格备份（要求 `Ok(Some(path))`，`Err`/`Ok(None)` 均拒绝执行修复），移除旧的“备份失败 warning 后继续迁移”行为；`version > 18` 在 create_tables/修复前直接拒绝；新空库不备份；调用前释放 DB mutex 避免死锁；同次启动只备份一次。
  - [ ] **迁移验收矩阵（21 项）**：全新建库 / 官方 v17 / 本地 v17 / 本地旧 v17 无 dedup / 官方 v16→v18 / 本地旧 v12→v18 / 已完整 18 幂等 / 非 TEXT+BLOB provider config / 已有池与选中 Key 逐值保真 / 中断态二次迁移 / `assert_eq!(user_version, SCHEMA_VERSION)` / 实机等价形态（盖章 18 缺游标列，池数据与关联不变）/ 官方 v18 盖章库建池 / 备份门禁判定 / 无 providers 不写 marker / 有 marker 不重跑 / 补 marker 不复活旧 Key / marker 冲突报错回滚 / 备份失败与 version>18 不改写 / SQL 与 SQLite 往返含 marker / 迁移中途故障注入全量回滚。
  - [ ] **共享 Key 与新链路衔接（方案 A 单点物化）**：新增 `src-tauri/src/services/provider/shared_key_live.rs`，在 `build_effective_settings_with_common_config` 尾部把池中选中 Key 物化进**内存** effective settings（数据库不动），覆盖普通 live 构建、Claude 接管同步（`services/proxy.rs:685/767`）与接管备份重建（`services/proxy.rs:2819`）三路；Codex 的 OAuth/官方卡跳过池 Key，Claude 保留选中手工 Key 优先，按池条目 strategy 决定 `ANTHROPIC_AUTH_TOKEN`/`ANTHROPIC_API_KEY` 并互斥。新增 `Database` 级 hydrate wrapper，对 provider 先内存 clone 并 hydrate `meta.api_keys` 后再物化（覆盖“传入 Provider 只有 selectedKeyId”的保存后路径）；不得写回数据库，DB 错误向上传递，异常配置返回 `AppError` 而非 panic。
  - [ ] 新增 Rust 单测（函数名统一含 `shared_keys`，命中 CI 的 `cargo test --lib shared_keys` 步骤）：hydrate 物化、Codex 池 Key 通过预检并写入 config.toml、空配置仍被拒、Claude 两种池策略落点、Codex/xAI OAuth/Copilot/官方卡不被顶替、非 Claude/Codex 不受影响；补保存后 reload 已归一化 provider、同值 Key 合并导致 ID 改变的 service 级回归。
  - [ ] D 节语义审查写进本文件：`proxy/forwarder.rs` 通配映射 → `[1m]` 剥离 → Codex→Anthropic 延后的顺序与 4 参 `validate_codex_official_authorization`；`proxy/providers/codex.rs` 新判定与本地共享 Key 分支共存；OAuth 优先级差异仅记录不改行为；config-only 切换删除 `auth.json` 对本地选中 Key 链路与接管恢复的影响；`useCodexConfigState.ts` 上游初始化顺序 + bearer token 重建 vs 本地通配显示名映射；上游大改面（`services/provider/mod.rs`、`services/proxy.rs`、`codex_config.rs`、`proxy/providers/codex_oauth_auth.rs`）逐项核对；四语言 JSON 键一致性；本地专有资产（`config.rs`、`services/stream_check.rs`、`ProviderTestStatusContext.tsx`、`mapWithConcurrency.ts`、`.github/workflows/windows-manual-build.yml`、`.gitignore`）未被触碰。
  - [ ] 本地门禁（WSL，前端）：i18n JSON 解析 + 四语言键一致性、`pnpm typecheck`、`pnpm format:check`、`pnpm test:unit`、`pnpm build:renderer`。
  - [ ] 推送 `upgrade/upstream-v3.20.3` 并触发 Windows Manual Build（`run_checks=true`、`run_rust_tests=true`）；CI 失败继续在升级分支修复，不提前进入实机阶段。
  - [ ] **本轮终点为 CI 通过即止**：不安装、不替换本机 exe、不触碰 `/mnt/c/Users/yun/.cc-switch`；实机升级、共享 Key 全链路回归、合并回私有 `main`、更新安装另行安排。
  - [ ] **实机遗留项（后续授权后执行）**：完整备份 `.cc-switch`（`cc-switch.db`、`settings.json`、`backups/`、`skills/`）；v18 等版本修复在 47 条真实池数据上首启验证；共享 Key 全链路（创建/编辑/删除、跨应用共享、选中 Key 切换与回退、表单与配置 JSON 同步、live 写入、代理取 Key、导入导出/备份恢复保真）；程序回退必须同时恢复迁移前数据，不能仅换 exe。

- [ ] 将本地魔改分支从官方 `v3.19.2` 升级适配到官方 `v3.20.0`（本轮已深入比对两套代码并采纳审核意见）：官方正式标签 `v3.20.0` 的 commit = `0b5da5101689...`（tag 对象 SHA 为 `b944ef79`），发布于 2026-08-18；`git diff v3.19.2 v3.20.0` 实测 291 files / +54,385 / −6,682（release notes 写 284/53,108，为提交口径差异）。以 `upgrade/upstream-v3.19.2` 最新提交 `9a527ff1` 为基线建备份分支 `backup/upstream-v3.19.2-before-v3.20.0` 与升级分支 `upgrade/upstream-v3.20.0`。
  - [x] Schema v18、迁移测试（路径 3/5 补充，其余路径已有覆盖）、WSL 对齐、四语言合并已实现并提交；本地前端通过 `typecheck`、`format:check`、136 文件 / 1005 项单测、`build:renderer`。
  - [x] 已实测 `git merge-tree HEAD v3.20.0`：**8 个文件真实文本冲突** —— `config.rs`、`database/schema.rs`、`ProviderList.tsx`、`useCodexConfigState.ts`、`zh-TW.json`、`ProviderCardLayout.test.ts`、`ProviderForm.codexCatalog.test.ts`、`ProviderList.test.tsx`（不同于 v3.19.2 的零冲突）。
  - [x] GitHub Actions Windows Manual Build（run `32165499523`，commit `cddf2767`）通过前端检查、Rust formatting、Clippy、共享 Key 专项、完整 Rust 单测及 exe 构建，artifact `9336374736`（expires 2026-11-16）。
  - [x] **Schema 定为 v18 方案（采纳审核，弃方案 b）**：实证发现 `database/mod.rs` 的 `SCHEMA_VERSION` 官方 v3.20.0 与本地 HEAD 均为 `17`——两行完全相同，merge 时**不报冲突**，会让“官方 v17 结构 vs 本地 v17 结构”分叉静默通过，是最危险陷阱。实施必须：a) 将 `SCHEMA_VERSION` 改为 `18`；b) 保留官方 `migrate_v16_to_v17`（建 `session_usage_dedup`）；c) 新增本地 `migrate_v17_to_v18`（共享 Key 池 + 关联迁移）；d) v17→v18 内置结构探测/修复：旧本地 v17 库（有共享 Key、缺 `session_usage_dedup`）也要补建官方表；e) 所有 DDL/数据搬迁保持幂等，避免半完成迁移后重启二次损坏；f) 兼容官方 v16、本地 v17、本地旧 v12 三路汇入 v18。
  - [x] **迁移验收写成明确矩阵**（逐路径检查：最终 version、`session_usage_dedup` 结构正确、`shared_api_keys` 及关联表存在、provider 配置保持原值、选中 Key 仍生效、重启二次迁移无副作用、备份先于写迁移生成）：1) 全新建库→v18；2) 官方 v16→官方 v17→本地 v18；3) 旧本地 v17（无 dedup）→修复→v18；4) 旧本地 v12→连续迁移→v18；5) 已完整 v18 再次启动；6) 迁移含非 TEXT provider config/BLOB；7) 迁移前已有共享 Key/选中 Key/provider 关联；8) 部分表已建但 version 未更新的中断态。
  - [x] **WSL 修复双份对齐（不能只看“本地更多就保留”）**：实证官方 v3.20.0 把 `atomic_write` 重构为 `atomic_write` + `atomic_write_private(path, data, unix_mode)`，并在 ReplaceFileW 失败时以 `raw_os_error()==ERROR_NOT_SUPPORTED(50)` 追加回退到 `fs::rename`（仅一层）；本地 #16 则增加了“rename 也不支持时备份原文件→放入新文件→失败自动恢复”。需逐项比对行为矩阵（错误50分支、原文件有无、rename 已存在、跨文件系统、替换失败恢复、恢复失败备份、临时文件残留、权限异常、WSL 挂载盘 vs NTFS、成功前是否落盘），并吸收官方新增的 `unix_mode` 参数与 WSL2 真实 CI，保留本地更完整的回退语义；确认官方是否同时改了调用层/错误分类而不止 `config.rs` 单函数。
  - [ ] **交叉风险清单补共享 Key 全链路**：本地魔改实际触及 provider/shared_keys DAO、schema/迁移、Provider Form、Codex 配置状态、ApiKeyManager、stream/connectivity check、query mutations、四语言。回归不止看编译：创建/编辑/删除共享 Key、Claude/Codex 共享关系、选中 Key 切换、删除选中 Key 回退、表单同步、配置 JSON 同步、Live 写入、Proxy/agent 测试取 Key、导入导出/备份恢复保真、旧 `meta.api_keys` 迁移、非文本 provider config 不被破坏。
  - [ ] 回归官方新行为与本地魔改交叉点：官方卡退出自动故障转移（与本地故障转移/一键测试状态图文的交集）；DeepSeek V4 峰值档重定价；Kimi 干净透传；Codex Goals 移除；模型选择器模糊搜索组合框（与本地 Codex 通配/别名映射共用组件区，`useCodexConfigState.ts` 是冲突文件）。
  - [x] 合并四语言 `src/i18n/locales/{zh,en,ja,zh-TW}.json` 与 Pi 文案；执行 i18n键一致性、`pnpm typecheck`、`format:check`、`test:unit`、`build:renderer`。
  - [x] **严格范围（采纳审核拆分）**：默认只合并正式标签 `v3.20.0`；标签后 main 的修复不整体吸收；如发现**阻断升级**的上游修复，仅按独立 commit 精确 cherry-pick，每个额外 commit 单独记录 SHA/修复内容/引入原因/风险/测试；最终版本说明标注“v3.20.0 + 本地补丁”。
  - [ ] **用户数据备份前置门槛（硬性）**：任何替换 Windows 程序或运行迁移前，退出正在运行的 CC Switch，确认实际数据目录（含 `app_config_dir_override`），完整备份 `.cc-switch`（`cc-switch.db`、`settings.json`、`backups/`、`skills/`），记录路径与时间；迁移失败只从迁移前备份恢复，绝不用测试/空库覆盖真实库；**程序回退≠数据库回退**，真实库升到 v18 后换旧 exe 不安全，回滚须同时恢复迁移前数据。
  - [x] **CI 与实机分阶段通过条件**：CI 阶段（Windows runner：typecheck/format/test:unit/renderer build + Rust fmt/Clippy/-D warnings/共享Key专项/完整 Rust 单测 + exe 构建）与实机阶段（数据备份、v3.19.2 安装实态升级、全新库/旧本地 v17/旧 v12 首启、Claude/Codex/Gemini/WSL 写入、共享Key 选择删除转发、一键测试状态保持、故障转移、Codex 用量重建、WebDAV/S3 首次同步、退出重启、备份/恢复验证）分开设定；**先本地可审查提交，再推送触发 CI**，CI 失败在升级分支继续修复，不提前进入 Windows 安装阶段。

- [x] 修复 Windows 版 v3.19.2 对 WSL/UNC 配置路径的全局写入回归：官方将所有受管配置的原子写入改为 `ReplaceFileW`，但 `\\wsl.localhost\...` 文件系统不支持该 API并返回 `ERROR_NOT_SUPPORTED`（os error 50），导致 Grok Build、Claude、Codex 等复用 `config.rs::atomic_write` 的配置均无法修改。
  - [x] 本地 NTFS 继续优先使用 `ReplaceFileW`；错误50时先回退到覆盖式 rename，不支持时再以“原文件改名保留→放入新文件→失败自动恢复”完成兼容替换，恢复失败会明确报告原文件备份位置。
  - [x] 新增 Windows 错误50识别、兼容替换和既有文件锁定保护测试；GitHub Actions run `31499983749` 已通过 Rust formatting、Clippy、共享 Key 专项测试、完整 Rust 单测及 exe 构建，artifact `9105459055`。

- [ ] 将本地魔改分支从官方 `v3.19.1` 升级适配到官方 `v3.19.2`（标签提交 `43eaf073`，发布于 2026-08-06）：以 `upgrade/upstream-v3.19.1` 最新提交 `4d1ff494` 为基线创建备份分支 `backup/upstream-v3.19.1-before-v3.19.2` 与升级分支 `upgrade/upstream-v3.19.2`，合并官方 `v3.19.2` 并完整保留全部本地魔改；官方本版无 schema 迁移（上游 `SCHEMA_VERSION` 仍为 16），本地必须继续保持 v17 及 `migrate_v16_to_v17`。
  - [x] 合并前已用 `git merge-tree --write-tree HEAD v3.19.2` 试合并验证：**零文本冲突**；正式合并同样无冲突。官方改动 114 个文件，与本地魔改重叠仅 10 个，其中 6 个为实质代码文件。
  - [x] 合并后逐项语义复核重叠文件：`database/mod.rs` 的 `SCHEMA_VERSION` 保持 17；`database/schema.rs` 仅吸收官方新增的 `qwen3.8-max` 定价种子，本地 v16→v17 迁移与 `assert_eq!(..., SCHEMA_VERSION)` 断言不得被回退为 16；`database/tests.rs` 确认官方将 `V3_8_SCHEMA_V1_SQL` 改为 `pub(super)` 后新测试模块仍可引用；`provider.rs` 确认官方新增的 `claude_uses_api_key_field()` 依赖的 `meta.api_key_field` 字段在本地结构体中仍存在。
  - [x] 重点复核 `proxy/forwarder.rs`：本地 Codex 通配/别名映射先执行，官方新增的 `[1m]` 剥离随后执行；Codex→Anthropic 路径仍按官方要求延后处理，没有绕过新逻辑。
  - [x] 合并四语言 `src/i18n/locales/{zh,en,ja,zh-TW}.json`：官方新增管理面板搜索、批量应用开关、认证中心订阅用量文案，本地新增测试状态图标与一键测试文案；合并后跑 JSON 校验与四语言键一致性检查。
  - [ ] 回归官方 v3.19.2 新行为与本地魔改的交叉点：代理缓冲响应体 128MiB 上限不影响本地 agent 最小真实探测请求；MCP/Skills 批量开关（串行写 live 配置）与本地“一键测试全部供应商”在同一列表页共存且互不阻塞；Codex 用量导入批量提交在本地 v17 数据库上可正常执行一次手动重建。
  - [x] 已评估官方 `main` 上尚未随版本发布的提交 `413c09e`：为保持本次升级严格对应正式标签 `v3.19.2`，暂不混入未发布提交，后续可作为独立修复评估。
  - [x] 已通过 i18n JSON/新增语言键校验、`typecheck`、`format:check`、105个测试文件共713项前端单测及 production renderer build。
  - [x] 首轮 Windows CI 暴露官方备份 BLOB 保真测试与本地 v17 共享 Key 迁移的交叉问题：已让迁移只处理 TEXT JSON 行、对非文本 provider 配置保留原值并安全跳过，同时新增回归测试。
  - [x] GitHub Actions Windows Manual Build 已在提交 `77827e0a` 上通过前端检查、Rust formatting、Clippy、共享 Key 专项测试、完整 Rust 单测及 exe 构建（run `31322930444`，artifact `9041088565`）。
  - [ ] 待 Windows 实机回归 v12→v17 迁移、全部本地魔改，以及 v3.19.2 的 Codex 用量重建和首次 WebDAV/S3 同步。

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

