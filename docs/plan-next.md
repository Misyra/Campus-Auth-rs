# 后续计划（活跃入口）

> R/F/G/A/B 组已在 `docs/changelog.md` 第十一~十六轮落地并归档，历史详见 `docs/archive/`。
> 当前活跃：`v5.0.0-alpha.10+`（`docs/changelog.md`）。P0/P1 与更新子系统待办的权威摘要以本文件与 `docs/known-issues.md` 为准（原 defect-recheck / updater-audit 过程报告已于 2026-09-12 删除）。
> 验证块：`cargo clippy --all-targets -- -D warnings` / `cargo test` 双 feature（含 `rust-tests-unix`）/ `cargo fmt --check` / `uv run pytest` / `npm run build` + `vitest` / `e2e-login-chain` 全链路。

## C 组收尾（2026-08-26 增补，仍为活跃入口）

> 摘自 `docs/archive/review-2026-08-24.md` 的 C 组，完成一项后在 `docs/changelog.md` 归档并更新 `docs/known-issues.md`。

### 批次五：A-4 孤儿清理降频 + 防护（S）

决策：Toolhelp32 替换 PowerShell 已否决（快照拿不到 CommandLine，误杀风险），改为降频 + 超时防护（见归档原文）。

### 批次六：S 级小尾巴五件

见归档原文批次六 5 项（`PerProbeDetail::new`、scripts 单次读盘、`SchedulerApi::history_dir` 删除、`parse_host_port("::1")`、IconApp 收敛）。

### 批次七：A-5 system.rs 按域拆分（M）

见归档原文批次七（`routes/background.rs` / `routes/uninstall.rs`，关闭 `state.container` 旁路）。

### 批次八：B3 根治（M）

见归档原文批次八（调试会话存活期纳入槽位，`handle_idle_timeout` 存活期跳过）。

## alpha.8 之后（待排期，按复核优先级）

- **必修（P0，9 条）**：Bridge 槽位/取消注册表、录制器多 frame、任务创建 `..Default` 空 steps 等（原 76 条 v2 对质结论，报告已删）。
- **强烈建议（P1，约 12 项）**：`wait` 三方矛盾、`timeout` 钳制、`evaluate` 关页、别名漂移、`select` 空值、LLM `finish_reason`/`max_tokens`/重试、更新后 `uv sync`、错误映射 500/404、Esc 脏状态、前端保存防连点与 `validateConfig`/`cron` 校验。（`shell` 类型静默失效已解决：`type=shell` 现于反序列化明确报错并提示改用 `script`，见 `src/tasks/models.rs`。）
- **更新子系统（4 项 P2 + 3 项 P3）**：A 正确性（base 分离与 helper 落点）；B 体验（停滞超时与配额 403/429）；C 接口（pin 版本闸门）；D 清理（double-fire 与 `available` 语义）。
- 已对齐项：`VALID_STEP_TYPES` 含 `evaluate`/`custom`（本轮已对齐 `task-writing-guide`）、`UpdateChannel` 三通道与 `update/last_check.json`（`check.rs`/`mod.rs` 已实现，本轮补文档）、CI 含 `e2e-login-chain` + `rust-tests-unix`。

## 2026-09-15 全仓审查后的剩余项（本轮已修 4 P1 + 8 P2 + 3 P3，见 `docs/changelog.md`）

未修项及理由（均为**已确认但本轮有意不改**，非遗漏）：

- **`openapi.json` 无字段级契约**（原报告 P2-4，含 P2-8 的各类表现面）：spec 有 85 个路径但 `components.schemas` 为 0，各响应的 `application/json.schema` 全为 `{}`；`openapi_json_matches_route_table` 归一后**只比对「方法 + 路径」集合**，故字段名/类型、状态码、媒体类型、路径参数四类漂移无任何自动拦截。已抽查确认**当前一致**（`AssessmentReason` 10 变体、`AssessmentConfidence` 3 变体、`RecoveryAdvice`、`LocalLinkState`、`strict_login_mode` 默认值两侧一致）。属已书面化的取舍（待 `utoipa` 宏化），本轮不改为不引入半成套的生成链路。**廉价替代方案**：补一个「Rust 侧序列化全部枚举变体 → 与 `types.ts` 文本比对」的双向一致性测试，因枚举漂移最易发生且前端 `switch` 有 `default` 兜底、不会报错、只会静默显示"待确认"。
- **抢占总在取消分支不等旧会话收尾**（原报告 P2-11）：`src/login/mod.rs` 的取消分支 `return self.cancelled_handle(...)` 使已 `take()` 出的 `old` 被 drop，`wait_old_session_finished` 不执行。已核对：`submit_gate` 在整个 `submit` 期间持有，故**不会**造成空窗插队；代码注释表明"等待期间点取消即放弃本次提交"是**有意设计**。若需收紧，可让取消分支同样等 `old.finished`（代价：点取消后最多再等 18s）。
- **`useConfig` 保存期间的编辑被折进快照**（原报告 P3）：`:212-235` 构造载荷 → `await patch` → 用**当前** `config` 写 `savedSnapshot`；PATCH 在途期间的编辑会被当作已保存基准并置 `dirty=false`，实际未提交。修法明确（在 `await` **之前**取快照），本轮未做——需同步调整既有 4 个 dirty 快照用例，改动面超出本轮范围。
- **`TrayAction::Quit` 无退出 watchdog**（原报告 P3）：托盘退出只做 `deps.shutdown.cancel()` + dispatch，而 Web 退出与定时自重启都有 `spawn_exit_watchdog`。属不一致；原报告中"托盘线程可能无界阻塞 `join`"已被证伪（Windows 循环最多 50ms 回到循环头，无 `TrackPopupMenu`），故风险仅为"未来若托盘线程引入阻塞逻辑则无兜底"。
- **`docs/known-issues.md` 的 #22 注（21 项 P3 挂账）与 MON-4-R1/R2/R3 未逐条复核**：本轮只确认其中 3 条（#2 / #7 / W13）已过时并已标注，**其余状态未知**，建议整份与当前代码对质。
- **未覆盖的验证面**：Unix 平台未实跑（CI `rust-tests-unix` 覆盖）；未做真实校园网门户验证（需实际门户）；未做人工渗透测试（安全结论基于代码审查与既有负向用例）；`updater::apply_pending_on_startup` 是否响应 shutdown、`client.ts` 的 `ensureAuthToken` 占用超时预算、`useScheduledTasks` 列表写入无 epoch 三项存疑未验证。

> 另：原报告将 P1-4 归因于「登录收尾的 `close_browser` 关掉定时任务的浏览器」，经复核**该机制不成立**（Python `_serve` 是严格串行命令循环，`close_browser` 必须排队），真实破坏点是 `force_recycle` 无条件强杀 —— 本轮按后者修复。记录以免后来者按错误机制重复排查。

## 长期挂账（2026-08-26 核实修订）

- M1 trait 化已完成；Pinia 不适用（前端无 Pinia）；utoipa 手写 `openapi.json` 仍为前端 `typegen` 数据源，待 `utoipa` 宏化后自动生成；
- `AppState.container` 后续可评估移除。
- 定时任务「启动后执行」增强（2026-09-12，已上线固定延迟方案）：可选增强为等待监测服务首次网络连通后再触发（接 StatusManager watch，超时兜底强制执行），彻底消除"网络未就绪烧掉一次尝试"的场景。

> 本文件为唯一活跃计划入口，完成一项后在 `docs/changelog.md` 归档并更新 `docs/known-issues.md`。
