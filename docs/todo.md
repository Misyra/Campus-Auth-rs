# 待办清单（TODO）

> 概略规划，不锚定具体行号（代码会变）。来源：`docs/known-issues.md` 有效条目 + 工程化短板。
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

> 来源：QoderWork 全面审查（P0×3 / P1×21 / P2 若干，已逐条对照源码验证去误报）。
> 本次会话已完成第一~三批并提交；以下为剩余工作。

## 已完成（本提交包含）

- P0-1 Worker stdio 编码：`worker_main.py` 强制 UTF-8 reconfigure + `process.rs` spawn 注入 `PYTHONUTF8=1`
- P0-2 脚本保存摧毁脚本：前端 payload 补 `type:"script"`；后端 `get_script` 返回完整字段、`update_script` 前置 type 校验
- P0-3 路径穿越：`scheduler/mod.rs` delete_task 与 `routes/scheduler.rs` job_history 补 `is_valid_id`；CORS 从 `mirror_request` 收紧为仅本机 Origin
- P1-2 托盘退出：TrayDeps 注入 shutdown 令牌，Quit 时取消；graceful_shutdown 首步取消令牌；watch_engine 不再在关闭期重启 Engine
- P1-3 restart 路由：spawn 带 `--restarting` + 走优雅关闭（原来必"重启变退出"）
- P1-4 startup_action 接线：启动按配置派发 Start/LoginOnce；`runtime_mode` 缺省时读 settings.json（CLI --mode 改 Option）
- P1-5 日志热更新：`SharedTargets`（Arc<Mutex<Targets>> 自定义 Filter）真正生效；初始级别读 `logging.level`
- P1-8 探测误判：HTTP/URL 探测 4xx/5xx 改判 Pass（连通成立），仅超时/连接失败为 Fail
- P1-9 登录结果重复计数：回传 channel 改传 `LoginResult`，仅 source=Auto 计入冷却统计；`auto_login_in_flight` 去重
- P1-12 配置损坏防护：load_settings 失败时沿用缓存或进入隔离态，save_settings 拒绝覆盖
- P1-13 禁用任务入缓存（可再启用）；P1-14 save_task 校验 cron
- P2-1 冷却到期重置失败计数；P2-2 Stop 后状态不被合并回 Running；P2-3 定时器首 tick 消费（`reset_check_timer`）
- P1-20 Python 非 dict JSON 行 guard（不再杀死 Worker）
- （P1-1 密码泄露经核实在此前工作区已修复：list_profiles 返回无密码的 ProfileSummary）

## 待做

## 批次五：Bridge / 更新器 / 环境 / Python Worker

- [ ] **P1-6 OCR 并发摧毁登录会话槽位**：`bridge/mod.rs` `check_session_compat` 无条件放行 ocr_recognize，但 `execute_inner` 会覆写单槽位（current_session/cancel_id/request_id），OCR 完成即复位 Idle 并启动空闲计时器 → 在途登录可能被空闲回收杀掉 Worker。方案：OCR 走独立轻量通道（不动会话槽位，guard 只清自己的 pending/cancel 注册）
- [ ] **P1-7 Bridge 调用方超时不清理**：`execute_with_timeout` 超时只返回 Timeout，supervisor 侧转发 task 持 SessionGuard 无限等待；Worker 挂死时会话槽位永久被占。方案：超时路径生成 cancel_id 并发 `SupervisorCommand::Cancel`（需同时在本地 trigger token）
- [ ] **P1-10 更新流程断裂**：`updater/mod.rs` apply_update 注释要求调用方退出主进程，但 `routes/system.rs` 与前端 AboutView 均不退出；helper 等 60s 强制继续 → 覆盖失败 → cleanup 删 pending+staging，更新丢失。方案：①helper 超时改为保留 staging 退出（交给 apply_pending_on_startup 兜底）；②前端更新成功后确认并调用 shutdown
- [ ] **P1-11 uv 判定与使用不一致**：`bootstrap.rs` check 接受 PATH 上的 uv，但 `uv.rs` run_uv_sync/install_playwright 硬编码本地 `environment/uv.exe` → PATH-only 机器引导卡死。方案：本地缺失时回退 `Command::new("uv")`
- [ ] **P1-21 debug session_id 两端不一致**：`routes/debug.rs` debug_stop/debug_run_all 发 `Value::Null`，Python 侧 session_id="" → run_all 必失败、stop 跳过凭据截图清理。方案：Python 侧空 session_id 回退唯一活跃会话
- [ ] **P2-4 正常退出记为崩溃**：`bridge/mod.rs` handle_worker_exited 不分退出码，空闲回收/API stop 也 inc_worker_crash + 状态翻 Error
- [ ] **P2-5 OCR 模型每次重载**：`playwright_worker.py:~837` 与 `step_handlers.py:~387` 各自 new DdddOcr；按 old 参数做模块级单例缓存，classification 用 asyncio.to_thread
- [ ] 环境模块 P2：E2 版本/SHA 下载无超时（uv.rs fetch/download_text）；E6 Playwright 就绪检查过弱；E7 Unix 孤儿清理单点失败中止全部
- [ ] 更新器 P2：U2 GitHub 源 sha256 为空跳过校验；U4 helper spawn 未加 CREATE_NO_WINDOW；U6 `check_on_startup` 开关误伤定期检查、`update_channel` 死配置；U3 apply_pending_on_startup 无二次校验

## 批次六：前端契约修复

- [ ] **P1-15 新建方案必 404**：`api/index.ts` profilesApi 只有 PUT；新建应走 `POST /api/profiles/{id}`（useProfiles.saveProfile 按 `_isNew` 分流）
- [ ] **P1-16 定时任务历史契约错位**：后端返回 `[{run_at, success, message}]`，`ScheduledTasksView.vue` 用 `record.timestamp/.status/.duration` 渲染崩溃。方案：后端 map_history_records 补 duration 字段 + 前端按 run_at/success/message 渲染
- [ ] **P1-17 自定义运营商输入框敲一字即消失**：`ProfilesView.vue` 与 `AccountSettings.vue` v-if 与 v-model 绑同一字段（isp==='自定义'）。方案：独立 showCustomCarrier 标记
- [ ] **P1-18 channel 命名不一致**：后端 `"chromium"` vs 前端 `"playwright"`（useUi.ts:~220、BrowserSettings.vue:~28）→ Chromium 自动下载永不触发
- [ ] **P1-19 status_detail 幽灵字段**：useStatus.ts 初始 "已停止" 恒 truthy，后端从不推送 → 横幅恒显示已停止。按 monitoring/network 推导文案
- [ ] 前端死代码清理：TaskEditor.vue / StepEditor.vue / BrowserSelector.vue / LoadingSpinner.vue / useMutation.ts / uninstallApi / useUi.recordInitError、destroyApp / useConfig setAutostartMode（后端 /api/autostart/mode 因此无人调用）/ constants 未用常量（BG_COLORS、LOGIN_ACTION_OPTIONS、LOG_SOURCE_OPTIONS、SCHEDULED_TASK_TYPE_OPTIONS、AUTOSTART_MODE_OPTIONS 等）
- [ ] 调试面板不可达：useDebug.startDebug 无调用方 → 在任务编辑器加"调试"入口接线（或暂缓）
- [ ] axios 遗留 `error.response?.status` 三处失效（useStatus.ts fetchAutostart / useConfig.toggleAutostart / useUi.checkInitStatus）；useAppearance.ts:~374 `e.response?.data?.detail` 同病
- [ ] 小修：useConfirm 并发覆盖 resolver（前一 Promise 永挂）；Dashboard 清空历史无确认（本地实现遮蔽 useUi 带确认版）；ProfilesView 保存失败仍隐藏编辑器；任务/脚本列表互串显（按 task_type 过滤）；脚本页拖拽排序不持久化；SystemSettings "文件日志开关"标签写错；About health.python_version 幽灵字段；resetConfig 漏 worker 段；客户端 timeout 与 signal 互斥（client.ts）

## 批次七：Rust 死代码清理 + 杂项 P2

- [ ] 死代码（均 grep 确认无调用方）：`launcher.rs _restart`（已被路由内实现替代）；`EngineHandle::stop/into_completion` + stop 通道；`LoginError` 整个枚举；`EngineDeps.base_path`；`monitor rebuild_client`；`network/detect.rs` SOCKS5 遗留错误变体（Io 文档提及/Socks5PortBusy/Socks5Crashed）+ GatewayInfo + sort_interfaces；`scheduler SchedulerStatus/status()`；`web/state.rs axum_running` 三件套；`TrayDeps.orchestrator`；`updater cached_manifest`（只写不读）+ `check.rs sig_url`；`config ENC_KEY_FILE/KeyFileCorrupt`；`utils/lock InstanceLock::release()`；`environment UV_MIN_VERSION/_worker_state_for_capability/run_uv_command`；`bridge SessionGuard` 四个未用方法 + cancelled 死字段；`tasks TaskError::{ExecutionCancelled,QueueFull}`；`routes/system.rs _helper_path`
- [ ] web 杂项：未注册 `/api/*` GET 返回 200+index.html（static_files 回退需排除 /api 前缀）；fetch_logs limit 无上限；history 分页 total 与截断后不一致；error.rs 所有 NotFound 都报 CONFIG_NOT_FOUND、serde_json::Error→BadRequest 误伤响应序列化；execute_task 把 BridgeError 字符串化丢失 409 映射；create_job 重复 id 静默覆盖（应 409）；run_job 的 run_id 死数据 + 手动触发绕过并发信号量；start/stop_monitor 吞 try_dispatch 错误；patch_settings 收 carrier_custom 但不应用；import_tasks 部分成功无提示
- [ ] 其他：uninstall.bat 自删所在目录 + 文案路径不符；helper cleanup 路径与 staging 参数不一致（helper_main.rs）；任务超时 kill_on_drop 只杀 cmd.exe 子进程；网卡枚举无 TTL 缓存（每探测周期起 ipconfig/netsh 子进程，建议 30s）；`utils::io::atomic_write_json` 无 fsync；updater 进度 `as u8` 回绕
- [ ] Python 其余 P2：_resolve 原地改写 StepConfig（调试重跑二次解析，应返回副本）；浏览器数据目录相对 CWD；任务级 timeout Python 侧未实现；duration_ms 恒 0 两处；stealth_custom_script 显式 null 崩溃；多调试会话共享 self._page；iframe 步骤绕过 _locator；assert_text 字符串拼接构造 JS；step_delay sleep 不响应取消

## 批次八：验证（每批完成后）

- [ ] `cargo test`（注意：2026-08-15 第一~三批修复后未跑完整测试，提交前仅 cargo check/clippy 通过——次日首先补跑）
- [ ] `cd frontend && npm run build`（vue-tsc 验证前端契约改动）
- [ ] python_worker pytest（如 venv 可用）
- [ ] 手工冒烟：托盘退出进程真正退出、Web restart 可用、禁用任务可再启用、脚本编辑保存后重新打开字段完整