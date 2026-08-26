# 优化执行计划（设计型修复 + 剩余重构）[已归档 2026-08-27]

> 已归档 — 本文件的 A~C 组计划已被 `docs/archive/review-2026-08-24.md` 重审计并落地（第十一~十三轮），剩余 C 组收敛至 `docs/plan-next.md`。原路径 `docs/optimization-plan.md` 已替换为 stub。
> 归档前为 2026-08-16 全库审计产出。死代码清理（-1615 行）已完成并提交（2ab9346 / 363b757 / 40e96ef），本文件是**剩余工作**的执行计划，供执行模型使用。
> 项目根：`e:\Campus-Auth-rs`。验证命令：Rust `cargo test`；Python `cd python_worker && uv run pytest -q`；前端 `cd frontend && npm run build`。
> 通用规则：改哪层跑哪层的验证；每完成一个任务卡提交一次 commit（中文 conventional 风格）；行号基于 2026-08-16 快照，可能漂移，以内容定位为准。

---

## A 组：高影响修复（先做，每个任务卡独立可验收）

### A1. Worker 挂起死锁自愈（最高优先级）

**问题**：三个缺陷叠加成永久死锁，无自愈路径：
1. `python_worker/step_handlers.py` `handle_evaluate` 的 `page.evaluate(script)` 无超时，一段 `while(true){}` JS 即永久挂起；
2. `python_worker/worker_main.py` `_serve` 主循环 `await _dispatch(msg)` 严格串行，一个挂起操作堵死后续所有命令；
3. 取消是纯协作式（`_check_cancel` 仅步骤边界生效），而 Rust 侧 300s 超时只发 Cancel 通知并返回 `BridgeError::Timeout`，**不回收进程**（`src/bridge/mod.rs` 约 243-252 行）。

**方案**（Python 侧为主）：
- `_serve` 循环中把每条命令包成独立 `asyncio.Task`，用 `asyncio.wait_for(task, timeout=CMD_TIMEOUT)` 兜底，超时则 `task.cancel()` 并调用 `page.close()`（强制中断挂起的 Playwright await），然后按 `Outcome.UNKNOWN_ERROR` 回错误响应。
- `handle_evaluate` 单独加 `asyncio.wait_for(page.evaluate(...), context.default_timeout / 1000)`。
- `_serve` 退出路径 `close_browser()` 包 `asyncio.wait_for(..., 5.0)`。
- Rust 侧：`bridge/mod.rs` 的 300s 总超时分支，在发 Cancel 后增加一个宽限（如 10s）等待 worker 自行退出，仍未退出则调用现有的 `kill_worker_now` 强杀回收。
- 命令超时值从 Rust 下发的 `BrowserSettings` 推导（复用 `_to_ms` 语义），不新增协议字段。

**验收**：
- 新增 pytest：用 monkeypatch 让某 handler 永久 sleep，验证命令超时后 worker 能继续处理下一条命令（不死锁）。
- 新增 pytest：`handle_evaluate` 挂起脚本被超时中断。
- 现有 47 个测试全过；`cargo test` 全过（bridge 相关 4 个集成测试重点看）。

### A2. ConfigService 阻塞 I/O 移出 async 热路径

**问题**：`src/config/service.rs` `load_settings`（约 257-292 行）/ `load_profile`（约 314-335 行）在 async 上下文直接做 `std::fs::metadata` / `read_to_string`。Engine 每 5s 探测都调它；`reload_inner`（约 413-431 行）还持着 tokio Mutex 做这些阻塞 I/O，饿死并发保存。慢盘/杀软扫描时阻塞整个运行时。

**方案**（二选一，推荐前者）：
- 方案一：Engine 的 `run_loop.rs`（约 131、293 行）与 `login/mod.rs:215`、`web/routes/system.rs:20` 改读 `config.runtime().load()`（ArcSwap 无锁快照），不再每次走磁盘 mtime 校验。`load_settings` 保留给「确实要强制重读磁盘」的调用方（reload 链路），其磁盘 I/O 包 `tokio::task::spawn_blocking`。
- 方案二：仅把 `load_settings`/`load_profile` 的 fs 操作包 `spawn_blocking`（改动小但热路径仍有系统调用开销）。

**验收**：`cargo test` 全过；grep 确认 async fn 直接调用链上无裸 `std::fs::read_to_string`（config 模块内）；现有行为不变（config round-trip 测试覆盖）。

### A3. shutdown token 统一

**问题**：两个独立 `CancellationToken`：`container.uptime_cancel`（container.rs:133，一身三职：uptime 定时器 + 登录 shutdown + Drop 清理）与 LauncherState token（launcher.rs:179）。靠 `graceful_shutdown` 手动双取消（launcher.rs:1005、1039）维持正确，新增关闭路径易漏。

**方案**：
- LauncherState 创建自己的 token 后，传给 `ServiceContainer::new`，容器内用 `shutdown_token.child_token()` 派生登录 shutdown 与 uptime 各自的 child。`cancel_shutdown()` 与容器 Drop 的双取消即可删除（父 token cancel 自动传播）。
- 注意保持现有启动顺序与语义：`graceful_shutdown` 中 `container.cancel_shutdown()` 调用点删除后，确认登录会话仍能在 Bridge 关闭前收到取消（历史遗留 #8 的修复不能回退）。

**验收**：`cargo test` 全过；手动冒烟：`shutdown_app` 后进程在 30s watchdog 内干净退出、无 worker 残留（任务管理器确认）。

### A4. uninstall / restart watchdog 统一

**问题**：`src/web/routes/system.rs` `uninstall`（约 513-516 行）2s 后 `exit(0)`，短于优雅关闭总预算（Tray 3s + Scheduler 5s + Engine 5s + Bridge 8s + Axum 5s ≈ 26s），卸载时必被强杀留残留；`restart_app`（约 50-73 行）干脆没有 watchdog。

**方案**：提取 `spawn_exit_watchdog(secs)` 公共函数（30s），`shutdown_app` / `restart_app` / `uninstall` 三处共用。

**验收**：`cargo test` 全过；grep 三处路由均使用同一函数。

---

## B 组：中影响修复

### B1. 前端重连/加载的数据保护三件套（frontend/src/composables）

- **F1**：`useUi.ts` init 的 WS 重连回调（约 407-415 行）调 `config.fetchConfig()` 前判 `dirty.value`，dirty 时跳过（对齐 `useProfiles.refreshActiveProfileConfig` 的守卫策略）。
- **F2**：`useConfig.ts` `fetchConfig` catch（约 87-89 行）加 `toastOnly(false, "加载配置失败")`，并加 `configLoadFailed` ref，为 true 时 SettingsView 保存按钮禁用 + 顶部提示重试。
- **F3**：统一只读列表失败反馈：`useTasks.fetchTasks` / `useProfiles.fetchProfiles` / `useUi.fetchBrowsers` 首次失败加 toast（参照 `useStatus.fetchStatus` 的首败 notify 模式）；其余保留 log-only。
- **F8 顺带**：重连回调的 `Promise.allSettled` 补 `scripts.fetchScripts()`、`tasks.fetchPureMode()`、`fetchLoginHistory()`。
- **F9 顺带**：`useUi.init` 的轮询 setInterval 保存 id，`quitApp` 时 clearInterval。

**验收**：`npm run build` 零错误；手动：断开后端重连，设置页未保存编辑不丢。

### B2. cancel_current 改可等待锁

**问题**：`src/login/mod.rs` `cancel_current`（约 402-417 行）用 `try_lock`，撞上 `submit` 持锁窗口时用户取消被静默丢弃（点取消没反应）。

**方案**：改为 `async fn` + `lock().await`（锁窗口极短）；Web handler 与 Engine 调用点加 `.await`。

**验收**：`cargo test` 全过；新增单测：持锁状态下调用 cancel 能等到锁而非放弃。

### B3. Worker 连续启动失败熔断

**问题**：`src/bridge/mod.rs` `ensure_worker`（约 709-773 行）无熔断。Python 环境损坏时每次登录都 spawn → 30s 健康检查超时 → kill，重试 3 次等 90s。

**方案**：`BridgeInner` 加 `consecutive_spawn_failures: u32`；连续 ≥3 次后 `worker_state` 置 `Error` 且 `ensure_worker` 直接快速失败（不再 spawn），返回带「环境异常，请重新引导」语义的错误；EnvironmentManager 成功重建环境后复位计数（在 environment 引导成功路径调用 bridge 的复位方法）。

**验收**：新增集成测试模拟 spawn 连续失败，验证第 4 次调用快速返回错误而非再等 30s。

### B4. Worker 错误分类细化（python_worker）

- **P3**：`step_handlers.py` `_safe_op`（约 159-164 行）增加对非超时 `playwright.async_api.Error` 的捕获，按语境映射 `SELECTOR_FAILED`（可重试、不回收 worker），避免瞬时元素问题升格 UNKNOWN_ERROR 触发 worker 强制回收。
- **P7**：`handle_navigate` / `playwright_worker.py` `_navigate` 按异常消息细分：含 `ERR_CONNECTION_TIMED_OUT`/`ERR_NAME_NOT_RESOLVED` 等连接级错误 → `NETWORK_ERROR`（此时回收无意义，可降级处理）；Playwright 自身 TimeoutError → `NAVIGATION_TIMEOUT`。
- **P6**：`worker_main.py` `_error_result`（约 187-189 行）补 `outcome: "unknown_error"` 字段，保证 IPC 响应结构一致。

**验收**：pytest 全过；新增分类单测（构造不同异常消息断言 outcome）。

### B5. Worker 任务间脏状态防护（python_worker/playwright_worker.py）

- 注册 `page.on("dialog", lambda d: asyncio.ensure_future(d.dismiss()))`（约 416-427 行 context 配置处），防残留 alert 卡死后续导航。
- `_run_task` 任务失败（非重试同任务）后重建 page 或至少 `context.clear_cookies()`——按现有 context 复用结构选影响最小的方案，并在注释说明取舍。

**验收**：pytest 全过；新增单测：dialog 事件触发时 dismiss 被调用（mock page）。

### B6. status 双源竞态（frontend）

**问题**：`useUi.ts`（约 420-424 行）30s 轮询与 WS `updateStatus`（useWebSocket.ts:123-125）都 `Object.assign(status,...)`，过期轮询响应会短暂回退状态。

**方案**：`useStatus.ts` 的 status 加 `epoch`（单调递增，WS 推送与轮询响应各带来源序号/时间戳），`mapBackendStatus` 时 epoch 旧的直接丢弃。后端若已有 timestamp 字段可直接用；没有则前端维护本地计数器（WS 优先级高于轮询）。

**验收**：`npm run build`；手动验证状态面板不再闪断。

### B7. useDebug 进行中步骤误显失败（frontend/src/composables/useDebug.ts）

- **F5**：`handleStepProgress`（约 132-142 行）不再设 `success=false`，引入 `running` 标记；`getStepStatus`（约 106-111 行）加 running 分支（显示进行中样式）。
- **F6**：`_resultMap` 两个分支统一用 `_resultMap.value = new Map(_resultMap.value)` 重建引用，消除对 `current_step` 副作用的隐式依赖。
- `DebugPanel.vue` 步骤状态渲染加 running 态样式（转圈或灰色）。

**验收**：`npm run build`；手动跑 debug 会话，进行中步骤不再显示红 ✗。

---

## C 组：机械重构（无设计，按图施工）

### C1. zip 解压三处去重（约 -50 行）

`src/environment/git.rs::extract_mingit_zip`（约 210-253 行）、`src/environment/uv.rs::extract_uv_from_zip`（约 366-400 行）、`src/updater/download.rs::extract_to_staging_blocking`（约 143-200 行）是同一模板。抽 `utils::io::extract_zip(zip_path: &Path, dest: &Path, accept: impl Fn(&Path) -> bool) -> Result<()>`，三处保留各自过滤/strip 逻辑。**注意 updater 版有 staging 校验逻辑，别丢**。验收：`cargo test` 全过。

### C2. 流式下载两处去重（约 -30 行）

`git.rs::download_file`（约 175-207 行）与 `uv.rs::download_file_streaming`（约 294-333 行）合并为 `utils::http` 或 `utils::io::download_streaming(client, url, dest)`。验收：`cargo test` 全过（环境引导集成测试覆盖）。

### C3. atomic_write_json 三套收敛 + F_FULLFSYNC 合并（约 -32 行）

`config/service.rs::atomic_write_json_sync`（约 554-575 行）删除；`utils::io` 增加 `atomic_write_bytes(path, bytes)` raw 版本；macOS `F_FULLFSYNC` 块（service.rs:592-600 与 io.rs:31-39 重复）合并为 `utils::io::fsync_full(file)` 单实现。验收：`cargo test` 全过（config round-trip 测试覆盖持久化语义）。

### C4. LoginSession 重试样板收敛（约 -30 行）

`src/login/session.rs::run` 内 5 处 `emit(make_cancelled_result)+return` 与 4 处 `emit(make_result)+return` 抽成 `finish_with_cancelled` / `finish_with_failure` 两个 `-> !` 辅助方法。验收：`cargo test` 全过，登录相关测试重点看。

### C5. TcpProbe race 循环收缩（约 -5 行）

`src/monitor/probes.rs`（约 106-116 行）手写「首个成功即返回」改用 `futures::future::select_ok`。验收：`cargo test` 全过。

### C6. 零散小项（可合并一个 commit）

- `src/web/routes/debug.rs::start_debug`（约 20-28 行）与 `login/mod.rs::build_worker_config`（约 526-538 行）的 task_config 嵌入逻辑提取公共方法。
- `src/launcher.rs` `watch_engine` 重启延迟（5s）期间监听 shutdown token，避免 `latest_engine_cmd_tx` 在 shutdown 时指向旧 sender（L5）。
- `src/login/mod.rs::immediate_handle`（约 477-481 行）的历史写入改为可等待或 `graceful_shutdown` 前 flush（L2）。
- `python_worker/worker_main.py` stdin 单行加 16MB 上限，超限丢弃并告警（P9）。
- `python_worker/playwright_worker.py` `run_steps` 把非必须步骤失败摘要累积进最终 `StructuredResult.message`（P12）。
- `python_worker/models.py` 给 `CAPTCHA_FAILED/INVALID_CREDENTIAL` 加 docstring 注明「由 Rust 侧设置，worker 不产生」（P10）。
- `python_worker/step_handlers.py` `handle_assert_text` 超时映射改 `ASSERTION_FAILED`——**需同步 Rust `src/bridge/ipc.rs` 的 Outcome 枚举**，若 Rust 侧有按 outcome 决策重试/回收的 match 也要同步（P11）。
- `frontend/src/composables/useLogs.ts` 高频日志微任务批量合并（F7）。
- 移除 `frontend/package.json` 的 `openapi-typescript` devDep 与 typegen 脚本（当前 openapi.json 为空 schema，产出为零；待 utoipa 落地再装回）。

---

## 执行顺序建议

A1 → A3 → A4 → A2（A 组全部）→ B1..B7（按序）→ C1..C6（可并行/合并）。每个任务卡独立提交，commit message 注明任务卡编号（如 `fix: A1 Worker 挂起死锁自愈`）。
