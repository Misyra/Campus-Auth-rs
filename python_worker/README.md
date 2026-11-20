# Campus-Auth Python Worker

> Rust 侧对应模块 `src/bridge/`（`BridgeSupervisor` 通过 NDJSON IPC 驱动本 Worker），IPC 契约见本 README 与 `worker_main.py` docstring。
> Worker 协议版本：`v1.0.0`（与主程序发版版本独立）。本目录为 Rust 重写版的 Python Worker，已移除对旧项目 `app.*` 模块的全部依赖。

认证喵（Campus-Auth）的浏览器自动化子进程，由 Rust 主进程（控制平面）按需拉起、
空闲超时后关闭。

## 设计要点

- **不使用 `select` 模块**：Windows 不支持对 stdin 等非 socket fd 使用
  `select`。改为独立守护线程阻塞读取 stdin，EOF 后设置全局关闭事件。
- **主线程运行 asyncio 事件循环**，命令串行执行（命令队列保证单 Worker 串行）。
- **跨线程安全**：守护线程通过 `loop.call_soon_threadsafe` 写入 asyncio 队列，
  确保事件循环被正确唤醒。
- **取消机制**：Rust 侧发送 `{"cancel": "uuid"}` 通知，映射到 `threading.Event`，
  处理器在步骤边界检查，取消返回 `outcome: "cancelled"`。
- **异常隔离**：所有异常均在 IPC 层捕获并转为 `success: false` 响应，绝不逃逸到
  stdout（stdout 仅供 IPC 使用，日志走 stderr）。

## 运行方式

```bash
# 由 Rust 主进程作为子进程拉起（标准用法）
python worker_main.py

# 日志级别通过环境变量控制
WORKER_LOG_LEVEL=DEBUG python worker_main.py
```

`worker_main.py` 启动后阻塞等待 stdin 命令，收到 `shutdown` 命令或 stdin EOF 时
干净退出并释放浏览器资源。

## 模块结构（伴生模块范式）

`playwright_worker.py` 是**组装门面**：按职责拆分的伴生模块以 Mixin 形式
多继承回 `WorkerCore`，命令注册表 `COMMANDS` 在此收敛为单一事实源，并向
`worker_main` / tests / Rust 环境探针统一再导出符号。拆分纪律与既有的纯逻辑
伴生模块（`models` / `step_handlers` / `ocr_runtime` / `debug_session` /
`config_session` / `auto_login` / `variable_resolver`）一致：方法签名与行为
逐字段保留，只搬移与接线。

| 文件 | 职责 |
|------|------|
| `worker_main.py` | IPC 入口：stdin 读取线程、命令分发与超时自愈（`_dispatch_guarded`）、响应信封脱敏 |
| `playwright_worker.py` | 组装门面：`WorkerCore`（`__init__` 状态拓扑 + 健康检查/shutdown）、`COMMANDS` 注册表（18 条）、`WORKER_VERSION`、符号再导出 |
| `cancel_registry.py` | 取消注册表（cancel_id → Event，跨线程；含「取消先于注册到达」pending 语义） |
| `worker_paths.py` | 产物路径锚定（debug 截图 / 反馈快照 / AI 捕获 / 浏览器持久数据）与启动残留清理 |
| `browser_detect.py` | 浏览器环境探测（registry + 安装完成标记；语义与 Rust `environment::browser_registry` 一致） |
| `browser_launch.py` | 上下文准备：启动参数构建（黑名单过滤）、launcher 解析、stealth/直连/低资源路由、`_to_ms` 超时契约换算、`_channel_supports_cdp` 渠道判定 |
| `browser_lifecycle.py` | 浏览器生命周期：start/close/keep_alive 三档回收/空闲回收/生命周期锁、页面接线与弹窗自动确认（dialog 事件） |
| `step_engine.py` | 步骤执行引擎：`run_steps`、失败归一（`_normalize_step_failure`）、首导航地址解析 |
| `task_runner.py` | 任务执行：`_run_task`（首导航三级取值、success_condition 真值判定、按最终 outcome 的 cookie 清理矩阵）、`execute_*` 命令、系统变量注入 |
| `debug_commands.py` | 调试会话：debug_* 命令编排、步骤补拍截图、任务截图延迟清理 |
| `profile_commands.py` | 持久配置会话与自动登录：`profile_session_*` / `profile_auto_login` 命令编排（占用注册表/幽灵收尾/user_data_dir 只写不清） |
| `capture_kit.py` | 页面结构提取（跨 shadow DOM 控件摘要 + 脱敏局部 HTML）与 CSS/JS 资源快照（CDP 优先 + HTTP 回补兜底） |
| `capture_commands.py` | 临时浏览器捕获类命令：`test_redirect`（结果判定）、`page_capture`（meta.json 落盘）、`feedback_capture` |
| `ocr_commands.py` | `ocr_recognize` 命令编排（模型加载 + 推理共享 `OCR_TIMEOUT_SECS` 总预算，阶段级取消） |
| `ocr_runtime.py` | OCR 运行时：模型实例缓存、图片预处理、依赖主线程预加载与能力探测（`OCR_CAPABILITIES` 单一事实源） |

> 补丁语义约定：测试 monkeypatch 一律打在**属主模块**上（如
> `step_handlers.run_step_async`、`worker_paths._debug_screenshot_dir`、
> `browser_lifecycle._WORKER_KEEP_ALIVE`）；`playwright_worker` 的再导出仅保证
> 导入面兼容，不保证可替换语义。

## IPC 协议

**Rust → Worker（命令）**

```json
{"id": 1, "method": "browser_health_check", "params": {}}
```

**Worker → Rust（响应）**

```json
{"id": 1, "result": {"success": true, "data": {"healthy": true}, "error": null}}
```

**Rust → Worker（取消通知，无响应）**

```json
{"cancel": "uuid-xxxx"}
```

**Worker → Rust（事件推送，无 id）**

```json
{"event": "step_progress", "data": {"step_id": "s1", "status": "running"}}
```

其中 `command_started` 为内部信令事件（不进 WebSocket 转发白名单、不留痕）：
Worker 主循环从命令队列取出带数字 `id` 的命令、即将开始执行（命令级超时守卫
同时起跑）时发出，每命令恰好一次：

```json
{"event": "command_started", "data": {"id": 1}}
```

Rust 侧收到后把该请求的超时截止时刻重置为「now + 完整预算」（BRG-1b）——
FIFO 排队时间不再吃掉执行预算，与 BRG-1 的 `0.9 × 预算` Python 守卫在排队
场景重新对齐；`id=0` shutdown 哨兵同样发出（Rust 侧无注册项即忽略）。旧 Rust
忽略未知事件，双向向后兼容。

**命令级超时协商（BRG-1）**

`execute_*` 类命令的 params 由 Rust 侧注入两个保留字段：

- `cancel_id`：取消通知的匹配键（Worker 收到 `{"cancel": ...}` 时按它命中在途命令）；
- `rust_timeout_ms`：Rust 侧本次请求的超时预算（毫秒）。Worker 按
  `0.9 × 预算` 设置命令级自愈超时（全程权威，不与兜底公式取 min），保证
  Python 自愈先于 Rust 超时触发。字段缺省时回退固定兜底公式
  `max(270s, 单步超时 × 20)`（旧主程序兼容）。

## 支持的命令

正式登录、调试与重定向检测统一读取 Rust 注入的 `browser_settings.network_policy`：`mode=direct` 绕过系统代理，`mode=system` 允许系统代理，`mode=proxy` 使用 `proxy` 对象（`server`、`username`、`password`）。指定网卡由 Rust 提供带随机凭据的回环代理，Worker 不自行缓存或选择网卡。策略适用于纯净和持久会话；Chromium 使用启动参数，Firefox 使用偏好，其他引擎不注入 Chromium 参数。

缺少统一策略时继续兼容旧的 `bind_proxy` 和 `redirect_test_use_proxy`；新主程序不再暴露两个独立代理入口。自定义浏览器参数中的代理相关标志会被过滤，不能覆盖统一出口。Worker 不修改系统代理或 DNS；TUN 防火墙的强制拦截仍需用户配置例外。

与 `playwright_worker.py` 的 `COMMANDS` 注册表逐项对应（18 项）。

| 命令 | 说明 |
|------|------|
| `worker_health_check` | Worker 进程级健康探测，上报运行时能力（`capabilities.ocr`，Rust 侧缓存并供 `/api/ocr/status` 消费） |
| `browser_health_check` | 浏览器/Worker 健康探测，浏览器二进制缺失时返回 `healthy: false`（附 `missing` 组件明细；Playwright Python 包缺失时 Worker 进程根本无法启动，该场景由环境引导闭环兜底，不会走到本命令） |
| `test_redirect` | 用独立可见的临时浏览器验证触发地址能否到达校园网认证页；优先遵循统一 `network_policy`；缺省时兼容 `browser_settings.redirect_test_use_proxy`（默认 `false`）：忽略 `bind_proxy`，Chromium 系显式禁用系统代理（纯净模式同样生效）；设为 `true` 时保留绑定代理及浏览器默认系统代理行为。不修改全局代理或正式登录设置；TUN / VPN 路由需用户配置直连或临时暂停。params：`trigger_url`（必需，http/https）、`browser_settings`、`cancel_id`；返回 `{"status": "detected" \| "online" \| "not_detected"}`（刻意不含最终 URL 与页面正文，避免门户 token 进入 IPC） |
| `execute_login_attempt` | 执行一次登录尝试（按 TaskConfig 的步骤序列） |
| `execute_browser_task` | 执行通用浏览器任务（自定义步骤序列） |
| `close_browser` | 登录会话终态后的浏览器资源回收（三种档位见 `preserve_state` / `CAMPUS_AUTH_WORKER_KEEP_ALIVE`） |
| `debug_start` | 启动调试会话，保持页面上下文 |
| `debug_step` | 执行调试会话中的单步 |
| `debug_run_all` | 执行调试会话中从指定下标起的全部步骤 |
| `debug_stop` | 停止调试会话并释放页面 |
| `debug_status` | 查询调试会话详情（无副作用，供前端刷新后恢复步骤数据） |
| `feedback_capture` | 捕获当前调试页的完整 MHTML、截图与 CSS/JS 资源（供导出离线问题报告） |
| `page_capture` | 清理旧登录态后导航到目标页，落盘 MHTML（仅 Chromium 渠道，走 CDP）、原始 HTML、离线副本 `page.offline.html`（资源引用改写为 `resources/`，解压即可脱网还原）、结构化控件摘要与脱敏局部 HTML、CSS-JS 资源及截图到 `captures/latest/`（**非 Chromium 渠道无 CDP**，CSS/JS 改由页面枚举 + HTTP 回补抓取；超大全页截图自动降为视口截图；由 `POST /api/ai/capture` 触发） |
| `profile_session_start` | 启动持久配置会话（headed 持久上下文，详见[持久配置会话](#持久配置会话profile_session_)）：打开门户页供用户手动登录（勾选门户「记住密码」），注入录制器录下登录点击/选择序列 |
| `profile_session_stop` | 停止配置会话并关闭持久浏览器，返回录制动作；**user_data_dir 的 cookie/localStorage 原样保留，绝不清理**（详见[持久配置会话](#持久配置会话profile_session_)） |
| `profile_auto_login` | 持久自动登录（无头持久上下文执行器，详见[持久自动登录](#持久自动登录profile_auto_login)）：打开门户页等「记住密码」JS 自动回填 + 回放 `recorded_actions` + 结构化结果；登录成败以 Rust 侧网络验证兜底 |
| `ocr_recognize` | OCR 识别（需可选 `ocr` 依赖；不占单会话槽位、不被 WorkerBusy 拒绝，但 Worker 内仍串行排队执行） |
| `shutdown` | 优雅关闭 Worker 进程 |

> **会话互斥与轻量旁路**：`execute_login_attempt` / `execute_browser_task` 占用
> Worker 的**单会话槽位**；`ocr_recognize` / `feedback_capture` 不占会话槽位、
> 不被 WorkerBusy 拒绝，但 Worker 内仍串行排队执行（Rust → Worker 的命令始终
> 串行，`_serve` 主循环 `await` 至该条命令结束），且**不参与** Worker 回收时的
> 全量取消（Rust 侧 `CancelRegistry` 为其单独分区）。`profile_session_*` 与
> `profile_auto_login` 与自动化浏览器实例相互独立（独占 Playwright 连接，不共享
> context/page，不占单会话槽位），三者共用「同一时刻仅一个持久类操作」占用
> 注册表（配置会话进行中时 auto_login 报占用，反之亦然）。

## 持久配置会话（profile_session_*）

「记住密码」模式的工作方式：门户登录页自带「记住密码」，凭据存
cookies/localStorage；把 `user_data_dir` 持久化后这些登录态跨进程保留，下次
打开门户页时**门户自己的 JS 会把账号密码默认填入**（纯页面行为，无头可用）。
因此自动化**录制完整操作序列**（点击/选择 + 文本输入 + 页面导航；凭据类
输入以 `{{USERNAME}}`/`{{PASSWORD}}` 占位符录制，密码永不落明文）：

1. `profile_session_start`：开一次 headed 持久会话，用户手动登录（勾选门户
   记住密码），Worker 录下点击/选择/输入/导航序列；
2. `profile_session_stop`：关闭浏览器（登录态保留在 `user_data_dir`），取回
   录制结果；
3. 之后的自动登录（`profile_auto_login`）：无头持久上下文打开门户页（门户 JS
   自动回填）+ 回放录制序列（凭据占位符由下发的凭据参数解析）+ 网络验证兜底
   （Rust 侧编排）。

### 请求 / 响应契约

**`profile_session_start`**

- params：`user_data_dir`（**必需**，Rust 侧给的持久目录，缺失报错）、
  `portal_url`（可选，给则打开，仅 http/https）、`record`（默认 `true`，
  非布尔值报错）、`browser_settings`（可选，资源/直连/stealth 口径对齐
  `_start_browser`：未绑定代理即追加 `--no-proxy-server`，低资源路由与反检测
  脚本作用于配置会话自己的上下文）、`cancel_id`（可选，只覆盖启动命令本身）；
- 返回：`{"session_id": "<uuid>", "recorded": true|false}`；
- 浏览器恒为 **headed**（`headless=False`），持久上下文即浏览器本体；
- 错误形态：参数缺失/非法与「已存在活跃配置会话」均以
  `outcome: unknown_error` 的结构化错误返回（不重试语义，与 debug_start 占用
  口径一致）；门户页导航失败同样报错并整场回收（不留半开会话）。

**`profile_session_stop`**

- params：`session_id`（可选；单会话语义下为空时回退唯一活跃会话，与活跃
  会话不匹配报错）；
- 返回：`{"recorded_actions": [...], "truncated": bool}`（`record=false` 或
  无录制时空数组）；
- 无活跃会话时 stop 报错（显式契约，不做静默幂等，对齐 debug_stop）。

### 录制契约（recorded_actions 元素）

- `type`：`click`（按钮/提交类点击，捕获范围 `button`、`[type=submit]`、
  `input[type=submit]`、`[role=button]`，`closest` 兼容按钮内嵌子元素）、
  `change`（仅 `<select>`，ISP 选择类，附 `value` 为选中值）、`fill`
  （文本输入：`input[type=text|password|tel|email|number|search|url]` 与
  `textarea`，contenteditable 不录，附 `value`）或 `navigate`（页面导航标记：
  每个新文档上报一条，`value` 为落地地址 `location.href`，`selector` 恒为
  空串、`strategy` 恒为 `structure`；仅主框架且非 about:blank 才上报）；
- `fill` 的 `value` 占位符语义（密码永不落明文）：`input[type=password]`
  恒为 `{{PASSWORD}}`（**绝不捕获真实输入值**）；`autocomplete="username"`
  或 name/id/placeholder/aria-label 命中账号类关键词（user/email/account/
  mobile/phone/学号/账号/用户名，大小写不敏感，`RECORDER_USERNAME_HINTS`
  单一事实源）的文本框恒为 `{{USERNAME}}`；其余文本框录实际值字面量（空值
  不上报）。回放侧从命令参数的 `username` / `password` / `isp` / `auth_url`
  （与浏览器任务执行完全同名）构建系统变量映射，经 `variable_resolver`
  解析 fill/navigate 的占位符（`build_replay_variables`，未命中保留原样）；
- 输入捕获与结算：`input` 事件空闲防抖合并——同一输入框在 `DEDUP_MS`
  （500ms）空闲窗口内连续输入只产生一条待结算值（取最终值，不逐键外发）；
  blur / change / 回车提交 / 点击提交按钮时结算（回车提交不触发 blur，
  文档随导航销毁前由 Enter/click 兜底结算，保证「输入 → 提交」时序）；
- `selector` / `strategy`：启发式按 `#id → [name=] → [data-testid] → 首个
  其他 data-* → 可见文本（button 文案，截断转义，Playwright 引号精确匹配
  形态 `text="..."`）→ 结构路径（nth-child 链，兜底必命中）`取首个命中，
  `strategy` 标明采用的级别；优先级顺序由 Python 侧
  `config_session.SELECTOR_STRATEGY_PRIORITY` 常量单一事实源驱动（同时注入
  录制器 JS 模板）；
- 去重口径：`click` / `change` 同元素（同 type + 同 selector）500ms 内重复
  事件只记一次（窗口锚点为最近一条已记录动作，跨导航仍有效）；`fill` /
  `navigate` 与最近一条已记录动作同 (type, selector) 时**就地合并取最终值**
  （fill 取输入会话最终值——值不同不可按窗口丢弃；navigate 取重定向链最终
  落地页），不受窗口约束；
- 动作序列按 `ts` 升序返回，上限 50 条（`RECORDER_MAX_ACTIONS`），触顶丢弃
  后续并在响应 `truncated: true` 标记。

录制机制：动作经 `expose_binding`（挂 BrowserContext 级，覆盖弹窗新页）实时
推给 Python 汇聚——页面 JS 状态随文档销毁（登录点击后必然跳转），binding 是
动作跨导航存活的唯一通道；录制脚本经 `add_init_script` 随每个新文档重装监听
（捕获相位注册，门户 JS `stopPropagation` 不影响录制）。JS 侧文档内触顶会向
Python 回传 `{"type": "truncated"}` 标记（文档计数随导航归零，需标记补齐才能
正确覆盖跨文档累计的丢弃），全局上限与 `truncated` 语义由 Python 侧权威收口。

### 录制悬浮面板（实时步骤列表 + 单步删除）

录制脚本同批注入一个悬浮面板（方案页配置录制 / 任务页录制新任务两条录制
用途共用，回放侧无需感知）：

- **形态**：固定右下角、可折叠（默认展开；折叠态仅存页面内存、不跨页传递
  ——门户登录必然跳页，每页默认展开让用户始终看得见实时列表），内联样式
  全套（与页面 CSS 隔离），z-index 2147483647，半透明深色底，宽 ≤280px，
  列表高上限滚动；仅主框架渲染（iframe 不重复出现，iframe 里的动作仍照常
  录制并出现在主框架面板中，删除按动作身份寻址、与来源 frame 无关）；
- **实时列表**：捕获管线入列即同步镜像——列表项 = 序号 + 类型徽标（点击/
  选择/输入/跳转）+ 摘要（click 显示 selector；change 显示 selector = 选中值；
  fill 显示 value @ selector，`{{PASSWORD}}`/`{{USERNAME}}` 占位符原样显示；
  navigate 显示落地 URL），防抖待结算中的输入以「输入中」条目先行显示，
  结算后转为正式条目；头部「共 N 步」为权威总计数。跨页口径：页面 JS 无法
  主动拉取汇聚器状态，`expose_binding` 的**返回值**是唯一回程通道（回调即
  `RecorderSink.add_json` 本体）——汇聚器处理每条上报/删除后把权威**全量
  列表**（含跨页累计，≤50 条，人力节奏下体积可忽略）经返回值回推，面板
  显示全量列表 + 权威计数；binding 失联时退回本页乐观镜像（此时上报本身
  也失联，仅可见性降级）；
- **单步删除语义**：每项右侧 × 按钮（aria-label「删除第 N 步」）——未上报
  （防抖待结算）的输入直接清页面缓冲（无控制事件）；已上报的经 binding
  上报 `{type:"__delete__", value:"<ts>", action_type, selector}` 控制事件，
  汇聚器按 (ts, action_type, selector) 身份删除对应动作并经返回值回推最新
  全量列表（面板头部计数与列表即时刷新）。`__delete__` 本身不进
  `recorded_actions` 输出；删除后其余动作保持原序（不重排，回放按 ts 升序）；
  `truncated` 不因删除复位（发生过触顶丢弃的事实不可逆），删除后计数下降、
  上限容量自然重开；去重/合并锚点重算为当前最近一条已记录动作（既有去重/
  合并口径不受影响）；
- **面板自防**：面板根节点所有事件 `stopPropagation`（拦门户 JS 的冒泡监听），
  且录制器 click/fill/change/keydown/blur 捕获监听显式忽略事件源在面板内的
  目标——document 捕获相位先于面板根触发，仅 stopPropagation 拦不住录制器
  自身，两层防线确保面板按钮绝不被录成动作。

### 生命周期与收尾

- **命令级超时只覆盖 start/stop 命令本身**；会话存续期（start 返回后到 stop
  之间）不处于任何命令执行窗口内，不受命令级超时约束（对齐 debug session：
  会话生命周期独立于单条命令预算），也不进 `_INTERRUPTIBLE_METHODS` 强制中断
  白名单（独立上下文，关 `self._page` 既打不着它也会误伤 keep_alive 常驻
  会话）；
- **占用互斥**：同一时刻仅允许一个活跃配置会话，重复 `profile_session_start`
  报「已存在活跃配置会话」；浏览器进程意外退出的幽灵会话（`is_closed()` 已
  真或 close 事件已到）在下次 start 时自动收尾后放行；
- **幽灵会话收尾**：客户端断连 / shutdown 随 Worker 退出
  （`worker_main._serve` 收尾段调 `close_profile_session`）一并收尾；浏览器
  进程意外退出经 close 事件触发收尾——两条路径都只关浏览器与独占 Playwright
  连接，**绝不清理 user_data_dir**（目录里的 cookie/localStorage 就是
  「记住密码」机制的全部价值）；
- **与自动化回收完全独立**：`close_browser` 命令 / 浏览器空闲回收 /
  ensure_browser 重建回滚只管自动化浏览器，不触碰配置会话；
- stop 后或收尾后可再次 start（`session_id` 每次重新生成）。

## 持久自动登录（profile_auto_login）

「记住密码」自动登录的 Worker 侧执行器（编排由 Rust 侧负责）：无头持久上下文
打开门户页 → 门户自己的 JS 自动回填账号密码（纯页面行为）→ 逐条回放
`recorded_actions`（fill/navigate 的占位符 value 由下发的凭据参数解析）→
返回结构化结果。**本命令的成功 = 回放全部动作无错误且页面完成导航/加载沉降；
登录是否真的成功以 Rust 侧登录后网络验证兜底**（非本命令职责）。

### 请求 / 响应契约

- params：`user_data_dir`（**必需**，持久目录，缺失报错）、`portal_url`
  （**必需**，仅 http/https）、`recorded_actions`（数组，元素
  `{type, selector, strategy, ts, value?}` 即 `profile_session_stop` 的
  `recorded_actions` 原样回传；**空数组 = 只打开门户页等回填不点击**——门户
  可能记忆了会话直接放行成功页）、`username` / `password` / `isp` /
  `auth_url`（可选，凭据参数键与浏览器任务执行完全同名——fill/navigate 的
  `{{USERNAME}}`/`{{PASSWORD}}`/`{{ISP}}`/`{{LOGIN_URL}}` 占位符解析源，
  缺失时占位符保留原样）、`browser_settings`（可选，直连/资源路由/
  stealth 口径对齐 `_start_browser`，作用于本命令自己的持久 context）、
  `cancel_id`（可选，覆盖本命令全程）；
- 行为：**headless** `launch_persistent_context(user_data_dir)`（与配置会话的
  headed 区分）→ `goto(portal_url)`（导航超时口径同既有分类
  `navigation_timeout` / `network_error`）→ **回填沉降**（`load` 态等待 5s
  best-effort + 固定沉降 1.5s，常量在 `auto_login.py`，窗口可测）→ 逐条回放
  （见下）→ **回放后沉降**（给提交导航留落地窗口，best-effort 不判失败）→
  关浏览器返回；命令级超时（`rust_timeout_ms` 的 0.9×）覆盖本命令全程。
- 返回：沿用 `_structured_result` 信封语义——
  - 成功：`success=true`，`data.outcome="success"` + 人话 `message` +
    `duration_ms`（StructuredResult 形状，与 `execute_login_attempt` 一致）；
  - 回放步骤失败：`success=false`，`data.outcome="assertion_failed"`，message
    带动作定位（第 N/M 个 + type + selector）与原始失败原因（Rust 侧可重试、
    不回收 Worker）；
  - 取消：`success=true`，`data.outcome="cancelled"`；
  - 参数缺失/非法、`recorded_actions` 畸形（非数组 / 非对象元素 / 不认识的
    type / `change` 缺非空 value / 超上限 200 条）：浏览器启动**之前**即
    `success=false` + `data.outcome="unknown_error"`（不重试语义）。

### 回放映射（复用既有步骤处理器，不另写执行器）

- `click` → `click` 处理器：selector 直接用录制串（Playwright 兼容形态，
  含 `text="..."` / 结构路径），候选拆分、隐藏降级、文本兜底全部复用；
- `change` → `select` 处理器：按录制 value 做 option 精确值匹配 → 文本匹配
  兜底，最终 `select_option`；
- `fill` → `input` 处理器（`clear` 默认 True → `locator.fill` 整值填充）：
  value 先经凭据变量映射解析占位符（`build_replay_variables` →
  `variable_resolver.resolve`）；
- `navigate` → `navigate` 处理器（`goto`，domcontentloaded 等待与门户首导航
  同口径；load 级沉降不落在单步上——慢资源门户到不了 load 会把「登录其实已
  成功」的回放打成失败，load 级等待由回放后既有沉降窗口承担）：value 同样
  解析占位符；**序列首条 navigate 与 `portal_url` 相同则跳过**（回放主流程
  已先打开门户页并完成回填沉降，重复 goto 会重载门户页冲掉沉降成果；跳过
  以 `wait(0)` 占位表达，保持动作 ↔ 步骤 1:1 对齐供错误消息定位）；
- 每步预算沿用上下文默认超时（`browser_settings.timeout`，秒）与取消契约，
  失败即中止回放（后续动作依赖前序语义）。

### 生命周期

- **占用互斥**：与 `profile_session_*` **共用同一占用注册表**（同一时刻仅
  一个持久类操作；配置会话进行中报「已存在活跃配置会话」，幽灵会话自动收尾
  后放行）；
- **持久目录只写不清**：绝不清理 `user_data_dir` 的 cookies/localStorage
  （正常完成 / 失败 / 取消 / 幽灵回收 / Worker 退出兜底一律原样保留）；
- **幽灵收尾对齐配置会话范式**：浏览器进程意外退出经 close 事件回收槽位，
  命令失败/取消整场回收（不留半开浏览器）；
- 不进 `_INTERRUPTIBLE_METHODS` 强制中断白名单：本命令运行在独立上下文上
  （关 `self._page` 既打不着它也会误伤 keep_alive 常驻会话），超时自愈靠
  任务取消 + 退出兜底收尾（豁免口径同 `profile_session_*`）。

## 依赖安装

```bash
# 仅核心（Playwright；OCR 不预声明，用时经应用内设置页 add/remove）
uv sync
```

> PyPI 镜像已配置为清华大学源以加速下载（见 `pyproject.toml` 的 `[tool.uv]`）。
