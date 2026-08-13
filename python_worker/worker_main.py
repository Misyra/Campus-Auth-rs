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
import sys
import threading

# 标准库导入完成后，导入本 Worker 模块（其顶层导入 playwright 等重型依赖，
# 放在此处延后加载，避免 import 阶段即因缺失依赖而崩溃）
from playwright_worker import (  # noqa: E402
    COMMANDS,
    StepCancelled,
    WorkerError,
    cancel_registry,
    worker_core,
)

logger = logging.getLogger(__name__)

# stdout 写入锁（保证响应与事件行不互相穿插）
_stdout_lock = threading.Lock()
# 全局关闭事件：stdin EOF 或收到 shutdown 命令时置位
shutdown_event = threading.Event()


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
    """向 stdout 推送事件（step_progress / screenshot / log）。"""
    line = json.dumps({"event": event_type, "data": data}, ensure_ascii=False)
    with _stdout_lock:
        sys.stdout.write(line + "\n")
        sys.stdout.flush()


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
            line = raw.strip()
            if not line:
                continue
            try:
                msg = json.loads(line)
            except json.JSONDecodeError:
                logger.warning(f"忽略非 JSON 的 stdin 行: {line[:200]}")
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
        except Exception:  # noqa: BLE001
            pass


async def _dispatch(msg: dict) -> None:
    """分发单条命令并写回响应。"""
    msg_id = msg.get("id")
    method = msg.get("method")
    params = msg.get("params") or {}
    handler = COMMANDS.get(method)

    try:
        if handler is None:
            emit_response(msg_id, _error_result(f"未知命令: {method}"))
            return
        data = await handler(params, worker_core)
        emit_response(msg_id, {"success": True, "data": data, "error": None})
    except StepCancelled as exc:
        # 取消：视为成功终态（outcome=cancelled）
        emit_response(msg_id, _structured_result(exc, success=True))
    except WorkerError as exc:
        # 可分类失败：保留 outcome 供 Rust 侧决定重试/回收策略
        emit_response(msg_id, _structured_result(exc, success=False))
    except Exception as exc:  # noqa: BLE001
        logger.exception(f"命令 {method} 执行异常")
        emit_response(msg_id, _error_result(str(exc)))


def _structured_result(exc: WorkerError, *, success: bool) -> dict:
    """将 WorkerError / StepCancelled 归一为带 outcome 的 IPC 响应。"""
    return {
        "success": success,
        "data": {
            "outcome": exc.outcome,
            "message": exc.message,
            "duration_ms": 0,
            "screenshots": [],
        },
        "error": exc.message if not success else None,
    }


def _error_result(message: str) -> dict:
    """构造无 data 的错误响应（未知命令 / 未捕获异常）。"""
    return {"success": False, "data": None, "error": message}


async def _serve() -> None:
    """主服务循环：从队列取命令并分发，直到收到关闭信号。"""
    loop = asyncio.get_running_loop()
    queue: asyncio.Queue = asyncio.Queue()
    reader = threading.Thread(target=stdin_reader, args=(queue, loop), daemon=True)
    reader.start()

    # 注入事件推送与关闭事件到核心
    worker_core.emit = emit_event
    worker_core.shutdown_event = shutdown_event

    logger.info("Worker 已启动，等待 Rust 侧命令")
    try:
        # 仅以哨兵 None 作为退出信号：EOF 会置位 shutdown_event（处理器可观测到），
        # 但主循环仍应排空队列中已到达的命令，避免丢失 stdin 关闭前最后一批指令。
        while True:
            msg = await queue.get()
            if msg is None:
                break
            await _dispatch(msg)
            if msg.get("method") == "shutdown":
                logger.info("收到 shutdown 命令，准备退出")
                break
        logger.info("Worker 主循环退出")
    finally:
        # 在原 event loop 中关闭浏览器，避免 Playwright 连接与新 loop 不兼容
        try:
            await worker_core.close_browser()
        except Exception:  # noqa: BLE001
            pass


def main() -> None:
    """Worker 进程入口。"""
    _configure_logging()
    try:
        asyncio.run(_serve())
    except KeyboardInterrupt:
        logger.info("收到 KeyboardInterrupt，退出")
    logger.info("Worker 进程结束")


if __name__ == "__main__":
    main()
