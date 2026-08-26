# 后续计划（C 组收尾与长期挂账）

> 活跃计划 — 摘自 `docs/archive/review-2026-08-24.md` 的 C 组（2026-08-26 增补）。R/F/G/A/B 组已在 `docs/changelog.md` 第十一~十三轮落地，历史详见归档。
> 验证块：`cargo clippy --all-targets --features no-embed -D warnings` / `cargo test` 双 feature / `uv run pytest` / `npm run build`。

## 批次五：A-4 孤儿清理降频 + 防护（S）

决策：Toolhelp32 替换 PowerShell 已否决（快照拿不到 CommandLine，误杀风险），改为降频 + 超时防护（见归档原文）。

## 批次六：S 级小尾巴五件

见归档原文批次六 5 项（`PerProbeDetail::new`、scripts 单次读盘、`SchedulerApi::history_dir` 删除、`parse_host_port("::1")`、IconApp 收敛）。

## 批次七：A-5 system.rs 按域拆分（M）

见归档原文批次七（`routes/background.rs` / `routes/uninstall.rs`，关闭 `state.container` 旁路）。

## 批次八：B3 根治（M）

见归档原文批次八（调试会话存活期纳入槽位，`handle_idle_timeout` 存活期跳过）。

## 长期挂账（2026-08-26 核实修订）

- M1 trait 化已完成；Pinia 不适用（前端无 Pinia）；utoipa 缓办；`AppState.container` 后续可评估移除；5.0.0-alpha.2 暂缓
- 详见 `docs/archive/review-2026-08-24.md` 末节与 `docs/changelog.md` 第十三轮

> 本文件为活跃入口，完成一项后在 `docs/changelog.md` 归档并更新 `docs/known-issues.md`。
