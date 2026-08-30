# 功能实测 Bug 报告（2026-08-30）

> 测试方式：本地模拟认证门户（`mock_portal/server.py`，带图片验证码）+ Campus-Auth 全链路实测。
> 驱动方式：Web API（curl/python）+ ZCode 内置浏览器 GUI 黑盒操作（仪表盘/任务/设置/定时任务页）。
> 运行形态：`target/debug/campus-auth.exe --no-tray`（dev 模式，base_path=target/debug），版本 5.0.0-alpha.1（f08b18b）。
> 结论速览：**核心链路全部通过**（OCR 验证码登录、掉线自动重连、监测探测、cron 调度、GUI 渲染），共发现 **6 个缺陷**（P1×2、P2×2、P3×2）。
>
> **✅ 修复状态（2026-08-30）**：6 个缺陷 + 4 个观察项已全部修复并通过端到端复验（BUG-1 以卸载 junction 后真实登录验证；BUG-2/4/5/O3 经内置浏览器 GUI 验证），构建链全绿（cargo test 506、clippy -D warnings、vitest 40、pytest 123）。修复明细见文末附录。

---

## 一、结论摘要

### 实测通过项

| 功能 | 结果 |
|------|------|
| 手动登录（API 路径，含验证码 OCR） | ✅ 9.3s 成功；ddddocr 6 次验证码识别全部一次命中 |
| 掉线自动重连 | ✅ logout 后 ≤15s 监测发现 captive → `src=auto` 自动登录 → 恢复 online |
| 监测探测与状态翻转 | ✅ offline ↔ online 随门户认证状态正确切换（15s 周期，82 次探测无漏发） |
| 登录失败重试/冷却 | ✅ 失败按 2/4→3/4 重试，连续 3 次失败进入 300s 冷却（日志证据） |
| 定时任务 cron 调度 | ✅ 准点触发（01:13:00±1s）、结果持久化、任务卡显示"上次： 失败" |
| GUI 渲染 | ✅ 仪表盘/任务管理/设置/定时任务页布局正常，登录历史完整，实时日志 WS 正常 |
| 环境按需引导 | ✅ `POST /api/install/playwright` → ensure_capability 引导完成（浏览器自动化能力就绪） |

### 缺陷一览

| 编号 | 严重度 | 标题 | 影响面 |
|------|--------|------|--------|
| BUG-1 | 🔴 P1 | dev 模式下 Bridge 误报"Worker 环境未安装"，登录必失败 | 开发模式全部登录功能 |
| BUG-2 | 🔴 P1 | 前端"手动登录/取消登录"按钮 415，功能不可用 | 发布版 UI 核心按钮 |
| BUG-3 | 🟠 P2 | 任务独立执行/定时任务/调试会话不注入 `{{LOGIN_URL}}` 等系统变量 | 任务卡"执行"按钮、定时任务、调试面板 |
| BUG-4 | 🟠 P2 | 定时任务"查看历史"点击无反应 | 定时任务页 |
| BUG-5 | 🟡 P3 | Worker 步骤日志时间戳为 UTC，与本地时间混排 | 日志排查体验 |
| BUG-6 | 🟡 P3 | `PATCH /api/config` monitor 仅认前端字段名，错传静默丢配置 | API 契约健壮性 |

---

## 二、缺陷详情

### ✅ BUG-1 🔴 dev 模式 Bridge 误报"Worker 环境未安装"（P1）

- **现象**：`cargo run` 后任何登录请求直接失败，返回 `Bridge 执行失败: Worker 环境未安装`，登录历史与日志均如此记录（2026-08-30 00:50:08 有实录）。实际环境能力是就绪的（`/api/ocr/status` 返回 `installed: true`，`POST /api/install/playwright` 返回成功）。
- **复现**：仓库根 `cargo run` → `POST /api/login` → 立即失败。
- **根因**：两处路径解析不一致。
  - `src/bridge/mod.rs:1073-1082`：spawn 前硬检查 `base_path/python_worker/.venv/Scripts/python.exe` 与 `worker_main.py`，不存在即返回 `BridgeError::WorkerNotInstalled`；
  - `src/environment/mod.rs`（`new()` 内 worker_project_path 解析，约 L416-434）：base_path 下无 `python_worker/` 时**回退仓库根/CARGO_MANIFEST_DIR**（dev 模式兜底，单元测试注释明示此语义）。
  - `cargo run` 时 exe 在 `target/debug/`，数据目录下无 `python_worker/`，环境侧判定就绪（走回退），Bridge 判定未安装（硬路径）。
- **影响**：dev 模式下登录编排（手动/自动/once）完全不可用，且错误信息有误导性。发布版（build.ps1 将 python_worker 部署在 exe 旁）不受影响。
- **临时绕过**：`junction target\debug\python_worker → 仓库根 python_worker`（本次测试即采用）。
- **修复建议**：Bridge spawn 前的检查复用 `EnvironmentManager::python_path()` / `worker_project_path()` 的解析结果（与 `TaskExecutor` 同源，`src/environment/mod.rs` 注释已强调"脚本执行器与 Bridge 必须引用同一个 python_worker/.venv"）。

### ✅ BUG-2 🔴 前端"手动登录/取消登录"按钮 415（P1）

- **现象**：仪表盘点击"手动登录"，前端日志报 `手动登录失败 meta="请求失败 (415)"`（2026-08-30 01:04:53 实录），后端未收到有效请求体。
- **根因**：`frontend/src/api/client.ts:206-211` 的 `post()`：`rawBody: body !== undefined && body !== null ? JSON.stringify(body) : undefined`，而 `triggerLogin` 传 `body=null`（`frontend/src/api/index.ts:60`）→ `rawBody=undefined` → 请求**无 body 且无 `Content-Type`**（Content-Type 仅在 `rawBody !== undefined` 时补，client.ts:112-114）。后端 `POST /api/login` 用 axum `Json` 提取器，要求 `Content-Type: application/json` + 非空 body → 415。`cancelLogin`（`/api/login/cancel`，body undefined）同病。
- **影响**：发布版 UI 上这两个按钮必然失败（与 dev/发布形态无关）。后端 API 本身正常（带 Content-Type 直调验证通过）。
- **修复建议**（任选其一）：a) 前端 `post()` 对 null/undefined body 发送 `"{}"` 并补 Content-Type；b) `triggerLogin`/`cancelLogin` 传 `{}`；c) 后端两 handler 改为可空 body 提取。建议 a) 一次性根治同类调用。

### ✅ BUG-3 🟠 任务独立执行/定时任务/调试会话不注入系统变量（P2）

- **现象**：任务卡"执行"按钮（`POST /api/tasks/{id}/execute`）与定时任务触发的浏览器任务，凡使用 `{{LOGIN_URL}}` 模板的必然失败。实测响应（HTTP 200，`success:false`）：

  ```text
  导航失败: Page.goto: Protocol error (Page.navigate): Cannot navigate to invalid URL
  Call log: navigating to "{{LOGIN_URL}}" ...
  ```

  定时任务实测（01:13 准点触发）`last_result` 同样记录该失败；**调试会话实测同样在导航步骤即失败**（`POST /api/debug/start` → `outcome: network_error`，02:0x 实录）——BUG-3 影响面覆盖：登录编排 ✅正常、任务执行 ❌、定时任务 ❌、调试面板 ❌。
- **根因**：系统变量注入只存在于登录编排路径——Worker 仅在 `execute_login_attempt` 注入 `USERNAME/PASSWORD/ISP/LOGIN_URL`（`python_worker/playwright_worker.py:853-858`）；`TaskExecutor::execute_browser`（`src/tasks/executor.rs:134`，注释明示"不注入账号密码"）与 Worker 的 `execute_browser_task` 均不注入。
- **矛盾点**：任务管理页文档宣称 `{{USERNAME}}/{{PASSWORD}}/{{ISP}}/{{LOGIN_URL}}` 可用，内置 `default.json`（"通用登录"）的 url 就是 `{{LOGIN_URL}}`——即内置任务点"执行"必失败。登录编排路径（`/api/login`、自动登录）不受影响（实测 6 次成功，变量解析正常，含链式 `variables: {"username": "{{USERNAME}}"}`）。
- **修复建议**（需拍板取舍）：a) `execute_browser` 路径也注入系统变量（语义变为"任务可获取凭据"，与现注释的通用自动化定位冲突）；b) 保持不注入，但前端任务编辑器/执行前检测 `{{...}}` 未解析变量并提示改走登录；c) 至少修正任务页文档与内置任务模板。另注意 `ScheduledTask.profile_id` 字段注释"预留字段……暂不生效"（`src/scheduler/task.rs:59-62`），"定时登录"语义目前不存在。

### ✅ BUG-4 🟠 定时任务"查看历史"点击无反应（P2）

- **现象**：定时任务页点击"查看历史"按钮，UI 无任何变化（无弹层/抽屉/面板节点新增，DOM 已核实 body 下无新节点）。后端 `GET /api/scheduler/jobs/{id}/history` 数据正常（实测返回 run_at + message 记录）。
- **范围**：仅 GUI 层。疑似与历史遗留的 Modal 渲染条件问题同类（Modal 依赖必传 `open` prop，标签级 `v-if` 不够）。
- **修复建议**：排查 `frontend/src/views/` 定时任务页历史按钮的处理链与弹层渲染条件。

### ✅ BUG-5 🟡 Worker 步骤日志时间戳为 UTC（P3）

- **现象**：实时日志面板中，Python Worker 侧步骤日志（"步骤 1/5: 输入账号"等）时间戳比 Rust 侧日志**早 8 小时**（实录：同一分钟内 Rust 记 `01:04:25`、Worker 记 `2026-08-29 17:04:27`）。
- **根因**：Python Worker 日志时间戳使用 UTC（未转本地时区），前端原样展示，与 Rust 侧本地时间戳混排。
- **修复建议**：Worker 侧日志统一本地时区（或 ISO 带偏移），前端按偏移渲染。

### ✅ BUG-6 🟡 `PATCH /api/config` monitor 契约脆弱，错传字段静默丢配置（P3）

- **现象**：向 `PATCH /api/config` 传后端字段名（如 `{"monitor":{"http_targets":[...],"http_enabled":true}}`）时，请求成功返回，但 monitor 被写成空配置：`http_targets=[]`、`http_enabled=false`、`check_interval` 被默认值覆盖（实测 15→300）。无任何告警。本次测试首轮配置即被此坑命中。
- **根因**：`src/web/routes/config.rs:398-439` `monitor_frontend_to_backend` 按前端字段名取值（`test_urls`/`enable_http_check`/`check_interval_seconds`…），取不到即用 `unwrap_or` 默认值并合并落盘。openapi.json 对该 patch 的字段描述未见强约束（可复核）。
- **影响**：仅影响直接调 API 的场景（前端使用前端字段名不受影响）。
- **修复建议**：a) 对 monitor patch 做字段名校验，含未知/后端字段名时 400 拒绝；b) openapi.json 明确 monitor patch 仅接受前端形态字段。

---

## 三、非缺陷备注

- **暂停时段 `0:00-0:00` = 全天暂停**：`src/engine/run_loop.rs:711-713` 有意设计。本次测试环境遗留 `pause.enabled=true + 0:00-0:00`，导致监测完全不跑（`probe_total=0`、掉线不重连），日志提示"监测处于暂停时段"。排查监测类问题时应先查 pause 配置；可考虑在 UI 上对 0:00-0:00 给出"全天暂停"提示（体验项，非缺陷）。
- **dev 模式运行前提**：本次测试通过 junction 绕过 BUG-1（`target/debug/python_worker → 仓库根`）；修复 BUG-1 后可移除。
- **测试环境残留**：`mock_portal/`（模拟门户 + 轮询脚本）、`target/debug/tasks/browser/mock-ocr-login.json`、Profile default 已指向 `http://127.0.0.1:18765/`、settings 中监测 HTTP 探测指向 `http://127.0.0.1:18765/generate_204`。复测直接可用；正式开发时可按需还原。

## 四、关键证据时间线（2026-08-30，节选）

| 时间 | 事件 |
|------|------|
| 00:41:55 | 启动；"监测处于暂停时段，跳过手动操作触发的立即检测"（pause 遗留配置） |
| 00:50:08 | `POST /api/login` 失败："Bridge 执行失败: Worker 环境未安装"（BUG-1） |
| 00:53–00:54 | 首轮登录（junction 后）：OCR 3/3 识别正确，但 `user='{{username}}'` 字面量提交（任务模板大小写配置错误，修正后通过） |
| 00:56:29 | 登录成功（9.3s），登录后网络验证 Online |
| 01:00:25–01:00:40 | 掉线自动重连闭环：CaptivePortal → 自动登录 → Online |
| 01:01:25–01:01:40 | 第二次掉线自动重连（OCR 一次命中） |
| 01:04:25–01:04:53 | 掉线后监测先触发 auto 登录成功；GUI"手动登录"返回 415（BUG-2） |
| 01:07:35 | GUI 任务"执行"点击：前端仅记 INFO，后续 curl 复现确认 `{{LOGIN_URL}}` 未注入（BUG-3） |
| 01:13:00 | 定时任务准点触发，执行失败于同一变量问题（BUG-3）；`/history` 端点有数据而 UI 无弹层（BUG-4） |

---

## 附录：修复记录（2026-08-30）

| 编号 | 修复方式 | 复验方式 |
|------|----------|----------|
| BUG-1 | worker 工程路径解析抽为 `environment::resolve_worker_project_path`（仓库根回退），Bridge spawn 检查与 EnvironmentManager 共用（`src/bridge/mod.rs`、`src/environment/mod.rs`） | 卸载 junction 后真实登录成功 |
| BUG-2 | 前端写方法（post/put/patch）对缺失 body 统一发送 `{}` 并携带 application/json（`frontend/src/api/client.ts`） | GUI 点击"手动登录"完整登录成功，无 415 |
| BUG-3 | Rust `TaskExecutor::execute_browser` 与 `POST /api/debug/start` 注入活跃 Profile 系统变量；Worker `execute_browser_task`/`debug_start` 合并注入（仅覆盖调用方显式提供的键） | 任务卡执行 success:true 完整登录；debug_start 导航成功、步骤执行成功 |
| BUG-4 | 真正根因是 `ScheduledTask.id` 标记 `#[serde(skip)]` 导致列表/详情响应无 id，前端所有行级按钮拿不到 id；`list_jobs`/`get_job` 序列化后显式回填 `id`（`src/web/routes/scheduler.rs`） | GUI 点击"查看历史"面板正常弹出并渲染记录 |
| BUG-5 | 前端 3 处 `toISOString()`（UTC）改为本地时间 `localNowTimestamp()`（`useWebSocket.ts`、`utils/logger.ts`），与后端日志时间戳格式/时区一致 | GUI 日志面板步骤条目时间戳为本地时区 |
| BUG-6 | monitor patch 增加字段名白名单校验（含 check_auth_url/auth_url_targets/script_timeout 三个往返保真字段），白名单外 400 | 后端字段名 patch 返回 400 并列出合法字段；前端形态 patch 正常 |
| O-1 | `ProfileCreateBody.id` 改为 Option（路径 id 优先），两者皆空 400 | 不带 body.id 创建成功 |
| O-2 | import 全部未导入且无失败明细时 400 报格式原因；空数组 400 | 包裹层/缺 id/空数组均返回明确错误 |
| O-3 | 通知面板注册 document 级点击监听，点外部即关闭（`AppTopbar.vue`） | GUI：打开→点空白→关闭 |
| O-4 | `decode_netsh_ssid_hex` 还原 netsh 对非 ASCII SSID 的 hex 转义（UTF-8 + 含非 ASCII 可打印字符双重条件防误转），含 3 项单测 | detect 返回可读中文 SSID |
| 追加-1 | 调试会话状态可查询/可恢复：新增 `GET /api/debug/status`（active + 最近截图 URL），前端启动时自动恢复"失忆"的调试面板，可自助停止 | 开调试会话→刷新页面→面板自动恢复→停止→登录恢复可用 |
| 追加-2 | 调试面板截图预览：三层修复——`/api/debug/start` 下发 browser_settings（此前调试永远无头）；Bridge screenshot 事件 path→URL 映射 + 新增 `GET /api/debug/screenshot/{filename}`（只读、防穿越、GET 鉴权豁免）；`syncSession` 不再用响应 null 抹掉 WS 截图；`ws.rs` 内联目录统一回退解析；status 返回最近截图 URL 供刷新恢复 | 调试面板截图正常显示；刷新恢复后截图仍在；截图 HTTP 200 PNG、防穿越 400 |
| 追加-3 | 任务卡"执行"按钮替换为"调试"（用户决策）：打开调试面板单步执行；执行能力保留于 API 与调度路径 | 任务卡按钮为 使用/编辑/调试/复制/导出/删除 |
| 追加-4 | 调试面板 UI 重做 + 步骤数据恢复：Worker 新增 `debug_status` 无副作用查询命令（compat 白名单放行）；`/api/debug/status` 返回完整会话（步骤/结果），前端刷新后整体恢复；面板重排（任务徽章/状态 pill/序号化步骤卡片/截图标题区）；Modal 背景改用高不透明 `--bg-modal`（原 --bg-card 0.6/0.75 穿透底层文字）并加 backdrop blur | 刷新后面板恢复 5 步骤 + 截图 + 状态徽章；背景无穿透 |
