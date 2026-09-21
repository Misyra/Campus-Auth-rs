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

**命令级超时协商（BRG-1）**

`execute_*` 类命令的 params 由 Rust 侧注入两个保留字段：

- `cancel_id`：取消通知的匹配键（Worker 收到 `{"cancel": ...}` 时按它命中在途命令）；
- `rust_timeout_ms`：Rust 侧本次请求的超时预算（毫秒）。Worker 按
  `min(0.9 × 预算, max(270s, 单步超时 × 20))` 设置命令级自愈超时，保证
  Python 自愈先于 Rust 超时触发。字段缺省时回退固定兜底公式（旧主程序兼容）。

## 支持的命令

与 `playwright_worker.py` 的 `COMMANDS` 注册表逐项对应（14 项）。

| 命令 | 说明 |
|------|------|
| `worker_health_check` | Worker 进程级健康探测，上报运行时能力（`capabilities.ocr`，Rust 侧缓存并供 `/api/ocr/status` 消费） |
| `browser_health_check` | 浏览器/Worker 健康探测，未安装 Playwright 时返回 `healthy: false` |
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
| `ocr_recognize` | OCR 识别（需可选 `ocr` 依赖；与任意会话并发的轻量旁路，不占用单会话槽位） |
| `shutdown` | 优雅关闭 Worker 进程 |

> **会话互斥与轻量旁路**：`execute_login_attempt` / `execute_browser_task` 占用
> Worker 的**单会话槽位**；`ocr_recognize` / `feedback_capture` 属轻量旁路，可与任意
> 会话并发，且**不参与** Worker 回收时的全量取消（Rust 侧 `CancelRegistry` 为其
> 单独分区）。Rust → Worker 的命令始终串行执行（`_serve` 主循环 `await` 至该条命令
> 结束），故同一时刻只有一个命令真正操作浏览器。

## 依赖安装

```bash
# 仅核心（Playwright；OCR 不预声明，用时经应用内设置页 add/remove）
uv sync
```

> PyPI 镜像已配置为清华大学源以加速下载（见 `pyproject.toml` 的 `[tool.uv]`）。
