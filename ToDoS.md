# ToDoS
- [x] 供应商多 API Key 支持：每个供应商可存储多个 API Key（meta.api_keys），通过手动选择（meta.selected_key_id）指定当前生效的 Key；适配器 extract_auth() 优先读取选中 Key，Proxy 转发和 Live 配置写入使用选中 Key；前端增加 Key 列表管理 UI。

## 未完成

- [ ] 将本地魔改分支从官方 `v3.20.0` 升级适配到官方 `v3.20.3`（本轮计划见 `.pi/PLAN.md`）：目标 ref 为严格官方标签 `v3.20.3`（2026-09-11），中间含 `v3.20.1`/`v3.20.2`；`git diff v3.20.0 v3.20.3` 实测 152 文件 / +24,262 / −2,232（Rust 48 文件 / +13,332 / −1,430）。三版主线：3.20.1 = Codex config-only 切换重构 + 账号/数据安全堵漏 + 会话扫描字节游标（含 schema v17→v18）；3.20.2 = Grok 经 xAI 原生 Responses + 一族修复 + 定价/预设；3.20.3 = Kimi 预设改原生 Responses 直连 + 代理正确性修复。以 `upgrade/upstream-v3.20.0` 最新提交 `b033f563` 为基线建备份分支 `backup/upstream-v3.20.0-before-v3.20.3` 与升级分支 `upgrade/upstream-v3.20.3`。
  - [x] 实测已确认的合并形态：两侧共同修改文件 14 个；唯一真实文本冲突在 `database/schema.rs` 的 `17 => {...}` 迁移臂；`database/mod.rs` 两侧都把 `SCHEMA_VERSION` 提到 18，**取值相同、静默干净合并**（最危险点）；暴力解冲突会产生两个同名 `migrate_v17_to_v18` → E0428 响亮失败；`proxy/forwarder.rs`、`proxy/providers/{codex,claude,mod}.rs`、`provider.rs`、`database/tests.rs`、`useCodexConfigState.ts`、`types.ts`、四语言 JSON 均干净合并；本地 WSL 原子写修复的 `config.rs` 官方这三版一行未动，零风险保留。
  - [x] 已建安全网 `backup/upstream-v3.20.0-before-v3.20.3`，从 `b033f563` 切出 `upgrade/upstream-v3.20.3`；`git merge --no-ff v3.20.3` 实际结果与实测预测一致——**唯一冲突文件 `src-tauri/src/database/schema.rs`**，其余 149 个文件自动合并；合并提交 `1074b490`，规模 155 文件 / +26,065 / −2,255。
  - [x] **Schema 定为「单一 v18 = 官方 v18 ∪ 本地 v18」**：新增 `ensure_v18_structures`，官方字节游标两列（`last_byte_offset`/`last_tail_fingerprint`）∪ 本地共享 Key 池；`migrate_v17_to_v18` 收敛为它的薄封装，`schema.rs` 内 `fn migrate_v17_to_v18` 定义唯一；`SCHEMA_VERSION` 保持 18，不占用官方未来 19。
  - [x] **版本无关可重入**：`apply_schema_migrations_on_conn` 在 `17 =>` 迁移臂与 while 循环结束后（savepoint 提交前）各调用一次 `ensure_v18_structures`，覆盖实机「已盖章 18 但缺游标列」形态；有 `settings` 但缺游标列时 `delete` 后直接运行迁移也能正确补齐。
  - [x] **纯结构确保与共享 Key 数据迁移分离**：`ensure_v18_structures` → 补游标列 → 补 `session_usage_dedup` → 无 `providers` 表则仅补结构不写 marker → 建池表**之前**先做只读冲突探测 `ensure_v18_pool_marker_consistent_with_raw_state`（marker 与原始池结构矛盾、缺列但残留数据均直接拒绝）→ `classify_shared_key_pool_state` 四态分流：`CompleteWithMarker`（零写入）/ `CompleteWithoutMarker`（只补 marker，逐值保留 Key、label、sort_index、分组、关联与 provider 原始 meta）/ `LegacyUninitialized`（复用 `migrate_provider_keys_to_shared_pools` + `validate_shared_key_pool_state` + marker）/ `Inconsistent(reason)`（明确报错，不重建不覆盖）。marker 使用 settings 保留键 `shared_key_pool_migrated_v18`，与迁移同属一个 savepoint；settings 表缺失时不写 marker；不可解析的 marker 值报错而不是当作已完成。
  - [x] **equal-version repair 的备份预检与初始化顺序**：`Database::init` 调整为「open → 读 version/结构 → 判定 → 备份 → create_tables → apply_schema_migrations」；新增 `safety_backup_reason`（有用户表且需迁移或需 v18 修复才备份，全新库不备份）与 `schema_needs_v18_repair`（缺 `session_log_sync`/游标列/dedup/池表或必需列/有效 marker，或池状态异常）；备份严格要求 `Ok(Some(path))`，`Err` 与 `Ok(None)` 都直接终止启动而不再「warning 后继续」；`version > 18` 在任何写入前拒绝；备份调用在释放 DB mutex 之后，避免与 backup 内部取锁互锁。
  - [x] **导入/恢复的严格备份门禁**：新增 `require_safety_backup_before_replace`，SQL 导入与 SQLite 恢复在替换主库前若为主库存在却拿不到安全备份则直接拒绝；staging 库的建池/marker/补列统一走 `apply_schema_migrations_on_conn` 的 ensure。
  - [x] **共享 Key 与新链路衔接（方案 A 单点物化）**：新增 `src-tauri/src/services/provider/shared_key_live.rs`；`build_effective_settings_with_common_config` 先对 provider 做内存 clone 并经新增 `Database::hydrate_shared_keys_for_provider_in_db` 按池 hydrate（后续共用配置判定、OAuth 判定、默认值与物化全部使用该 clone，不写回数据库），再调用 `materialize_selected_shared_key`。覆盖普通 live 构建、Codex 写入预检、切换事务、Claude 接管同步（`proxy.rs:685/767`）与接管备份重建（`proxy.rs:2825`）。Codex 的 xAI/Copilot 代理注入 OAuth、codex_oauth、官方卡跳过池 Key；Claude 保留选中手工 Key 优先，按池条目 strategy 在 `ANTHROPIC_AUTH_TOKEN`/`ANTHROPIC_API_KEY` 间互斥落点，无 strategy 时回退 `meta.apiKeyField`；非对象 `env`/`auth` 返回 `AppError` 而不覆盖整个配置。
  - [x] **保存后 reload 边界**：新增 `ProviderService::reload_provider_after_save`，在 `add`（常规路径、累加模式）与 `update`（累加模式、一般路径）的 live/备份写入前重读已归一化 provider，避免池内同值 Key 合并重映射 `selectedKeyId` 后物化出解析不到的选中 Key；托管 Codex 事务路径保持「DB 最后提交」设计不变（该路径本就跳过池 Key）。
  - [x] **迁移验收矩阵（21 项）已落到 `database/tests.rs`**：全新建库 / 官方 v17（含池与游标列补建 + marker）/ 本地 v17（池与选中 Key 逐值保真、meta 不再存 Key 列表）/ 本地旧 v17 缺 dedup / 本地旧 v12 连续链 / 官方 v16 连续链 / 中断态恢复 / 实机等价形态（盖章 18 缺游标列，47 条池数据等价夹具，存量行保持 NULL）/ 完整池无 marker 只补 marker 且不复活旧 Key / marker 与结构矛盾拒绝 / 悬空关联拒绝且补列回滚 / 无 `providers` 不写 marker 且补齐后仍会迁移 / 非 TEXT 与 BLOB 配置原样保留 / 备份门禁纯判定 + 探测函数 / SQL 往返保 marker 与池 / `version > 18` 拒绝且不改写。历史夹具用历史 DDL（含重建表退回无游标列形态），不再用「先建当前 schema 当旧库」。
  - [x] 共享 Key 物化测试补齐到 13 项，`.pi/PLAN.md` C 节要求全覆盖：hydrate + 物化（请求对象只带 `selectedKeyId` 也正确）、Codex 池 Key 进入 `auth.OPENAI_API_KEY` 并通过 `preflight_codex_live_write`、config.toml 投影用池 Key、空配置仍被拒、Claude 两种池策略落点互斥、`codex_oauth`/`github_copilot`/官方卡不被池 Key 顶替、Claude 侧手工 Key 优先的现状基线（计划 D3 记录用）、非 Claude/Codex 不受影响、非对象配置报错；另有 service 级回归 `shared_keys_save_normalizes_selection_before_live_write`（同值 Key 合并重映射 `selectedKeyId` 后仍能物化出池 Key）。
  - [x] D 节语义审查逐项完成：`forwarder.rs` 中 `validate_codex_official_authorization`（4 参）先于本地通配映射（`:1253`）执行，`[1m]` 剥离（`:1284/1291`）在其后且按上游要求跳过 Codex→Anthropic 路径（该路径稍后在最终 anthropic_body 上剥离）；`proxy/providers/codex.rs` 新增 `is_codex_official_provider`/`should_convert_*`/`provider_needs_responses_namespace_flatten` 与本地共享 Key 分支（`:1051`）共存；OAuth 优先级差异仍保持现状（本轮不重构代理适配器取 Key 顺序）；config-only 切换删除 `auth.json` 由 `preserve_official_login` 决定，选中 Key 链路改由 live 物化保证；`useCodexConfigState.ts` 的 `mapCodexCatalogModelForForm` 与官方 bearer token 重建（`updateCodexExperimentalBearerToken`）并存；`lib.rs` 无新增命令注册遗漏；本地专有资产（`config.rs`、`services/stream_check.rs`、`ProviderTestStatusContext.tsx`、`lib/utils/mapWithConcurrency.ts`、`.github/workflows/windows-manual-build.yml`、`.gitignore`）未被触碰。
  - [x] 本地门禁（WSL 前端）全绿：i18n 四语言键一致（各 2870 键；`zh-TW` 的重复 `oneClickInstall` 为合并前既有形态，与 `b033f563` 一致）、`pnpm typecheck`、`pnpm format:check`、`pnpm test:unit` 142 文件 / 1124 项、`pnpm build:renderer`。
  - [x] **Windows runner 前端单测超时的实测与修复**：首次 CI（run `35423958509`）在 Unit tests 步骤红在 `tests/components/PiProviderForm.test.tsx` 2 项 `Test timed out in 5000ms`。归因证据——该测试文件与组件自 v3.20.0 起一行未改，v3.20.3 只是给 `src/config/piProviderPresets.ts` 加了 783 行预设；本机默认并行复现 4 项超时，`--no-file-parallelism` 串行 1124 项全过，单跑该文件 46 项全过（单项 2~3s），`--maxWorkers=2` 全过；上游 CI 跑在 Linux 所以看不到。结论是上游新数据把重型交互用例推到 5s 默认上限边缘、由 2 核 runner 的资源竞争触发假失败，不是逻辑缺陷。修复：`vitest.config.ts` 放宽 `testTimeout`/`hookTimeout` 到 15s（只抬上限，不动断言，真实挂死仍会失败），本地默认并行复跑 142 文件 / 1124 项全绿。
  - [x] 推送 `upgrade/upstream-v3.20.3` 并触发 Windows Manual Build（`run_checks=true`、`run_rust_tests=true`）；CI 失败继续在升级分支修复，不提前进入实机阶段。**已全绿：run `35432621760`（commit `5e573a88`，39m26s）通过 typecheck / format:check / `pnpm test:unit` / `cargo fmt --check` / `cargo clippy -D warnings` / `cargo test --lib shared_keys`（26 项）/ 全量 `cargo test`（**2887 项通过、6 项 ignored**；上一轮记录写成 2886 是把失败轮次的 passed 数当成了总数）/ Tauri exe 构建，artifact `cc-switch-windows-x64-exe-5e573a8894aa429210e3edaa0fb64f11d0a5de64`。**
  - [x] **CI 迭代过程中实测出的真实缺陷与修复（共 14 轮运次，前 13 轮逐轮暴露、末轮全绿）**：① 测试侧编译错误——`lock_conn!` 内部用 `?`，两个取锁测试必须返回 `Result`，`prepare_codex_provider_live_config` 第二参是 `&str` 而非 `Option<&str>`；② 物化字段落点认知修正——hydrate 会按「当前配置」重算 `shared_key_strategy`（`shared_keys.rs` 的 `shared_key_strategy`），所以落点随卡片形态走，配置无 `ANTHROPIC_AUTH_TOKEN` 时推导为 `anthropic`、落 `ANTHROPIC_API_KEY`，测试此前把池条目的 strategy 字符串当成落点依据；③ 池不做 id 重编号，首次保存沿用卡片条目 id；④ **分类器语义缺陷（影响最大）**：把「已关联分组无 Key（用户清空池）」与「卡片持有 Key 列表但无池关联」当成损坏拒绝，导致 **12 项备份/导入测试**与 v3.8 连续链迁移全部失败——合法备份永远导不进来；现只判结构性矛盾（关联指向不存在的分组、选中 Key 不属于其关联池、marker 与结构互斥），数据层缺项交给认领/修复路径；⑤ `v18_repair_detection_and_backup_gate` 夹具自建池表与 `create_tables` 冲突，改 `IF NOT EXISTS`；⑥ `sync_universal_to_apps_preserves_child_metadata` 两处严格 meta 相等断言改为「继承字段保留 / after 为 before 超集」，因为 DAO 读路径会补池视图字段（数据库行本身不被污染，另用原始 SQL 断言守住）。
  - [x] **外部审核（GPT）复核后的两处数据门禁漏项修复（本轮）**：审核认定「合并与主体适配已完成、CI 全绿」但「数据安全门禁未全部落实」，逐条取证后确认两条成立并已修复。① **池结构矛盾检查晚于建表**：`create_tables_on_conn` 在建表阶段就创建池表（本地既有设计，commit `ee72045e` 引入），而 `ensure_v18_pool_marker_consistent_with_raw_state` 只在迁移阶段被调用——补出的空表让「marker 已完成却缺池表」永久不可见，`Database::init` 的只读门禁也没做这项判定。现已在 `init` 的只读块（`create_tables` 之前）同时调用该矛盾检查与新增的 `ensure_core_tables_before_write`，两条门禁都在任何建表之前生效并直接拒绝启动。② **缺核心表的存量库无备份被补建**：`schema_needs_v18_repair` 对「有用户表但缺 `providers`」直接返回 `false`（不触发备份），后续 `create_tables` 又把 `providers` 补成空表，用户丢光供应商配置却看到「一切正常」。现由 `ensure_core_tables_before_write` 拒绝，并区分「全新空库（一张表都没有）放行」与「存量残缺库拒绝」。③ 同类遗漏一并修掉：`version=0` 但已有用户表会被迁移链从 v0 逐级改写，却因 `needs_migration` 要求 `version > 0` 而漏掉备份——抽成 `needs_migration_backup` 纯函数后覆盖（导入/恢复路径本就由 `validate_imported_schema` 守 7 张必需表，缺的只有启动路径）。
  - [x] **同轮修正：分类器不再把「已完成标记 + 空池」当成待初始化**（审核未点名、属同类风险）：原实现对空池无条件返回 `LegacyUninitialized`，会在「池表被外部删空/用户清空且标记仍在」时触发一次完整重迁，把卡片残留 `meta.apiKeys` 复活进池。现改为「有 Done 标记 → `CompleteWithMarker`（什么都不做）、无标记 → `LegacyUninitialized`（可安全初始化）」，并新增回归 `v18_repair_keeps_emptied_pool_with_marker_instead_of_reseeding`（断言既有 `schema_needs_v18_repair` 为 false、迁移后池仍为 0 行）。
  - [x] **同轮修正：移除物化层未经计划约定的「取池中第一条 Key」**（审核问题 3，成立）：选中 id 解析不到条目时，物化会回退到 `meta.api_keys.first()`，而代理路径（`resolve_selected_key` 的调用方）在同样情况下退回卡片配置——同一张卡的直连写入与代理转发会落到不同账户的凭据上。现已恢复「解析失败即不物化」的既有语义，与代理路径一致；失效选中的修正只由 hydrate 的同值重映射负责。
  - [x] **同轮修正：service 层回归测试重构为真实跨应用合并**（审核问题 4，成立）：原 `shared_keys_save_normalizes_selection_before_live_write` 两张卡都是 Claude、域名不同，既没建立跨应用共享也没验证重映射，且输入配置本就带着期望的 Key 值，物化即使不生效也能通过。现已改为「Codex 卡先占同域名池 → Claude 卡提交同值不同 id」，断言：两卡链接到同一池分组、前端 id 被重映射到池中同值条目（`assert_ne!` + 回查 `key_value`）、物化结果为池 Key 而非配置里的旧值，并用「请求快照物化只能拿到旧值」反证 `reload_provider_after_save` 的必要性。
  - [x] **审核修复后的复核结果：CI 全绿（run `35689021744`，commit `c90514ed`，26m03s）**，artifact `cc-switch-windows-x64-exe-c90514edef4b6e1f9f97ffb3484833440de6e3a3`；门禁顺序全部通过，`cargo test --lib shared_keys` 26 项、lib 单测 **2891 项通过 / 6 项 ignored**（较上一轮 2887 增加 4 项，正是本轮新增的 4 个门禁与分类器回归）。中间 4 轮失败均为可追溯的真实信号，不是重复踩坑：`35685984515` rustfmt 要求 `expect_err` 单行、`35686577977` 闭包内 `lock_conn!` 需返回 `Result`、`35687344761` 夹具缺顶层 `model_provider` 导致 `extract_codex_base_url` 取不到域名、`35688071328` 我原先写的「反证 reload 必要性」断言与实测行为不符（见下条）。
  - [x] **同轮修正：物化与 hydrate 两条防线的真实分工（对上一轮认知的更正）**：CI 实测证明 `hydrate_shared_keys_for_provider_in_db` 的「同值按 Key 找回池条目」重映射**并非防御性死代码**——前端原始请求对象（selected 仍指向未进池的 id）走 `build_effective_settings_with_common_config` 时，正是这道重映射把它找回池条目并物化出正确的池 Key。测试应先前的错误断言改为分别固定两条事实：① 请求快照能经 hydrate 物化出池 Key；② 请求快照自身的 `meta.selectedKeyId` 仍是未归一化的请求 id，这正是 add/update 之后必须 `reload_provider_after_save` 的理由——不经 live 构建的消费路径（备份写入、代理读写、前端回读）不会替调用点做归一化。
  - [x] **第二轮审核（GPT）复核后补齐导入/恢复门禁与落盘断言（本轮）**：审核判定「启动门禁与凭据回退可认可，导入/恢复门禁仍需补齐」，逐条取证后确认成立并已修。① **导入与恢复的 staging 路径同样漏了原始池结构检查**（审核问题 1，成立）：`import_sql_string`（`backup.rs:220` 校验 → `:223` 建表）与 `restore_from_backup_with_hook`（`:1081` 校验 → `:1083` 建表）都先跑 `validate_imported_schema`，而它只检查七张基础表，不检查共享池与 marker；一份「七张表齐全 + 完成标记 + 池表整组缺失」的备份因此通过校验，被 `create_tables_on_conn` 补出空池表后再也看不到矛盾（`apply_schema_migrations_on_conn` 内的检查此时已失效），最终被当成正常备份换进主库。现两条 staging 路径都在建表之前调用 `ensure_v18_pool_marker_consistent_with_raw_state`。该函数对缺表有 `table_exists` 保护（`shared_key_pool_has_core_rows` 逐表探测），因此官方 v17/v18 备份（无池表、无标记、无残留数据）照旧可导入并由迁移建表——这一点专门加了防误伤回归，避免门禁把官方老备份挡在门外。② **落盘断言补齐**（审核问题 3，成立）：新增断言读取 service 实际写出的 `~/.claude/settings.json`。取证过程中的一个事实更正：Claude 属于非 additive 应用，`add_to_live` 参数对它**不生效**（那条早退只作用于 OpenCode/OpenClaw 等 additive 应用），只要库里没有当前供应商，`add` 就会设其为当前并走 `write_live_snapshot` 落盘，所以原测试其实一直在走落盘路径，缺的只是断言文件内容——现已断言落盘文件的 `env.ANTHROPIC_AUTH_TOKEN` 等于池 Key。
  - [x] **本轮 CI 结果：全绿（run `35699208370`，commit `0e85585d`，23m31s）**，artifact `cc-switch-windows-x64-exe-0e85585dacf0b8d2afbbdc94874fb539824e0d77`；前端 1124 项通过、共享 Key 专项 26 项、lib 单测 **2894 项通过 / 6 项 ignored**（较上轮 2891 增加 3 项，正是新增的导入拒绝、导入防误伤、恢复拒绝三个回归）。中间 6 轮失败全是可追溯的编译/格式细节，无一是设计返工：rustfmt 对 `let` 断行与 `expect_err` 单行的偏好（2 轮）、闭包内 `lock_conn!` 需返回 `Result`、夹具缺顶层 `model_provider` 导致 `extract_codex_base_url` 取不到域名、`backup.rs` 测试模块只导入具名项（非 glob）导致 `fs::` 与 `get_app_config_dir` 无法解析（2 轮）。
  - [x] **本轮终点为 CI 通过即止**：不安装、不替换本机 exe、不触碰 `/mnt/c/Users/yun/.cc-switch`；实机升级、共享 Key 全链路回归、合并回私有 `main`、更新安装另行安排。
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

