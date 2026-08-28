"""Worker 浏览器执行/调试路径的契约回归测试。

不启动真实浏览器，重点锁住 TaskConfig 顶层时序参数、浏览器启动语义与
Debug Run All 相对正式 run_steps 的一致性，避免调试/生产路径契约漂移。
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


def test_webkit_uses_webkit_launcher_and_no_chromium_flags():
    chromium = object()
    firefox = object()
    webkit = object()
    playwright = type(
        "FakePlaywright",
        (),
        {"chromium": chromium, "firefox": firefox, "webkit": webkit},
    )()
    core = WorkerCore()
    core._last_browser_settings = {}

    launcher, path = core._resolve_launcher(playwright, "webkit", "")

    assert launcher is webkit
    assert path is None
    assert "--no-sandbox" not in core._build_launch_args({}, "webkit")
    assert "--disable-gpu" not in core._build_launch_args({}, "webkit")


def test_custom_webkit_filters_chromium_only_flags():
    core = WorkerCore()
    args = core._build_launch_args(
        {
            "custom_browser_engine": "webkit",
            "browser_args": "--no-sandbox\n--custom-safe-flag",
        },
        "custom",
    )

    assert "--no-sandbox" not in args
    assert args == ["--custom-safe-flag"]


def test_pure_mode_keeps_context_options(monkeypatch):
    core = WorkerCore()
    core._playwright = object()
    captured: dict = {}

    class FakeBrowser:
        async def new_context(self, **kwargs):
            captured.update(kwargs)
            return object()

    async def fake_launch(*_args, **_kwargs):
        return FakeBrowser()

    async def fake_new_page():
        return object()

    monkeypatch.setattr(core, "_launch_browser", fake_launch)
    monkeypatch.setattr(core, "_new_page", fake_new_page)

    asyncio.run(
        core._start_browser(
            {
                "browser_settings": {
                    "pure_mode": True,
                    "persistent_context": False,
                    "browser_channel": "playwright",
                    "locale": "en-US",
                    "timezone_id": "UTC",
                    "user_agent": "CampusAuth-Test",
                    "extra_headers_json": '{"X-Test":"1"}',
                    "bind_proxy": "http://127.0.0.1:7890",
                    "ignore_https_errors": False,
                    "viewport_width": 1024,
                    "viewport_height": 768,
                }
            }
        )
    )

    assert captured["locale"] == "en-US"
    assert captured["timezone_id"] == "UTC"
    assert captured["user_agent"] == "CampusAuth-Test"
    assert captured["extra_http_headers"] == {"X-Test": "1"}
    assert captured["proxy"] == {"server": "http://127.0.0.1:7890"}
    assert captured["ignore_https_errors"] is False
    assert captured["viewport"] == {"width": 1024, "height": 768}


def test_login_system_variables_override_task_variables(monkeypatch):
    core = WorkerCore()
    captured: dict[str, str] = {}

    class FakeResult:
        data = None

        def to_dict(self):
            return {"success": True, "data": self.data}

    async def fake_run_task(
        _task, _bs, variables, _cancel_event, _screenshot_dir, navigate_url=""
    ):
        captured.update(variables)
        assert navigate_url == "https://portal.example/login"
        return FakeResult()

    monkeypatch.setattr(core, "_run_task", fake_run_task)

    response = asyncio.run(
        core.handle_execute_login_attempt(
            {
                "username": "profile-user",
                "password": "profile-pass",
                "isp": "profile-isp",
                "auth_url": "https://portal.example/login",
                "task_config": {
                    "variables": {
                        "USERNAME": "stale-user",
                        "PASSWORD": "stale-pass",
                        "ISP": "stale-isp",
                        "LOGIN_URL": "https://stale.invalid",
                        "CUSTOM": "keep-me",
                    }
                },
            }
        )
    )

    assert response["success"] is True
    assert captured == {
        "USERNAME": "profile-user",
        "PASSWORD": "profile-pass",
        "ISP": "profile-isp",
        "LOGIN_URL": "https://portal.example/login",
        "CUSTOM": "keep-me",
    }


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


def test_health_check_rejects_closed_context_even_if_browser_process_is_connected():
    core = WorkerCore()

    class FakeBrowser:
        def is_connected(self) -> bool:
            return True

    class ClosedContext:
        async def cookies(self):
            raise RuntimeError("Target page, context or browser has been closed")

    core._browser = FakeBrowser()
    core._context = ClosedContext()

    assert asyncio.run(core._health_check()) is False


def test_health_check_accepts_live_context_and_connected_browser():
    core = WorkerCore()
    calls = 0

    class FakeBrowser:
        def is_connected(self) -> bool:
            return True

    class LiveContext:
        async def cookies(self):
            nonlocal calls
            calls += 1
            return []

    core._browser = FakeBrowser()
    core._context = LiveContext()

    assert asyncio.run(core._health_check()) is True
    assert calls == 1


def test_health_check_accepts_live_persistent_context_without_browser_handle():
    core = WorkerCore()

    class LiveContext:
        async def cookies(self):
            return []

    core._browser = None
    core._context = LiveContext()

    assert asyncio.run(core._health_check()) is True
