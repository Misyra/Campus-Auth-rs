"""Worker 浏览器执行/调试路径的契约回归测试。

不启动真实浏览器，重点锁住 TaskConfig 顶层时序参数与 Debug Run All
相对正式 run_steps 的一致性，避免调试成功/失败表现与实际执行不同。
"""

from __future__ import annotations

import asyncio

from debug_session import DebugSession, _build_steps_info
from models import Outcome, StepConfig, TaskConfig
import playwright_worker
from playwright_worker import WorkerCore
from step_handlers import StepContext, WorkerError


def _step(step_id: str, *, required: bool = True) -> StepConfig:
    return StepConfig.from_dict(
        {
            "id": step_id,
            "type": "click",
            "selector": f"#{step_id}",
            "required": required,
        }
    )


def _session(task: TaskConfig, *, step_delay: float = 0.0) -> DebugSession:
    context = StepContext(page=object(), step_delay=step_delay)
    return DebugSession(
        session_id="session",
        page=context.page,
        task_config=task,
        context=context,
        task_id=task.task_id,
        steps_info=_build_steps_info(task),
    )


def test_navigation_wait_uses_cancellable_sleep(monkeypatch):
    task = TaskConfig(navigation_wait=2.5)
    context = StepContext(page=None)
    calls: list[tuple[float, StepContext]] = []

    async def fake_sleep(seconds: float, ctx: StepContext) -> None:
        calls.append((seconds, ctx))

    monkeypatch.setattr(playwright_worker, "_sleep_cancellable", fake_sleep)
    asyncio.run(WorkerCore._wait_after_navigation(task, context))

    assert calls == [(2.5, context)]


def test_navigation_wait_zero_skips_sleep(monkeypatch):
    task = TaskConfig(navigation_wait=0)
    context = StepContext(page=None)
    called = False

    async def fake_sleep(_seconds: float, _ctx: StepContext) -> None:
        nonlocal called
        called = True

    monkeypatch.setattr(playwright_worker, "_sleep_cancellable", fake_sleep)
    asyncio.run(WorkerCore._wait_after_navigation(task, context))

    assert called is False


def test_debug_run_all_continues_after_optional_failure_and_applies_delay(monkeypatch):
    task = TaskConfig(
        task_id="debug-contract",
        steps=[
            _step("optional", required=False),
            _step("second"),
            _step("third"),
        ],
    )
    session = _session(task, step_delay=0.25)
    core = WorkerCore()
    core._debug_sessions[session.session_id] = session

    executed: list[str] = []
    delays: list[float] = []

    async def fake_run_step(_page, step, _context, **_kwargs) -> None:
        executed.append(step.id)
        if step.id == "optional":
            raise WorkerError(Outcome.SELECTOR_FAILED, "可选步骤失败")

    async def fake_sleep(seconds: float, _context: StepContext) -> None:
        delays.append(seconds)

    monkeypatch.setattr(playwright_worker, "run_step_async", fake_run_step)
    monkeypatch.setattr(playwright_worker, "_sleep_cancellable", fake_sleep)

    response = asyncio.run(core.handle_debug_run_all({"session_id": session.session_id}))

    assert executed == ["optional", "second", "third"]
    assert delays == [0.25, 0.25]
    assert response["current_step"] == 3
    assert [item["success"] for item in response["results"]] == [False, True, True]


def test_debug_run_all_stops_after_required_failure(monkeypatch):
    task = TaskConfig(
        task_id="debug-required",
        steps=[_step("first"), _step("required"), _step("never")],
    )
    session = _session(task)
    core = WorkerCore()
    core._debug_sessions[session.session_id] = session

    executed: list[str] = []

    async def fake_run_step(_page, step, _context, **_kwargs) -> None:
        executed.append(step.id)
        if step.id == "required":
            raise WorkerError(Outcome.SELECTOR_FAILED, "必需步骤失败")

    monkeypatch.setattr(playwright_worker, "run_step_async", fake_run_step)

    response = asyncio.run(core.handle_debug_run_all({"session_id": session.session_id}))

    assert executed == ["first", "required"]
    assert response["current_step"] == 2
    assert [item["success"] for item in response["results"]] == [True, False]


def test_debug_run_all_records_unexpected_exception_and_stops(monkeypatch):
    task = TaskConfig(
        task_id="debug-exception",
        steps=[_step("broken", required=False), _step("never")],
    )
    session = _session(task)
    core = WorkerCore()
    core._debug_sessions[session.session_id] = session

    async def fake_run_step(_page, _step, _context, **_kwargs) -> None:
        raise RuntimeError("boom")

    monkeypatch.setattr(playwright_worker, "run_step_async", fake_run_step)

    response = asyncio.run(core.handle_debug_run_all({"session_id": session.session_id}))

    assert response["current_step"] == 1
    assert response["results"][0]["success"] is False
    assert "boom" in response["results"][0]["message"]
