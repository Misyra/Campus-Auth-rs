"""Python Worker IPC 入口。

通过 stdin/stdout 与 Rust 主进程进行 NDJSON 协议通信：
- Rust → Worker：``{"id": N, "method": "...", "params": {...}}`` 命令
- Worker → Rust：``{"id": N, "result": {"success", "data", "error"}}`` 响应
- Rust → Worker：``{"cancel": "uuid"}`` 取消通知（无响应）
- Worker → Rust：``{"event": "...", "data": {...}}`` 事件推送（无 id）

设计要点：
- **不使用** ``select`` 模块（Windows 不支持 stdin 等非 socket fd）。
  改用独立守护线程阻塞读取 stdin，EOF 后设置全局 ``shutdown_event``。
- 主线程运行 asyncio 事件循环，串行执行命令（命令队列保证单 Worker 串行）。
- 取消通过 ``cancel_id`` 映射到 ``threading.Event``，处理器在步骤边界检查。
- 所有异常均在 IPC 层捕获，绝不让异常逃逸到 stdout（stdout 仅供 IPC）。
"""

from __future__ import annotations

import asyncio
import json
import logging
import os
import re
import sys
import threading
import time

# 标准库导入完成后，导入本 Worker 模块（其顶层导入 playwright 等重型依赖，
# 放在此处延后加载，避免 import 阶段即因缺失依赖而崩溃）
from models import Outcome  # noqa: E402
from playwright_worker import (  # noqa: E402
    COMMANDS,
    StepCancelled,
    WorkerError,
    _purge_stale_debug_screenshots,
    _to_ms,
    cancel_registry,
    worker_core,
)

logger = logging.getLogger(__name__)

# stdout 写入锁（保证响应与事件行不互相穿插）
_stdout_lock = threading.Lock()
# 全局关闭事件：stdin EOF 或收到 shutdown 命令时置位
shutdown_event = threading.Event()
# stdin 单行大小上限（字节）：超限视为异常/恶意载荷，直接丢弃该行，
# 避免超大 JSON 解析耗尽内存（P9）
_MAX_STDIN_LINE_BYTES = 16 * 1024 * 1024

# Worker 版本（与 pyproject.toml 的 project.version 保持同步，手动维护）
WORKER_VERSION = "5.0.0"

# OCR 运行时能力（任务 10）：由 _preload_ocr_deps 探测结果填充，
# 随 worker_health_check 响应上报给 Rust 侧缓存（/api/ocr/status 消费）。
# 仅 ddddocr 完整加载成功才视为 True；numpy-only/均缺失为 False
#（识别请求会在运行时报「ddddocr 未安装」）。
OCR_CAPABILITIES: dict[str, bool] = {"ocr": False}


def _preload_ocr_deps() -> None:
    """预加载 OCR 依赖到主线程，规避后台线程加载 C 扩展卡死（根因修复）。

    背景：``ddddocr`` 链式加载 numpy C 扩展（``numpy._core._multiarray_umath``
    及其 numpy.libs/ 下的 OpenBLAS DLL）时，若发生在 **Worker 的后台线程**
    （``asyncio.to_thread``），Windows 的 loader lock 与 Python import lock
    会让该加载卡住约 100 秒，超过 OCR_TIMEOUT_SECS 后前端报「模型加载超时」。

    已实测：同样依赖在 **主线程** 加载仅需 ~0.1s；且只要主线程预加载出现过一次，
    后续后台线程 ``import`` 直接命中 ``sys.modules`` 缓存，不再重新加载 DLL，
    后续识别全程正常（~0.5s）。

    本函数因此在 **主线程、事件循环启动前** 调用。best-effort：
    - OCR 依赖未安装（numpy/ddddocr 缺失）时静默跳过，不影响 Worker 正常启动，
      待安装完成后配合重启 Worker 使预加载生效；
    - 加载失败不抛异常，避免预加载拖垮 Worker 启动。

    探测结果同步写入模块级 ``OCR_CAPABILITIES``（任务 10）：
    完整加载 → ``{"ocr": True}``；numpy-only / 均不可用 → ``{"ocr": False}``。
    """
    # 探测前先复位（幂等）：仅完整加载成功才置 True
    OCR_CAPABILITIES["ocr"] = False
    # 主线程先完整加载 ddddocr（连带 numpy 等 C 扩展一并进入 sys.modules 缓存），
    # 使后续 to_thread 只命中缓存、不再触发 DLL 加载。若顶层 import 过重/异常，
    # 退化为仅加载 numpy——它正是卡死的那个 C 扩展，单独预加载即已命中根因。
    try:
        import ddddocr  # noqa: F401

        logger.info("OCR 依赖已预加载（完整）")
        OCR_CAPABILITIES["ocr"] = True
        return
    except Exception as exc:  # noqa: BLE001 — DLL/运行库损坏常表现为 OSError
        logger.warning(f"OCR 依赖完整预加载失败，尝试仅加载 numpy: {exc}")
    try:
        import numpy  # noqa: F401

        logger.info("OCR 依赖未完整安装，已预加载 numpy 核心")
    except Exception as exc:  # noqa: BLE001 — best-effort，不能拖垮非 OCR 登录
        logger.info(f"OCR 依赖不可用，跳过预加载: {exc}")


def _force_utf8_stdio() -> None:
    """强制 stdio 使用 UTF-8 编码。

    Windows 下管道重定向的 stdin/stdout/stderr 默认使用 ANSI 代码页
    （简体中文系统为 cp936），而 Rust 侧严格按 UTF-8 编解码 IPC 行：
    写方向会导致中文响应整行被 Rust 丢弃（无 id 可回收，在途请求只能超时），
    读方向会导致中文命令解码损坏甚至抛 UnicodeDecodeError。
    Python 3.15 才默认 UTF-8（PEP 686），故必须显式 reconfigure。
    stdin 用 errors="replace" 保证脏数据不致命（跳过而非崩溃）。
    """
    for name in ("stdin", "stdout", "stderr"):
        stream = getattr(sys, name)
        try:
            if stream is sys.stdin:
                stream.reconfigure(encoding="utf-8", errors="replace")
            else:
                stream.reconfigure(encoding="utf-8")
        except (AttributeError, ValueError):
            # reconfigure 失败时检查实际编码：stdout 非 UTF-8 则 IPC 必然乱码，
            # 静默继续只会让 Rust 侧持续丢弃响应行、在途请求全部超时——
            # 不如 fail-fast 让 Supervisor 立即感知并重启（M6）
            enc = (getattr(stream, "encoding", "") or "").lower().replace("-", "").replace("_", "")
            if name == "stdout" and enc not in ("", "utf8"):
                # 用最底层的 os.write 绕过可能同样损坏的 stdout/stderr
                os.write(2, f"[worker] FATAL: stdout encoding={enc} cannot be forced to utf-8, IPC would corrupt; exiting\n".encode("utf-8", "replace"))
                raise SystemExit(3)


def _configure_logging() -> None:
    """将日志输出到 stderr（stdout 仅供 IPC 使用）。"""
    level_name = os.getenv("WORKER_LOG_LEVEL", "INFO").upper()
    level = getattr(logging, level_name, logging.INFO)
    handler = logging.StreamHandler(sys.stderr)
    handler.setFormatter(
        logging.Formatter("%(asctime)s %(levelname)s [%(name)s] %(message)s")
    )
    root = logging.getLogger()
    root.handlers.clear()
    root.addHandler(handler)
    root.setLevel(level)


def emit_response(msg_id: int | None, result: dict) -> None:
    """向 stdout 写入命令响应。"""
    line = json.dumps({"id": msg_id, "result": result}, ensure_ascii=False)
    with _stdout_lock:
        sys.stdout.write(line + "\n")
        sys.stdout.flush()


def emit_event(event_type: str, data: dict) -> None:
    """向 stdout 推送事件（实际事件类型：step_progress / screenshot / dialog）。"""
    line = json.dumps({"event": event_type, "data": data}, ensure_ascii=False)
    with _stdout_lock:
        sys.stdout.write(line + "\n")
        sys.stdout.flush()


def _oversized_request_id(raw: str) -> int | None:
    """从超限请求的有界前缀提取 id，供 IPC 返回明确错误。

    Rust 的 ``IpcRequest`` 固定先序列化 ``id``；只检查前 512 个字符，避免对恶意
    超长输入执行无界正则。无法提取时返回 ``None``，该行仍会被丢弃并记录日志。
    """
    match = re.match(r'^\s*\{\s*"id"\s*:\s*(\d+)', raw[:512])
    return int(match.group(1)) if match else None


def stdin_reader(queue: asyncio.Queue, loop: asyncio.AbstractEventLoop) -> None:
    """守护线程：阻塞读取 stdin NDJSON，分发到队列或取消注册表。

    不使用 ``select``：直接 ``for line in sys.stdin`` 阻塞读取，
    Windows 上同样可靠。EOF 时设置 ``shutdown_event`` 并放入哨兵唤醒主循环。

    跨线程写入 asyncio.Queue 必须通过 ``loop.call_soon_threadsafe``，
    否则事件循环可能无法被唤醒（直接 ``put_nowait`` 不保证线程安全）。
    """
    def enqueue(item: object) -> None:
        loop.call_soon_threadsafe(queue.put_nowait, item)

    try:
        for raw in sys.stdin:
            # 单行大小上限（P9）：按字节数校验，超限直接丢弃，不尝试 JSON 解析
            if len(raw.encode("utf-8", errors="replace")) > _MAX_STDIN_LINE_BYTES:
                message = (
                    f"IPC 请求超过 {_MAX_STDIN_LINE_BYTES // (1024 * 1024)}MiB 上限"
                )
                logger.warning(f"{message}，拒绝解析")
                msg_id = _oversized_request_id(raw)
                if msg_id is not None:
                    emit_response(msg_id, _error_result(message))
                continue
            line = raw.strip()
            if not line:
                continue
            try:
                msg = json.loads(line)
            except json.JSONDecodeError:
                logger.warning(f"忽略非 JSON 的 stdin 行: {line[:200]}")
                continue
            # 合法 JSON 但非对象（字符串/数组/数字）时 `"cancel" in msg`
            # 会退化为子串判断、`msg["cancel"]` 会抛 TypeError，
            # 进而被外层 except 捕获导致整个 Worker 关闭——必须逐条拦截
            if not isinstance(msg, dict):
                logger.warning(f"忽略非 JSON 对象的 stdin 行: {line[:200]}")
                continue

            # 取消通知：无 id、无响应
            if "cancel" in msg:
                cancel_registry.trigger(str(msg["cancel"]))
                continue

            # 命令：含 id + method
            if "id" in msg and "method" in msg:
                enqueue(msg)
            # 其他形态忽略
    except Exception:  # noqa: BLE001
        logger.exception("stdin 读取异常")
    finally:
        shutdown_event.set()
        try:
            enqueue(None)  # 哨兵：唤醒主循环退出
        except Exception as exc:  # noqa: BLE001
            logger.debug("入队退出哨兵失败（事件循环可能已关闭）: %r", exc)


async def _dispatch(msg: dict) -> None:
    """分发单条命令并写回响应。"""
    msg_id = msg.get("id")
    method = msg.get("method")
    params = msg.get("params") or {}
    handler = COMMANDS.get(method)
    start = time.perf_counter()

    try:
        if handler is None:
            emit_response(msg_id, _error_result(f"未知命令: {method}"))
            return
        data = await handler(params)
        # 命令完成日志：debug 常规记录，超过 5s 的慢命令升为 info 便于观察
        duration_ms = int((time.perf_counter() - start) * 1000)
        if duration_ms > 5000:
            logger.info("命令 %s 完成，耗时 %dms", method, duration_ms)
        else:
            logger.debug("命令 %s 完成，耗时 %dms", method, duration_ms)
        emit_response(msg_id, {"success": True, "data": data, "error": None})
    except StepCancelled as exc:
        # 取消：视为成功终态（outcome=cancelled）
        emit_response(msg_id, _structured_result(exc, success=True, start=start))
    except WorkerError as exc:
        # 可分类失败：保留 outcome 供 Rust 侧决定重试/回收策略
        emit_response(msg_id, _structured_result(exc, success=False, start=start))
    except Exception as exc:  # noqa: BLE001
        logger.exception(f"命令 {method} 执行异常")
        emit_response(msg_id, _error_result(str(exc)))


def _structured_result(exc: WorkerError, *, success: bool, start: float | None = None) -> dict:
    """将 WorkerError / StepCancelled 归一为带 outcome 的 IPC 响应。

    ``start`` 为命令开始时刻（perf_counter），用于计算真实耗时；缺失时
    duration_ms 取 0（兼容直接调用）。
    """
    duration_ms = int((time.perf_counter() - start) * 1000) if start is not None else 0
    return {
        "success": success,
        "data": {
            "outcome": exc.outcome,
            "message": exc.message,
            "duration_ms": duration_ms,
            "screenshots": [],
        },
        "error": exc.message if not success else None,
    }


def _command_timeout(params: dict) -> float:
    """从命令参数推导命令级超时（秒）。

    固定兜底 270s = Rust 侧 ``execute``/``execute_with_timeout`` 默认 300s 的
    0.9 倍（B6 竞速修复）：两端同时起跑时 Python 侧必须**先于** Rust 超时
    触发轻量自愈（取消任务 + 关页打断挂起的 CDP await → 响应错误 → Rust 收到
    结果而非超时），否则同值 300s 下 Python 自愈基本抢不到，Rust 超时后还要
    走 Cancel + 10s 宽限 + 可能的强杀回收，代价高得多。
    0.9 倍预留 30s（≥10s 宽限期的 3 倍）确保自愈完成并回包。

    浏览器 settings 存在时以单步默认超时为基准放大（覆盖超大 step timeout
    配置），但不低于固定兜底；不新增协议字段（复用 ``_to_ms`` 语义）。
    """
    bs = params.get("browser_settings") or {}
    step_ms = _to_ms(bs, "timeout", 10000)
    return max(270.0, step_ms / 1000 * 20)


def _error_result(message: str) -> dict:
    """构造无 data 的错误响应（未知命令 / 未捕获异常 / 命令超时自愈）。

    P6：补 outcome 字段，保证与结构化响应结构一致（Rust 侧 failure 时仅读
    error 字段，此处补全 data 仅为协议一致性）。原 ``_timeout_result`` 与本
    函数逐字段相同，已合并（调用处以注释区分语义）。
    """
    return {
        "success": False,
        "data": {
            "outcome": Outcome.UNKNOWN_ERROR.value,
            "message": message,
            "duration_ms": 0,
            "screenshots": [],
        },
        "error": message,
    }


async def _dispatch_guarded(msg: dict) -> None:
    """分发单条命令，带命令级超时兜底。

    每条命令在独立 ``asyncio.Task`` 中执行；超时则取消任务并强制关闭当前页面，
    以中断挂起的 Playwright await（``page.evaluate``/``goto`` 挂在无限循环时
    仅取消协程无法打断 CDP 调用），随后按 ``UNKNOWN_ERROR`` 回错误响应，
    避免一条挂起命令永久堵死后续所有命令（A1 自愈）。
    """
    msg_id = msg.get("id")
    method = msg.get("method")
    params = msg.get("params") or {}
    timeout_s = _command_timeout(params)
    task = asyncio.ensure_future(_dispatch(msg))
    done, _pending = await asyncio.wait({task}, timeout=timeout_s)
    if task in done:
        exc = task.exception()
        if exc is not None:
            # _dispatch 已捕获绝大多数异常，此处仅兜底
            logger.error(f"命令 {method} 内部异常: {exc}")
        return
    # 超时：取消任务并强制中断挂起的 Playwright 操作
    logger.error(f"命令 {method} 超时（{timeout_s:.0f}s），强制中断自愈")
    task.cancel()
    try:
        await worker_core.force_interrupt_pending()
    except Exception:  # noqa: BLE001
        logger.exception("强制中断挂起操作失败")
    # 超时自愈错误响应（原 _timeout_result，与 _error_result 同构已合一）
    emit_response(msg_id, _error_result(f"命令 {method} 执行超时（{timeout_s:.0f}s）"))
    # 等待任务收敛：页面关闭后挂起的 Playwright await 应以“目标已关闭”异常结束
    try:
        await asyncio.wait_for(task, timeout=5.0)
    except asyncio.CancelledError:
        # 任务被本函数主动取消后的正常收敛路径，静默即可
        pass
    except Exception as exc:  # noqa: BLE001
        logger.debug("超时任务收敛异常: %r", exc)


async def _serve() -> None:
    """主服务循环：从队列取命令并分发，直到收到关闭信号。"""
    loop = asyncio.get_running_loop()
    queue: asyncio.Queue = asyncio.Queue()
    reader = threading.Thread(target=stdin_reader, args=(queue, loop), daemon=True)
    reader.start()

    # 注入事件推送、关闭事件与运行时能力到核心（任务 10：能力随
    # worker_health_check 响应上报给 Rust 侧缓存）
    worker_core.emit = emit_event
    worker_core.shutdown_event = shutdown_event
    worker_core.capabilities = dict(OCR_CAPABILITIES)

    logger.info("Worker 已启动，等待 Rust 侧命令")
    try:
        # 仅以哨兵 None 作为退出信号：EOF 会置位 shutdown_event（处理器可观测到），
        # 但主循环仍应排空队列中已到达的命令，避免丢失 stdin 关闭前最后一批指令。
        while True:
            msg = await queue.get()
            if msg is None:
                break
            method = msg.get("method")
            # 每条命令独立 Task + 超时兜底，避免一条挂起命令堵死后续命令（A1）
            await _dispatch_guarded(msg)
            if method == "shutdown":
                logger.info("收到 shutdown 命令，准备退出")
                break
        logger.info("Worker 主循环退出")
    finally:
        # 在原 event loop 中关闭浏览器，避免 Playwright 连接与新 loop 不兼容；
        # 加超时兜底，防止 close_browser 自身挂起阻塞退出（A1）
        try:
            await asyncio.wait_for(worker_core.close_browser(), timeout=5.0)
        except Exception:  # noqa: BLE001
            logger.warning("退出时关闭浏览器失败", exc_info=True)


def main() -> None:
    """Worker 进程入口。"""
    _force_utf8_stdio()
    _configure_logging()
    # A7：清理上次会话（进程被强杀）残留的截图文件（可能含明文凭据），best-effort
    _purge_stale_debug_screenshots()
    # 主线程、事件循环启动前预加载 OCR 依赖，规避后台线程加载 numpy C 扩展卡死
    _preload_ocr_deps()
    try:
        asyncio.run(_serve())
    except KeyboardInterrupt:
        logger.info("收到 KeyboardInterrupt，退出")
    logger.info("Worker 进程结束")


if __name__ == "__main__":
    main()
