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

与 `playwright_worker.py` 的 `COMMANDS` 注册表逐项对应（17 项）。

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
| `ocr_recognize` | OCR 识别（需可选 `ocr` 依赖；不占单会话槽位、不被 WorkerBusy 拒绝，但 Worker 内仍串行排队执行） |
| `shutdown` | 优雅关闭 Worker 进程 |

> **会话互斥与轻量旁路**：`execute_login_attempt` / `execute_browser_task` 占用
> Worker 的**单会话槽位**；`ocr_recognize` / `feedback_capture` 不占会话槽位、
> 不被 WorkerBusy 拒绝，但 Worker 内仍串行排队执行（Rust → Worker 的命令始终
> 串行，`_serve` 主循环 `await` 至该条命令结束），且**不参与** Worker 回收时的
> 全量取消（Rust 侧 `CancelRegistry` 为其单独分区）。`profile_session_*` 与
> 自动化浏览器实例相互独立（独占 Playwright 连接，不共享 context/page，不占
> 单会话槽位），自带「同一时刻仅一个活跃配置会话」互斥（见下节）。

## 持久配置会话（profile_session_*）

「记住密码」模式的工作方式：门户登录页自带「记住密码」，凭据存
cookies/localStorage；把 `user_data_dir` 持久化后这些登录态跨进程保留，下次
打开门户页时**门户自己的 JS 会把账号密码默认填入**（纯页面行为，无头可用）。
因此自动化只需**录制登录点击**：

1. `profile_session_start`：开一次 headed 持久会话，用户手动登录（勾选门户
   记住密码），Worker 录下点击/选择序列；
2. `profile_session_stop`：关闭浏览器（登录态保留在 `user_data_dir`），取回
   录制结果；
3. 之后的自动登录（Rust 侧编排）：无头持久上下文打开门户页（门户 JS 自动
   回填）+ 回放录到的点击 + 网络验证兜底。

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
  `input[type=submit]`、`[role=button]`，`closest` 兼容按钮内嵌子元素）或
  `change`（仅 `<select>`，ISP 选择类，附 `value` 为选中值）；**输入类交互
  不录**——账号密码回填由门户自己的 JS 负责；
- `selector` / `strategy`：启发式按 `#id → [name=] → [data-testid] → 首个
  其他 data-* → 可见文本（button 文案，截断转义，Playwright 引号精确匹配
  形态 `text="..."`）→ 结构路径（nth-child 链，兜底必命中）`取首个命中，
  `strategy` 标明采用的级别；优先级顺序由 Python 侧
  `config_session.SELECTOR_STRATEGY_PRIORITY` 常量单一事实源驱动（同时注入
  录制器 JS 模板）；
- `ts`：事件时间戳（毫秒）；同一元素（同 type + 同 selector）500ms 内重复
  事件只记一次（窗口锚点为最近一条已记录动作，跨导航仍有效）；
- 动作序列按 `ts` 升序返回，上限 50 条（`RECORDER_MAX_ACTIONS`），触顶丢弃
  后续并在响应 `truncated: true` 标记。

录制机制：动作经 `expose_binding`（挂 BrowserContext 级，覆盖弹窗新页）实时
推给 Python 汇聚——页面 JS 状态随文档销毁（登录点击后必然跳转），binding 是
动作跨导航存活的唯一通道；录制脚本经 `add_init_script` 随每个新文档重装监听
（捕获相位注册，门户 JS `stopPropagation` 不影响录制）。JS 侧文档内触顶会向
Python 回传 `{"type": "truncated"}` 标记（文档计数随导航归零，需标记补齐才能
正确覆盖跨文档累计的丢弃），全局上限与 `truncated` 语义由 Python 侧权威收口。

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

## 依赖安装

```bash
# 仅核心（Playwright；OCR 不预声明，用时经应用内设置页 add/remove）
uv sync
```

> PyPI 镜像已配置为清华大学源以加速下载（见 `pyproject.toml` 的 `[tool.uv]`）。
