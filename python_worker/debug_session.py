"""调试会话纯状态机：会话数据与步骤信息构建（WorkerCore 拆分）。

自 playwright_worker.py 迁出（A 组重构）：DebugSession 数据类与 steps_info
构建不依赖 Playwright，可在无浏览器环境直测。命令编排（debug_start/step/
run_all/stop 等 WorkerCore 方法）仍留在 WorkerCore——它们驱动真实页面 I/O。
"""

from __future__ import annotations

import threading
from dataclasses import dataclass, field
from typing import Any

from models import TaskConfig
from step_handlers import StepContext


@dataclass
class DebugSession:
    """调试会话状态。"""

    session_id: str
    page: Any
    task_config: TaskConfig
    context: StepContext
    cancel_id: str = ""
    cancel_event: threading.Event | None = None
    # 自动步进游标：前端“下一步”无显式索引时，按顺序执行尚未运行的步骤
    current_step: int = 0
    # 面向前端的会话数据（对齐原版 debug_to_response）：
    # steps: [{index,id,type,description}]；results: [{step_index,success,message,running}]
    task_id: str = ""
    steps_info: list[dict] = field(default_factory=list)
    results: list[dict] = field(default_factory=list)


def _build_steps_info(task_config: TaskConfig) -> list[dict]:
    """构建前端可展示的步骤列表（index/id/type/description）。"""
    return [
        {
            "index": i,
            "id": s.id or f"step_{i}",
            "type": s.step_type or s.id or "?",
            "description": s.description or "",
        }
        for i, s in enumerate(task_config.steps)
    ]


__all__ = ["DebugSession", "_build_steps_info"]
