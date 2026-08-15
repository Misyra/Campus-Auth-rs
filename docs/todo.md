# 待办清单（TODO）

> 概略规划，不锚定具体行号（以文件 + 函数/符号定位，代码会变）。来源：`docs/known-issues.md` 有效条目 + 工程化短板 + 2026-08-15 全面审查。
> 完成后在 `docs/changelog.md` 归档，并从 `docs/known-issues.md` 移除对应条目。

## 批次一：死代码激活（延续"修复而非删除"原则）

- [x] `PartialSnapshot::Uptime` 变体接线：`uptime_seconds` 恒为 0，状态快照与 `/api/system` 不一致
- [x] `ConfigReloadSignal::TasksChanged` / `ProfileSwitched` 死变体：启用信号去重，避免每次切 Profile 全量重载任务
- [x] Bridge `last_activity` 只写不读（4 处写入）：接入实际用途（如空闲回收/状态展示）或标注
- [x] `PasswordCrypto::decrypt`（非 zeroizing）仅测试使用：标注或收敛到测试专用

## 批次二：行为类修复（需谨慎验证）

- [x] 关闭序列：发 Shutdown 后 await Engine 退出再关 Bridge，取消在途登录 task
- [x] 双层 CORS 叠加：外层硬编码 50721 白名单挡住 vite dev / 局域网来源，合并为一层
- [x] `GET /api/profiles` 列表泄露密码密文字段：与 `get_profile` 清空行为对齐（已证实 `list_profiles` 返回无密码的 `ProfileSummary`，无需改）
- [x] URL 探测全量下载 body 才截断 64KB：改 `take(64K)` 流式读取
- [x] `CaptchaFailed` 硬编码终态：改为重试整个流程（OCR 失败与瞬时失败难区分）
- [x] 托盘状态变化节流去重；reload 后 fsync（configured）；Axum 关闭超时 abort；scheduler 同步 fs 迁移；launcher unwrap 防御

## 批次三：工程化收尾

- [x] E1 openapi.json 生产可用：rust-embed 嵌入，前端兜底 fetch 拿到 SPA index → version="unknown" 死路径消除
- [x] E2 根目录 README.md（用户/贡献者入口）

## 批次四：测试补强

- [x] web/routes handler 层测试（新增路由/鉴权分支）
- [x] tray 菜单逻辑纯函数测试
- [x] environment/ 模块测试
- [x] python_worker 补 pytest（SDK 解析、command 序列化）

---

# 2026-08-15 全面审查修复（进行中）

> 来源：QoderWork 全面审查（P0×3 / P1×21 / P2 若干），所有条目已逐条对照源码验证并剔除误报
> （例："方案保存清空密码"为误报——Web PUT 最终走 `ProfileService::update_profile` 的 `save_password` 空串保留语义）。
> 进度：第一~三批已修复并随 `00f9ea8` 提交；批次五~七已全部修复，批次八全量验证通过。
> **2026-08-15 本批全部完成**（批次五 Rust 后端 + Python Worker、批次六前端、批次七 Rust 清理 + Web 杂项 + 后端杂项、批次八验证），详见 `docs/changelog.md`。
> 建议执行顺序：五（后端）→ 六（前端）→ 七（清理），每批完成后跑一遍下方验证命令块。

## 全局验证命令（每批完成后必跑）

```bash
cargo clippy --all-targets -- -D warnings        # 必须零警告
cargo test                                        # 默认 feature（需 frontend/dist 存在）
cargo test --features no-embed                    # 跳过前端嵌入的快速路径
cd frontend && npm run build                      # vue-tsc 类型检查 + 构建
python_worker/.venv/Scripts/python.exe -m pytest python_worker/tests -q
# 注意：系统 python 缺 playwright，必须用项目 venv；当前 41 项
```

## 已完成（随 00f9ea8 提交）

- P0-1 Worker stdio 编码：`worker_main.py` 新增 `_force_utf8_stdio()`（stdin 用 errors=replace）+ `process.rs` spawn 注入 `PYTHONUTF8=1`
- P0-2 脚本保存摧毁脚本：前端 payload 补 `type:"script"`；后端 `get_script` 改为 load_task 返回完整字段（name/description/content/binary_path/script_path/args/timeout），`update_script` 前置 type 校验拒绝非 script 负载
- P0-3 路径穿越：`scheduler/mod.rs` delete_task 与 `routes/scheduler.rs` job_history 补 `is_valid_id`；CORS 从 `mirror_request` 收紧为仅本机 Origin（127.0.0.1/localhost/[::1] 任意端口）
- P1-2 托盘退出：TrayDeps/TrayManager 注入 `shutdown: CancellationToken`，Quit 时取消；`graceful_shutdown` 第 0 步取消令牌；`watch_engine` 收到初始 Engine 完成通知后先查令牌，关闭期不再重启
- P1-3 restart 路由：spawn 带 `--restarting`（args_os，防非法 Unicode panic）+ CREATE_NO_WINDOW + 走 shutdown_tx 优雅关闭
- P1-4 startup_action 接线：新增 `apply_startup_action`（launch_full/launch_lightweight 调用）；CLI `--mode` 改 `Option`，缺省读 settings.json `app.runtime_mode`
- P1-5 日志热更新：新增 `SharedTargets`（`Arc<Mutex<Targets>>` + 自定义 `Filter` 实现，`callsite_enabled` 恒 `sometimes` 禁缓存）；初始级别读 `logging.level`
- P1-8 探测误判：HTTP/URL 探测 1xx/4xx/5xx 改判 Pass（收到响应即物理连通），仅超时/连接失败为 Fail
- P1-9 登录结果重复计数：回传 channel 改传 `LoginResult`（带 source）；仅 source=Auto 失败计入冷却；`auto_login_in_flight` 标记去重触发
- P1-12 配置损坏防护：`load_settings` 解析失败沿用缓存或置 poisoned；`save_settings` 隔离态拒绝保存
- P1-13 禁用任务入缓存（`load_and_parse_all` 所有任务进 `loaded`，仅调度表跳过禁用）；P1-14 `save_task` 落盘前 `parse_cron_expr` 校验（400）
- P2-1 冷却到期重置失败计数；P2-2 登录结果回传/pause/resume 按 `engine_state_for` 合并状态；P2-3 `reset_check_timer` 消费 interval 首 tick
- P1-20 Python 非 dict JSON 行 guard（`isinstance(msg, dict)` 拦截，不再杀死 Worker）
- （P1-1 密码泄露经核实在批次二已修复：list_profiles 返回无密码的 ProfileSummary）
- 验证状态：cargo test 双 feature 308 项全过、clippy 零警告、前端构建过、pytest 41 项过（2026-08-15）

## 批次五：Bridge / 更新器 / 环境 / Python Worker（后端）

- [x] 全部完成（2026-08-15）

### 5.1 P1-6 OCR 并发摧毁登录会话槽位

- 位置：`src/bridge/mod.rs` — `check_session_compat`（`method == "ocr_recognize"` 无条件 Ok）、`execute_inner`（临界区 4.4 步覆写 `current_session` / `current_cancel_id` / `current_request_id` 并 abort 空闲计时器、移除旧会话 cancel_id）、`reset_session`（guard drop 时复位 + `start_idle_timer`）
- 根因链：compat 层把 OCR 视为"可与任意会话并发"，但 execute_inner 把它当普通 Login 会话写入**单槽位** → OCR 完成 → 转发 task drop guard → `reset_session` 双重匹配（session+rid）命中 → 状态复位 Idle 并启动空闲计时器。此后在途登录仍在 Worker 内执行，Rust 侧已判 Idle；空闲超时（`worker.keep_alive=false` 时默认 300s）触发 `handle_idle_timeout` 回收 Worker → 登录以 WorkerCrashed 失败。原登录会话的 cancel_id 也被移出 CancelRegistry（本地取消失效）。调试会话同理被提前复位
- 方案：`execute_inner` 对 `ocr_recognize` 走旁路——仍注册 `pending_requests` 与 `cancel_registry`，但**不**触碰 `current_session` / `current_cancel_id` / `current_request_id` / 空闲计时器 / `worker_state`。守卫改用轻量清理变体（enum `Guard { Session(SessionGuard), Lightweight { cancel_id, request_id, weak } }`，或给 SessionGuard 换一个只做 `pending.remove(rid)` + `cancel_registry.remove(cancel_id)` 的 drop 回调，不要复用 reset_session 的匹配逻辑）
- 验证：单测——预置 `current_session = Some(Login)` 后走 execute("ocr_recognize")，断言槽位/worker_state 未变；guard drop 后 pending 与 cancel 注册表已清、current_session 仍为 Login

### 5.2 P1-7 Bridge 调用方超时不清理会话槽位

- 位置：`src/bridge/mod.rs` — `execute_with_timeout`（超时直接返回 `BridgeError::Timeout`，无善后）；supervisor 主循环 Execute 分支 spawn 的转发 task（`select! { rx, token.cancelled() }` 持有 SessionGuard）
- 根因：Worker 存活但挂死时，转发 task 在 rx/token 上永久等待并持有 guard → 槽位与 `pending_requests` 条目永久滞留 → 后续 debug 类请求一律 WorkerBusy、登录类 FIFO 连环超时，只能靠 Worker 崩溃 / force_recycle / 重启解套
- 方案：① `execute_with_timeout` 自生成 cancel_id（params 无则注入 `params["cancel_id"]`，保留副本）；② 超时分支发送 `SupervisorCommand::Cancel { cancel_id }`；③ 检查 supervisor 的 Cancel 处理分支——目前只向 Worker stdin 发 `{"cancel": id}`，需**同时** `cancel_registry.trigger(cancel_id)`（本地 token 立即唤醒转发 task 的 select 分支 → guard drop 释放槽位）。两条取消路径都要幂等
- 验证：单测——注入一个永不响应的 pending 请求，超时后断言 `current_session` 复位、`pending_requests` 为空

### 5.3 P1-10 更新主流程与 helper 交接断裂（每次更新必静默失败）

- 位置：`src/updater/mod.rs` — `apply_update`（doc 注释明确要求"调用方收到 Ok 后应执行优雅关闭"，但无调用方照做）、`spawn_helper`；`src/web/routes/system.rs` — `POST /api/system/update`（只返回"更新已暂存，重启后生效"）；`src/helper_main.rs` — `wait_for_process_exit`（60s 强制继续）+ 失败分支 `cleanup()`（删 pending.json + staging）；`frontend/src/views/AboutView.vue` — `applyUpdate`（拿到消息即止）
- 根因链：无人退出主进程 → helper 等 60s 强制继续 → Windows 覆盖运行中 exe 的 `fs::copy` 必失败 → 失败分支 cleanup 删 pending+staging → 更新彻底丢失，且 `apply_pending_on_startup` 启动兜底也被摧毁（pending.json 已被删）
- 方案（两端都修，防御纵深）：
  1. `helper_main.rs`：wait 超时后**不再强制继续**——报错退出并保留 staging 与 pending.json，把应用机会留给主进程下次启动的 `apply_pending_on_startup`
  2. 前端 `AboutView.applyUpdate` 成功后弹确认"更新已就绪，立即重启应用？"→ 确定 → `systemApi.shutdown()`（优雅关闭，helper 检测到主进程退出即接管替换与重启）
  3. 顺手修 `cleanup()` 路径不一致：当前硬编码 `base/update/staging`，应使用 CLI `--staging` 传入的实际路径
- 验证：本地起假版本源冒烟，或集成测试模拟 pending 存在 + helper 等待超时 → 断言 staging/pending 仍在

### 5.4 P1-11 uv 就绪判定与实际使用不一致（PATH-only 机器引导卡死）

- 位置：`src/environment/bootstrap.rs` — `check_environment`（`uv_ready = 本地 uv.exe 存在 || which::which("uv")`）；`src/environment/uv.rs` — `run_uv_sync`（硬编码 `env_path/uv.exe`，缺失即 Err "uv.exe 不存在，请先下载 uv"）；`src/environment/python.rs` — `install_playwright` 同样硬编码
- 根因：PATH 上有 uv 的机器被判"就绪"跳过下载（bootstrap 阶段 1 只看 `uv_ready`），但阶段 2/3 只认本地路径 → uv sync 必失败且无回退下载
- 方案：新增 helper（如 `uv_exe_path(mgr) -> PathBuf`）：本地存在返回本地路径，否则返回 `PathBuf::from("uv")`（`Command::new` 自动走 PATH 解析）；`run_uv_sync` / `install_playwright` 统一改用。顺带处理 `src/environment/mod.rs` 的死常量 `UV_MIN_VERSION`：在 PATH 回退分支做最低版本校验（`uv --version` 输出解析），或直接删除
- 验证：单测覆盖 helper 返回值两分支；有条件的话在 PATH-only 环境冒烟引导全流程

### 5.5 P1-21 debug_stop/debug_run_all session_id 两端不一致

- 位置：`src/web/routes/debug.rs` — `debug_stop` / `debug_run_all` 发送 `params = Value::Null`；`python_worker/playwright_worker.py` — `handle_debug_run_all` / `handle_debug_stop`（`session_id = params.get("session_id", "")`）
- 根因：Rust 侧从不传 session_id → Python 侧取空串 → `debug_run_all` 抛"调试会话不存在"（该命令 100% 失败）；`debug_stop` 的 `_debug_sessions.pop("")` 得 None → 跳过 `_cleanup_debug_screenshots`（含明文凭据的调试截图永久残留磁盘）与 `cancel_registry.unregister`
- 方案：Python 侧——session_id 为空且 `_debug_sessions` 恰有一个活跃会话时回退到该会话；多于一个时报错（与 Rust 单会话语义对齐）。Rust 侧可保持 `Value::Null` 不动。顺带：`_close_browser` 与 EOF/shutdown 路径也应清理全部调试会话截图
- 验证：pytest——构造单会话断言空 session_id 回退成功、双会话报错

### 5.6 P2-4 Worker 正常退出被记为崩溃

- 位置：`src/bridge/mod.rs` — `handle_worker_exited`（不分 exit_code：`inc_worker_crash` + `worker_state = Error` + 孤儿清理）；`src/bridge/process.rs` — `health_monitor_task` 发 `WorkerExited(code)`
- 根因：空闲回收（`handle_idle_timeout → handle_shutdown`）与 `/api/worker/stop` 的正常退出（exit_code=0）与崩溃走同一定性路径 → `worker_crash_total` 虚增、状态翻 Error、无谓触发孤儿清理
- 方案：`handle_worker_exited` 开头判 `code == 0` → info 日志 + 置 Idle + `merge_worker_status`，跳过 crash 计数/孤儿清理/Error（pending drain 可保留作防御）
- 验证：空闲超时回收后查 `/api/system` 的 `worker_crash_total` 不变、worker 状态非 Error

### 5.7 P2-5 OCR 模型每次调用重新加载（性能，Python 旧版遗留问题重演）

- 位置：`python_worker/playwright_worker.py` — `handle_ocr_recognize`（每次 `ddddocr.DdddOcr(old=...)`）；`python_worker/step_handlers.py` — `handle_ocr`（同样每次 new）
- 根因：每次实例化都重新加载 ONNX 模型（约 0.5-2s + native 资源反复申请释放），登录重试 + 验证码循环场景开销显著
- 方案：模块级缓存 `_ocr_cache: dict[bool, DdddOcr]`（key = old 参数）+ 统一获取函数，两处调用改走缓存；`classification()` 是同步 CPU 推理，包 `asyncio.to_thread` 避免阻塞事件循环（阻塞期间无法 emit 事件/消费取消）
- 验证：pytest 断言两次获取返回同一实例；冒烟看第二次 OCR 耗时显著下降

### 5.8 环境模块 P2 三项

- E2 下载无超时：`src/environment/uv.rs` — `fetch_latest_uv_version` / `download_text` 的 `send().await` 无 timeout（zip 下载有 300s 包裹，这两个没有）；`EnvironmentManager` 的 `http_client` 是 `Client::new()` 无全局超时 → 镜像半开连接时引导永久挂起（取消令牌只在循环边界检查，救不了）。方案：两处包 `tokio::time::timeout` 或给 client 加 `.timeout()`
- E6 Playwright 就绪检查过弱：`bootstrap.rs` `check_playwright_chromium_installed` 只查 `%LOCALAPPDATA%/ms-playwright/chromium-*` 目录存在 → 下载中断的空/残目录也算就绪；且无视 `PLAYWRIGHT_BROWSERS_PATH` 自定义缓存位置（会误判未安装而重复下载 ~150MB）。方案：目录存在时再校验非空（executable 文件存在更佳）；读环境变量覆盖默认路径
- E7 Unix 孤儿清理单点失败中止全部：`src/bridge/orphan.rs` — `cleanup_orphan_browsers_inner` 中 `parse_ppid_from_stat(&stat)?` 用 `?` 让整个函数提前返回 → 一个怪异进程的 stat 使全部孤儿浏览器漏清。方案：该处改 `continue`（与同函数其他错误处理一致）。Windows 优先项目，低优先级

### 5.9 更新器 P2 四项

- U2 默认源无完整性校验：`src/updater/check.rs` — `fetch_manifest` 的 GitHub 分支构造 `sha256: String::new()` → `download.rs` `download_and_verify` 遇空串 `warn!("跳过校验，信任 HTTPS")`；默认 `release_source_url` 恰是 GitHub API → **默认配置下更新包没有任何哈希校验**。方案：GitHub 源也从 release assets 找 `.sha256` 伴随文件；找不到时在响应/日志明示降级
- U4 helper 闪黑窗：`src/updater/mod.rs` — `spawn_helper` 是项目内唯一没设 `CREATE_NO_WINDOW`（0x08000000）的子进程 spawn，且 helper 内有多行 println。方案：补 `cfg(windows)` + `CommandExt::creation_flags`
- U6 开关语义错误 + 死配置：`src/updater/mod.rs` — `start_background_check` 循环体内 `if settings.check_on_startup {...}` 把"启动时检查"当成了所有迭代的总开关（关闭后 24h 定期检查一并消失）；`update_channel`（stable/beta）字段完全未被消费。方案：循环外读一次 `check_on_startup` 决定"启动即查"，循环内只做周期检查；`update_channel` 接入 `check_update` 的清单选择或删除
- U3 启动兜底无二次校验：`apply_pending_on_startup` 不重算 sha256、不比对 `pending.version` 与当前版本 → 下载与启动之间的时间窗内 staging 产物可被替换。方案：应用前重算哈希（若 manifest 可得），`pending.version <= 当前版本` 则跳过并清理

## 批次六：前端契约修复（frontend/src）

> 背景说明：openapi.json 是空 schema 的 path-only baseline（components.schemas 为空，92 个操作 0 个请求/响应 schema），
> 字段级契约实际以 `frontend/src/api/types.ts` 注释与后端 routes 实现为准——这是本批所有漂移的根因。
> 本批修完后可考虑给 openapi.json 补 schema（另立项，不混入本批）。

- [x] 全部完成（2026-08-15）

### 6.1 P1-15 新建配置方案必 404

- 位置：`frontend/src/api/index.ts` — `profilesApi`（只有 `save` = PUT）；`frontend/src/composables/useProfiles.ts` — `saveProfile`（`const { id, _isNew, ...settings } = profile` 把 `_isNew` 解构后丢弃，新建也走 PUT）；后端 `src/web/routes/profiles.rs` — PUT `update_profile` 先 `load_profile(&id)?` → 新 id 必 404；POST `create_profile` 存在但前端无人调用
- 方案：① `profilesApi` 加 `create: (id, payload) => http.post(...)`；② `saveProfile` 按 `editingProfile._isNew` 分流——新建走 create（body 必含 name/username/password，对齐后端 `ProfileCreateBody` 必填字段），更新走 save；③ 新建成功后本地置 `_isNew = false` 或直接关闭编辑器
- 验证：手工——新建方案→保存→列表出现→再次编辑保存成功；`npm run build`（vue-tsc）通过

### 6.2 P1-16 定时任务执行历史契约错位（弹窗渲染异常）

- 位置：后端 `src/web/routes/scheduler.rs` — `job_history` 返回扁平数组 `[{run_at, success, message}]`（`map_history_records` 映射，含单测）；前端 `frontend/src/views/ScheduledTasksView.vue` — 历史弹窗用 `record.timestamp` / `record.status` / `record.duration` 渲染（`record.timestamp.replace(...)` 对 undefined 抛 TypeError，`record.status === 'success'` 恒 false → 全部显示"失败"）；`frontend/src/composables/useScheduledTasks.ts` — `scheduledTaskHistory` 类型定义同错
- 方案：① 后端 `map_history_records` 输出补 `duration`（从原始 record 透传，缺失为 null）；② 前端模板改用 `run_at` / `success` / `message` / `duration`，成功判定 `record.success`，时间格式 `record.run_at.replace('T',' ').substring(0,19)`；③ 同步修 `useScheduledTasks.ts` 的类型定义
- 验证：手工——触发一次定时任务后打开历史弹窗，无控制台报错、成功记录显示"成功"

### 6.3 P1-17 "自定义运营商"输入框敲第一个字符即消失

- 位置：`frontend/src/views/ProfilesView.vue`（`v-if="p.editingProfile.value.isp === '自定义'"` 与输入框 `v-model.trim` 绑同一字段）；`frontend/src/views/settings/AccountSettings.vue` 同模式；`frontend/src/utils/constants.ts` — `CARRIER_OPTIONS` 的"自定义"项 value 就是字面量 `"自定义"`
- 根因：选中"自定义"→ 输入框出现 → 敲第一个字符 isp ≠ '自定义' → v-if 变 false 输入框当场卸载；直接保存会把字面量"自定义"作为 ISP 提交到后端
- 方案：两处页面加独立状态 `showCustomCarrier`（ref）；watch isp——新值 === '自定义' → true，新值为其他预设 → false；`v-if` 改绑 `showCustomCarrier`；编辑器初始化时 `showCustomCarrier = isp 非空 && 不在 CARRIER_OPTIONS 预设 value 列表`。保存前校验：`isp === '自定义'` 且自定义输入为空 → toast 拒绝
- 验证：手工——选"自定义"→输入文字框不消失→保存→重新打开编辑器值保留

### 6.4 P1-18 浏览器 channel 命名不一致（Chromium 自动下载永不触发）

- 位置：后端 `src/web/routes/system.rs` — `list_browsers` 的 Chromium 项 `channel: "chromium"`；前端 `frontend/src/composables/useUi.ts` — `handleBrowserClick` 判 `browser.channel === "playwright"`（分支永不命中，落入"跳转官网下载"兜底）、`selectedBrowser` 初始值 `"playwright"`；`frontend/src/views/settings/BrowserSettings.vue` — `handleBrowserClick` 两处 `"playwright"` 判断（未安装卡片点击无反应、下载中状态不显示）
- 方案：前端统一改判 `"chromium"`（useUi.ts / BrowserSettings.vue / selectedBrowser 初始值）；改完 `grep -r "playwright" frontend/src --include="*.ts" --include="*.vue"` 复查 channel 字面量无残留（注意区分 Playwright 库名本身的正常引用）
- 验证：手工——清空 Playwright 浏览器缓存后点击 Chromium 卡片，出现"是否自动下载（约 150MB）"确认框

### 6.5 P1-19 status_detail 幽灵字段（监控状态横幅恒"已停止"）

- 位置：`frontend/src/composables/useStatus.ts` — status 初始 `status_detail: "已停止"`（truthy），`networkStatusText = status.status_detail || "正在启动监控"`；后端 `src/status/snapshot.rs` — `StatusSnapshot` 无此字段，WS/HTTP 从不推送 → `||` 兜底永不生效
- 方案：删除 `status_detail` 字段；`networkStatusText` 改为——`!monitoring` → '已停止'；否则按后端实际推送的网络状态字段映射（对照 snapshot.rs 的字段名）：online → '在线监测中'、captive_portal → '检测到门户劫持'、offline → '网络断开'、初始/unknown → '正在启动监控'
- 验证：手工——启动监测后横幅不再显示"已停止"；断网/门户态文案正确切换

### 6.6 前端死代码清理（每项先 grep 确认零引用再删，删后 build 必须过）

- 组件：`components/TaskEditor.vue`、`components/StepEditor.vue`、`components/common/BrowserSelector.vue`、`components/common/LoadingSpinner.vue`（TasksView 用的是自带内联编辑器）
- composables / api / utils：`composables/useMutation.ts` 整体；`api/index.ts` 的 `uninstallApi`；`useUi.ts` 的 `recordInitError` / `destroyApp` / `openFullscreen` / `closeFullscreen` / `state.fullscreenSrc` / `getActiveBrowserChannel`；`useConfig.ts` 的 `onShellFileSelected`（自述空壳）/ `ensureAtLeastOneCheckMethod` / `setAutostartMode`（删除后后端 `/api/autostart/mode` 彻底无人调用，去留见 7.2）/ `availableShells` / `defaultShell`；`utils/formatters.ts` 的 `formatLogTime` / `getLogClass`；`utils/file.ts` 的 `safeApiCall`；`utils/constants.ts` 的 `BG_COLORS` / `LOGIN_ACTION_OPTIONS` / `LOG_SOURCE_OPTIONS` / `SCHEDULED_TASK_TYPE_OPTIONS` / `AUTOSTART_MODE_OPTIONS` / `TIMING.WS_READY_TIMEOUT` / `TIMING.WS_BACKOFF_MAX`（useWebSocket 自定义了 60s 上限，与常量 30s 不一致）/ `LIMITS.SCROLL_BOTTOM_THRESHOLD`（Dashboard 硬编码 40）
- 顺带：`useUi.ts` init 的 `Promise.allSettled` 失败统计形同虚设（所有 fetch 内部自 catch 从不 reject）——要么移除统计要么让 fetch 可 reject

### 6.7 调试面板不可达（功能写完没接入口）

- 位置：`frontend/src/composables/useDebug.ts` — `startDebug` 无调用方（唯一置 `visible=true` 的入口）；`components/DebugPanel.vue` 永不显示；`useWebSocket.ts` 的 screenshot / step_progress 分发无人消费
- 方案（二选一）：① 在 TasksView 内联任务编辑器工具栏加"调试"按钮 → `useDebug.startDebug(taskId)`（先读 useDebug API 确认参数形状）；② 暂不接线则在 known-issues 记录挂起。前置依赖：5.5 修完后 debug 功能才真正可用（debug_run_all 目前必失败）
- 验证：选①则手工——点调试进入面板，单步 / 全部执行 / 停止均可用，停止后凭据截图被清理

### 6.8 axios 遗留错误形状（兜底分支全失效）

- 位置：`frontend/src/composables/useStatus.ts` — `fetchAutostart`、`useConfig.ts` — `toggleAutostart`、`useUi.ts` — `checkInitStatus` 均判 `(error as {response?}).response?.status`（ApiError 把 status 挂在错误对象本身，`response` 恒 undefined，"当前后端不支持"兜底成死代码）；`useAppearance.ts` 壁纸 catch 读 `e.response?.data?.detail` 同病（永远显示兜底文案，丢失真实错误）
- 方案：统一改 `instanceof ApiError` / `(e as ApiError).status` 或 client.ts 暴露的类型判断助手（先读 `api/client.ts` 确认导出形状再动手）
- 验证：`npm run build` 通过；可选手工验证旧后端场景兜底生效

### 6.9 交互小修合集

- `useConfirm.ts`：单例 resolver 被并发 confirm 覆盖 → 前一 Promise 永挂（路由守卫确认与业务确认并发可触发）。方案：并发时先 `resolve(false)` 旧 Promise 再挂新的（或队列化）
- `DashboardView.vue` — `clearLoginHistory` 本地实现无确认，遮蔽 useUi 带确认版 → 改调 useUi 版并删除本地实现
- `ProfilesView.vue` — `saveAndClose` 无条件 `showEditor = false`，保存失败/校验拒绝时表单消失但数据仍在（再打开先弹"放弃未保存的修改"，体验割裂）→ 失败时保持编辑器打开（关闭动作移入成功分支，或 saveProfile 返回布尔由调用方决定）
- 任务/脚本列表互串显：`GET /api/tasks` 与 `GET /api/scripts` 返回同一个混合列表（都调 `list_all_tasks`），两页均不按 `task_type` 过滤 → 脚本页列出浏览器任务（点"编辑"走 `scriptsApi.get` 报"加载脚本失败"）、任务页列出脚本。方案：各页按 `task_type` 过滤（browser 归任务页，script/shell 归脚本页）
- `ScriptsView.vue` 拖拽排序只 splice 本地数组不持久化 → 复用 `useDragSort` 调 `POST /api/tasks/order`（与 TasksView 一致）
- `views/settings/SystemSettings.vue`："文件日志开关"（`logging.file_enabled`）标签误写"显示 HTTP 请求日志"→ 改正；"全局日志级别"只随配置保存落盘 → 改调 `PUT /api/config/log-level` 热更新（后端 P1-5 已修好该端点，两条路径生效时机应统一）
- `views/AboutView.vue` — `health.python_version` 后端 `/api/health` 从不返回（只有 status/version）→ 删除或改由 `/api/system` 的 environment 状态推导
- `useConfig.ts` — `resetConfig` 漏恢复 `config.worker` 段（fetchConfig 有 worker）→ 补
- `api/client.ts` — `request()` 中 `opts.timeout` 存在时 `opts.signal` 被静默忽略（useUi.installPlaywright 同时传两者，外部 AbortController 无效）→ 用 `AbortSignal.any([...])` 组合或文档注明互斥
- `utils/file.ts` — `pickFile` 取消选择时 Promise 永不 resolve（浏览器 input cancel 无事件，属平台限制）→ 可选：window focus 后延时 resolve(null) 兜底，注明局限

## 批次七：Rust 死代码清理 + Web 杂项

- [x] 全部完成（2026-08-15）

### 7.1 死代码删除清单

> 执行方式：每项先全仓库 grep 确认零引用（含测试），删后 `cargo clippy --all-targets -- -D warnings` + `cargo test` 必须过。
> 注意：`EgressBinder` trait（network/mod.rs）与 `config/schema.rs` 的 `bind_interface_name` 是文档标注的"预留"接口，**保留不删**（可加注释说明）。

- `src/launcher.rs` — `_restart` 函数（正确逻辑已由 `/api/system/restart` 路由内实现，此函数是死代码前身）
- `src/engine/mod.rs` — `EngineHandle::stop` / `into_completion` / `task_handle`（先确认 task_handle 零引用）+ `stop_tx`/`stop_rx` watch 通道整条路径（停机已由 `EngineCommand::Shutdown` 承担）；`EngineError::{ReloadFailed, ProfileNotFound, RestartExhausted}`（仅测试构造）；`EngineDeps.base_path` 字段（launcher/watch_engine 赋值但 run_loop 不读）
- `src/login/mod.rs` — `LoginError` 整个枚举（注释自称"保留"，确认后删）
- `src/monitor/mod.rs` — `rebuild_client` 函数
- `src/network/detect.rs` — `NetworkError::{Io, Socks5PortBusy, Socks5Crashed}`（SOCKS5 已于 a60b164 整体删除，Io 的文档注释还在说"绑定 SOCKS5 端口"）+ `GatewayInfo` 结构体；`src/network/interfaces.rs` — `sort_interfaces`
- `src/scheduler/mod.rs` — `SchedulerStatus` 结构体与 `SchedulerService::status()`；`SchedulerError::{SubmitRejected, ExecutorError}`（从无构造点）
- `src/web/state.rs` — `axum_running` 字段 + `set_running` / `is_running`
- `src/tray/mod.rs` — `TrayDeps.orchestrator` 与 `TrayManager.orchestrator` 字段（注释宣称托盘有"立即登录"，菜单实际只有 3 项）
- `src/updater/mod.rs` — `cached_manifest` 字段（只写不读）；`src/updater/check.rs` — `PlatformPackage.sig_url`（注释自认"未来支持"）
- `src/config/mod.rs` — `ENC_KEY_FILE` 常量（crypto.rs 硬编码了同一字符串）；`src/config/service.rs` — `ConfigError::KeyFileCorrupt`（从不构造）、`ConfigError::ConfigNotFound`（仅测试构造——先查 match 分支是否有意义再决定删否）
- `src/utils/lock.rs` — `InstanceLock::release()`；`src/utils/metrics.rs` — `Metrics::default()`（若仅测试用）
- `src/environment/mod.rs` — `UV_MIN_VERSION`（见 5.4 可活用）、`_worker_state_for_capability`；`src/environment/uv.rs` — `run_uv_command`（仅被 re-export）
- `src/bridge/session.rs` — `SessionGuard::{session_type, cancel, is_cancelled, force_close}` 四方法 + `cancelled` 死字段（当前清理全靠 Drop）
- `src/tasks/mod.rs` — `TaskError::{ExecutionCancelled, QueueFull}`（从不构造；同时说明执行器无取消机制——若要做取消属新功能另立项）
- `src/web/routes/system.rs` — `_helper_path`
- `src/web/ws.rs` — `WsMessage::Screenshot/StepProgress`（已标 allow；若 6.7 接线调试面板则保留）

### 7.2 Web API 杂项

- 静态回退吞 404：`src/web/static_files.rs` + `src/web/mod.rs` fallback——未注册的 `GET /api/xxx` 返回 200 + index.html → 对 `/api` 前缀直接 404 JSON，前端拼错路径可定位
- `fetch_logs` limit 无上限（`routes/system.rs`，可传 99999999 全量解析 MB 级日志）→ `.min(2000)` 钳制
- history 分页 total 不一致（`routes/history.rs`，total 在 limit 截断前取值）→ 先取全量 len 作 total 再截断
- `web/error.rs`：所有 NotFound 的 code 都是 `CONFIG_NOT_FOUND`（任务/Profile/定时任务 404 也报这个码）→ 按资源类型区分 code；`serde_json::Error → BadRequest` 会把**响应序列化失败**也变成 400 → 序列化失败应映射 500
- `execute_task` 把所有执行错误映射 500（`routes/tasks.rs` 手工 `ApiError::Internal`）→ executor 的 `TaskError::Bridge(String)` 把 BridgeError 字符串化丢失了变体；改带类型（或新增变体），恢复 `error.rs` 已有的 409 WorkerBusy 映射
- `create_job` 重复 id 静默覆盖已有任务（`routes/scheduler.rs`，save_task 是 upsert 语义）→ 已存在则 409；顺带 `JobCreateBody` 补 `description`/`timeout`（与 JobUpdateBody 不对称，前端新建表单有这两个输入框但被静默丢弃——与 6.x 前端侧联动）
- `run_job` 的 `run_id` 是死数据（生成了 UUID 却不传给 execute_scheduled_task，也无端点可查）+ 手动触发不走 concurrency 信号量 → run_id 记入执行历史或从响应删除；手动触发与 cron 触发走同一并发闸
- `start_monitor`/`stop_monitor` 吞 `try_dispatch` 错误（`routes/monitor.rs`，`let _ =` 后无条件返回"已启动/已停止"）→ 错误如实返回
- `patch_settings` 收 `carrier_custom` 但无应用分支（`routes/config.rs` 的 profile_keys 列表）→ 补应用逻辑或从 keys 移除
- `import_tasks` 中途失败无回滚也无部分成功提示（`routes/tasks.rs`）→ 返回 `{imported, failed: [{id, reason}]}`

### 7.3 其他后端杂项

- uninstall 路由（`routes/system.rs`）：脚本写到 `base/uninstall.bat` 但响应消息说 `config/uninstall.bat`；`rd /s /q "{base}"` 删除脚本自身所在目录，bat 运行期自身被锁必残留 → 脚本先 copy 到 `%TEMP%` 再执行自删；`_helper_path` 见 7.1
- 任务执行超时 `kill_on_drop` 只杀直接子进程（`tasks/executor.rs`，Windows 上 cmd.exe 的脚本子树可能存活为孤儿）→ `taskkill /T /PID` 或 Job Object（工程量大，可先记录 known-issues 挂起）
- 网卡枚举无 TTL 缓存：`src/network/detect.rs` 每次 `list_interfaces`/`default_gateways`/`current_ssid` 都新起 ipconfig/netsh/route 子进程（各 10s 超时）；monitor 每探测周期 + 每个 Web 请求（`routes/monitor.rs`、`routes/profiles.rs` detect）都调 → 加 30s TTL 缓存（与 2026-07-04 网卡绑定设计文档"IP 解析 30s TTL 单一缓存"方向一致）
- `src/utils/io.rs` — `atomic_write_json` 无 fsync（scheduler/task.rs 与 tasks/loader.rs 的持久化路径在用）→ 对齐 `ConfigService::atomic_write_json` 的保证（sync_all + 平台特定处理）
- `src/updater/download.rs` — 进度 `(downloaded * 100) / total` 后 `as u8`，服务端实发字节超 Content-Length 时 >255 回绕 → 改 u32 或 `saturating`
- Engine 崩溃重启后监测不自动恢复（launcher.rs watch_engine 重启的 Engine `monitoring=false`，且托盘/Web 持有的仍是已死的初始 Engine 引用——`container.engine_handle` 不可变，重启只更新 `latest_engine_cmd_tx`）→ 中期方案：Engine 引用收口为 `Arc<ArcSwap<Arc<Engine>>>` 之类的可替换句柄；短期至少重启后按原状态重发 Start。改动面较大，建议单独评估后再动

### 7.4 Python 其余 P2

- `step_handlers.py` — `_resolve` 原地改写 StepConfig 字段 → 改返回副本（`dataclasses.replace`），调试会话重跑同一步骤不再发生二次变量解析（密码值本身含 `{{VAR}}` 字面量时会被再次替换）
- `playwright_worker.py` — `browser_data/<channel>` 持久化目录与 `debug` 截图目录用相对路径（依赖继承的 CWD，Rust spawn 未设 current_dir）→ 锚定到 worker 脚本目录，或 Rust spawn 时显式传 `current_dir`
- 任务级 `TaskConfig.timeout` 在 Python 侧完全未实现（Rust 序列化进 extras 后无人读）→ `run_steps` 外层 `asyncio.wait_for` watchdog；或确认 Rust executor 已有超时兜底后注明
- `duration_ms` 恒 0 两处：`worker_main.py` — `_structured_result` 硬编码 0；`playwright_worker.py` — success_condition 失败分支把 `time.perf_counter()` 误当 start 传入 `_build_result` → 修正计时起点
- `playwright_worker.py` — `bs.get("stealth_custom_script", "").strip()` 对显式 null 抛 AttributeError → `(bs.get("stealth_custom_script") or "").strip()`
- 多调试会话共享 `self._page`：第二次 debug_start 在同一页面导航覆盖第一次 → 限制单调试会话（与 Rust 单会话语义一致）或每会话独立 page
- iframe 支持不一致：`handle_click_select` 的 option 定位与 `handle_ocr` 的 target 填充直接用 `context.page.locator(...)` 绕过 `_locator` 的 frame 处理；`handle_input` 的 reveal_hidden JS 用 `document.querySelector` 只查顶层文档 → 统一走 frame-aware 定位
- `handle_assert_text` 用字符串拼接构造 JS（只转义 `\` 和 `'`，值含真实换行即破坏字面量）→ `wait_for_function("...includes(arg)", [value])` 参数传递
- `run_steps` 的 `step_delay` sleep 期间不响应取消（仅步骤边界检查）→ sleep 分片 + 每片查 cancel_event

## 批次八：验证与收尾

- [x] 2026-08-15 全量验证通过：`cargo test`（默认 + no-embed 双 feature，311 项）、`cargo clippy --all-targets -- -D warnings` 零警告、`frontend npm run build`（vue-tsc + vite）、`python_worker pytest` 48 项（`.venv`）
- [x] 批次五~七每批完成后跑「全局验证命令」块
- [x] 手工冒烟清单：托盘退出后进程真正退出（`--no-tray` 模式无法验证 GUI 托盘，接口/进程层已验证优雅关闭后进程数为 0）；Web restart 不出现双进程互锁（PID 变化、单实例）；禁用的定时任务可在 UI/API 重新启用（toggle 返回正确翻转）；脚本编辑保存后重新打开 name/description/binary_path 完整（PUT 后 GET 全字段保留）；更新流程（helper 等待存活 PID 超时）exit 1 不再销毁 staging/pending；OCR 请求与登录/debug 会话并发不互相破坏（debug 会话中发 OCR，会话不被复位）
- [x] 全部完成后：`docs/changelog.md` 归档本批条目，`docs/known-issues.md` 移除对应项
