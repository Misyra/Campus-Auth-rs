"""取消注册表：cancel_id → threading.Event 的跨线程映射（WorkerCore 拆分）。

自 playwright_worker.py 迁出。Rust 侧取消通知（``{"cancel": "uuid"}``）经
worker_main.stdin_reader 触发本注册表，命令处理器在步骤边界轮询对应的
Event。模块级单例 ``cancel_registry`` 与 WorkerCore 方法、stdin 读取线程
共享同一实例——Worker 全局只有这一份。
"""

from __future__ import annotations

import threading


class CancelRegistry:
    """cancel_id → threading.Event 的线程安全映射。

    支持"取消先于注册到达"的场景：若 cancel 到达时对应事件尚未注册，
    记录为 pending，注册时立即置位。
    """

    def __init__(self) -> None:
        self._lock = threading.Lock()
        self._events: dict[str, threading.Event] = {}
        # dict 当有序集合用：键为到达顺序，逐项淘汰最旧需要保序（P2-7）
        self._pending: dict[str, None] = {}

    # pending 集合上限：限制从未被注册的 cancel_id 永久堆积（历史遗留 F10）
    _MAX_PENDING = 256

    def register(self, cancel_id: str) -> threading.Event:
        """注册 cancel_id，返回对应的 Event（若此前已触发取消则立即置位）。"""
        ev = threading.Event()
        with self._lock:
            if cancel_id in self._pending:
                self._pending.pop(cancel_id, None)
                ev.set()
            self._events[cancel_id] = ev
        return ev

    def trigger(self, cancel_id: str) -> None:
        """触发取消：置位对应 Event，或记录 pending。"""
        with self._lock:
            ev = self._events.get(cancel_id)
            if ev is not None:
                ev.set()
            else:
                # 记录 pending 以支持“取消先于注册到达”；达到上限时逐项淘汰
                # 最旧一条（对齐 Rust 逐项 TTL 语义）：clear() 会把刚到达的取消
                # 连同历史残留一起清空，等于给新取消无中生有地重置了窗口（P2-7）
                if cancel_id not in self._pending:
                    if len(self._pending) >= self._MAX_PENDING:
                        self._pending.pop(next(iter(self._pending)))
                    self._pending[cancel_id] = None

    def unregister(self, cancel_id: str) -> None:
        """清理 cancel_id 注册项。"""
        with self._lock:
            self._events.pop(cancel_id, None)
            self._pending.pop(cancel_id, None)


# 模块级取消注册表：Worker 全局唯一实例（stdin 线程触发 / 命令处理器注册共用）
cancel_registry = CancelRegistry()

__all__ = ["CancelRegistry", "cancel_registry"]
