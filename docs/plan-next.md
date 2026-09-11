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
- **强烈建议（P1，约 12 项）**：`wait` 三方矛盾、`timeout` 钳制、`evaluate` 关页、别名漂移、`select` 空值、LLM `finish_reason`/`max_tokens`/重试、更新后 `uv sync`、错误映射 500/404、`shell` 类型静默失效、Esc 脏状态、前端保存防连点与 `validateConfig`/`cron` 校验。
- **更新子系统（4 项 P2 + 3 项 P3）**：A 正确性（base 分离与 helper 落点）；B 体验（停滞超时与配额 403/429）；C 接口（pin 版本闸门）；D 清理（double-fire 与 `available` 语义）。
- 已对齐项：`VALID_STEP_TYPES` 含 `evaluate`/`custom`（本轮已对齐 `task-writing-guide`）、`UpdateChannel` 三通道与 `update/last_check.json`（`check.rs`/`mod.rs` 已实现，本轮补文档）、CI 含 `e2e-login-chain` + `rust-tests-unix`。

## 长期挂账（2026-08-26 核实修订）

- M1 trait 化已完成；Pinia 不适用（前端无 Pinia）；utoipa 手写 `openapi.json` 仍为前端 `typegen` 数据源，待 `utoipa` 宏化后自动生成；
- `AppState.container` 后续可评估移除。

> 本文件为唯一活跃计划入口，完成一项后在 `docs/changelog.md` 归档并更新 `docs/known-issues.md`。
