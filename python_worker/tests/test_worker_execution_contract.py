"""Worker 浏览器执行/调试路径的契约回归测试。

不启动真实浏览器，重点锁住 TaskConfig 顶层时序参数、浏览器启动语义与
Debug Run All 相对正式 run_steps 的一致性，避免调试/生产路径契约漂移。
"""

from __future__ import annotations

import asyncio
from pathlib import Path

from debug_session import DebugSession, _build_steps_info
from models import Outcome, StepConfig, TaskConfig
import playwright_worker
from playwright_worker import WorkerCore
from step_handlers import StepCancelled, StepContext, WorkerError


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


def test_optional_failure_is_reported_in_formal_result(monkeypatch):
    async def fail(*_args, **_kwargs):
        raise WorkerError(Outcome.SELECTOR_FAILED, "missing")

    monkeypatch.setattr(playwright_worker, "run_step_async", fail)
    context = StepContext(page=object())
    result = asyncio.run(
        playwright_worker.run_steps(
            context.page,
            [_step("optional", required=False)],
            context,
        )
    )
    assert result.outcome == Outcome.SUCCESS.value
    assert "optional" in result.message
    assert "1 个非必须步骤失败" in result.message


def test_debug_step_uses_current_command_cancel_id(monkeypatch):
    core = WorkerCore()
    task_config = TaskConfig(task_id="debug", steps=[_step("slow")])
    session = _session(task_config)
    core._debug_sessions[session.session_id] = session

    async def wait_for_cancel(_page, _step, context, **_kwargs):
        while context.cancel_event is None or not context.cancel_event.is_set():
            await asyncio.sleep(0.01)
        raise StepCancelled("调试步骤已取消")

    monkeypatch.setattr(playwright_worker, "run_step_async", wait_for_cancel)

    async def run():
        pending = asyncio.create_task(
            core.handle_debug_step(
                {"session_id": session.session_id, "cancel_id": "current-debug-command"}
            )
        )
        await asyncio.sleep(0.02)
        playwright_worker.cancel_registry.trigger("current-debug-command")
        return await pending

    response = asyncio.run(run())
    assert response["results"][-1]["success"] is False
    assert "取消" in response["results"][-1]["message"]
    assert session.context.cancel_event is None


def test_persistent_session_keeps_local_storage(monkeypatch):
    core = WorkerCore()
    core._last_browser_settings = {"persistent_context": True}

    class FakePage:
        def __init__(self):
            self.script = ""

        async def add_init_script(self, script):
            self.script = script

    fresh = FakePage()

    class FakeContext:
        pages = []

    core._context = FakeContext()
    monkeypatch.setattr(core, "_new_page", lambda: asyncio.sleep(0, result=fresh))
    asyncio.run(core._prepare_session_page())
    assert "sessionStorage.clear()" in fresh.script
    assert "localStorage.clear()" not in fresh.script


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


def test_redirect_classifier_distinguishes_portal_online_and_generic_login():
    portal = playwright_worker._classify_redirect_test(
        trigger_url="http://www.msftconnecttest.com/connecttest.txt",
        final_url="http://10.0.0.1/portal",
        response_status=200,
        visible_text="校园网 用户登录 统一认证",
        password_inputs=1,
        account_inputs=1,
        forms=1,
    )
    assert portal["status"] == "detected"

    online = playwright_worker._classify_redirect_test(
        trigger_url="http://www.msftconnecttest.com/connecttest.txt",
        final_url="http://www.msftconnecttest.com/connecttest.txt",
        response_status=200,
        visible_text="Microsoft Connect Test",
        password_inputs=0,
        account_inputs=0,
        forms=0,
    )
    assert online["status"] == "online"

    generic = playwright_worker._classify_redirect_test(
        trigger_url="http://example.com/",
        final_url="http://example.com/",
        response_status=200,
        visible_text="欢迎访问，登录后查看个人中心",
        password_inputs=0,
        account_inputs=0,
        forms=0,
    )
    assert generic["status"] == "not_detected"


def test_redirect_test_forces_visible_isolated_browser(monkeypatch):
    core = WorkerCore()
    core._playwright = object()
    launched: dict = {}

    class FakeLocator:
        def __init__(self, selector: str):
            self.selector = selector

        async def inner_text(self, timeout):
            assert timeout == 1500
            return "校园网 登录 认证 账号 密码"

        async def count(self):
            if "password" in self.selector or "form" in self.selector:
                return 1
            return 0

    class FakeFrame:
        def locator(self, selector):
            return FakeLocator(selector)

    class FakeResponse:
        status = 200

    class FakePage:
        url = "http://10.0.0.1/portal"
        frames = [FakeFrame()]

        async def goto(self, *_args, **_kwargs):
            return FakeResponse()

        async def title(self):
            return "校园网认证"

        def is_closed(self):
            return False

    class FakeContext:
        def __init__(self):
            self.on_page = None

        def on(self, event, callback):
            assert event == "page"
            self.on_page = callback

        async def new_page(self):
            page = FakePage()
            self.on_page(page)
            return page

        async def close(self):
            launched["context_closed"] = True

    class FakeBrowser:
        async def new_context(self, **_kwargs):
            return FakeContext()

        async def close(self):
            launched["browser_closed"] = True

    async def fake_launch(*args, **_kwargs):
        launched["headless"] = args[3]
        return FakeBrowser()

    monkeypatch.setattr(core, "_launch_browser", fake_launch)
    monkeypatch.setattr(playwright_worker, "_REDIRECT_TEST_SETTLE_SECS", 0.0)
    result = asyncio.run(
        core.handle_test_redirect(
            {
                "trigger_url": "http://www.msftconnecttest.com/connecttest.txt",
                "browser_settings": {},
            }
        )
    )

    assert result["status"] == "detected"
    assert launched == {
        "headless": False,
        "context_closed": True,
        "browser_closed": True,
    }
    assert core._context is None
    assert core._page is None


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



def test_top_level_session_page_isolates_web_storage_without_clearing_cookies(monkeypatch):
    core = WorkerCore()

    class FakePage:
        def __init__(self) -> None:
            self.closed = False
            self.init_scripts: list[str] = []

        async def close(self) -> None:
            self.closed = True

        async def add_init_script(self, script: str) -> None:
            self.init_scripts.append(script)

    old_a = FakePage()
    old_b = FakePage()
    fresh = FakePage()

    class FakeContext:
        def __init__(self) -> None:
            self.pages = [old_a, old_b]
            self.clear_cookies_calls = 0

        async def clear_cookies(self) -> None:
            self.clear_cookies_calls += 1

    context = FakeContext()
    core._context = context
    core._page = old_a

    async def fake_new_page():
        return fresh

    monkeypatch.setattr(core, "_new_page", fake_new_page)
    page = asyncio.run(core._prepare_session_page())

    assert page is fresh
    assert core._page is fresh
    assert old_a.closed is True
    assert old_b.closed is True
    assert context.clear_cookies_calls == 0
    assert len(fresh.init_scripts) == 1
    script = fresh.init_scripts[0]
    assert "localStorage.clear()" in script
    assert "sessionStorage.clear()" in script
    assert "sessionStorage.getItem(marker)" in script
    assert "sessionStorage.setItem(marker" in script


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

def test_login_trigger_url_overrides_auth_url(monkeypatch):
    core = WorkerCore()
    captured: dict[str, str] = {}
    seen: dict[str, str] = {}

    class FakeResult:
        data = None

        def to_dict(self):
            return {"success": True, "data": self.data}

    async def fake_run_task(
        _task, _bs, variables, _cancel_event, _screenshot_dir, navigate_url=""
    ):
        captured.update(variables)
        seen["navigate_url"] = navigate_url
        return FakeResult()

    monkeypatch.setattr(core, "_run_task", fake_run_task)

    asyncio.run(
        core.handle_execute_login_attempt(
            {
                "username": "u",
                "password": "p",
                "isp": "",
                "auth_url": "http://10.0.0.1/login",
                "trigger_url": "http://captive.apple.com/hotspot-detect.html",
                "task_config": {"variables": {}},
            }
        )
    )

    assert seen["navigate_url"] == "http://captive.apple.com/hotspot-detect.html"
    assert captured["LOGIN_URL"] == "http://captive.apple.com/hotspot-detect.html"


def test_system_variables_prefers_trigger():
    out = WorkerCore._system_variables(
        {"auth_url": "http://10.0.0.1/login", "trigger_url": "http://detectportal.firefox.com/success.txt"}
    )
    assert out["LOGIN_URL"] == "http://detectportal.firefox.com/success.txt"
    out2 = WorkerCore._system_variables({"auth_url": "http://10.0.0.1/login", "trigger_url": ""})
    assert out2["LOGIN_URL"] == "http://10.0.0.1/login"


def test_start_url_prefers_task_then_falls_back_to_profile():
    """首导航取值：任务自身 url 优先，未配置时回落 Profile 有效登录地址。"""
    from playwright_worker import _profile_login_url, _resolve_start_url

    fallback = "http://www.msftconnecttest.com/connecttest.txt"
    # 认证地址留空 → Rust 补的默认触发地址就是有效首导航地址
    assert _profile_login_url({"auth_url": "", "trigger_url": fallback}) == fallback
    # 触发器非空优先于认证地址（旧版同时保存两个地址的方案）
    assert (
        _profile_login_url({"auth_url": "http://10.0.0.1/login", "trigger_url": "http://detect/"})
        == "http://detect/"
    )
    assert _profile_login_url({"auth_url": "http://10.0.0.1/login", "trigger_url": ""}) == (
        "http://10.0.0.1/login"
    )
    # 任务自身地址优先，不被 Profile 地址覆盖
    assert _resolve_start_url("http://task.example/", {}, fallback) == "http://task.example/"
    # 任务 url 是模板 → 由变量解析成触发/认证地址
    assert (
        _resolve_start_url("{{LOGIN_URL}}", {"LOGIN_URL": "http://trigger/"}, "") == "http://trigger/"
    )
    # 变量未命中时 resolve 保留字面量 → 按空处理并回落（不把 "{{...}}" 交给浏览器）
    assert _resolve_start_url("{{MISSING}}", {}, fallback) == fallback
    # 任务未配地址 → 回落
    assert _resolve_start_url("", {}, fallback) == fallback
    # 任务与 Profile 都拿不到地址 → 空串（调用方跳过导航，而不是 goto("")）
    assert _resolve_start_url("", {}, "") == ""


class _FakeDebugPage:
    """只记录 goto 实参的调试页；screenshot 供初始截图空实现。"""

    def __init__(self) -> None:
        self.navigated: list[str] = []

    async def goto(self, url, **_kwargs):
        self.navigated.append(url)

    async def screenshot(self, **_kwargs):
        return b""


def _debug_start_core(monkeypatch, tmp_path, page):
    """装配 handle_debug_start 所需的浏览器替身（不启动真实浏览器）。"""
    core = WorkerCore()
    core._playwright = object()

    async def fake_ensure_browser(*_args, **_kwargs):
        return None

    async def fake_prepare_session_page():
        core._page = page

    def fake_make_context(*_args, **_kwargs):
        return StepContext(page=page)

    monkeypatch.setattr(core, "ensure_browser", fake_ensure_browser)
    monkeypatch.setattr(core, "_prepare_session_page", fake_prepare_session_page)
    monkeypatch.setattr(core, "_make_context", fake_make_context)
    monkeypatch.setattr(playwright_worker, "_debug_screenshot_dir", lambda: tmp_path)
    return core


def test_debug_start_falls_back_to_profile_url_when_task_has_none(monkeypatch, tmp_path):
    """任务未配起始地址 → 回落 Profile 有效登录地址（不再停在空白页）。"""
    page = _FakeDebugPage()
    core = _debug_start_core(monkeypatch, tmp_path, page)

    response = asyncio.run(
        core.handle_debug_start(
            {
                "task_config": {"task_id": "no-url", "url": "", "steps": [], "variables": {}},
                "auth_url": "",
                "trigger_url": "http://www.msftconnecttest.com/connecttest.txt",
            }
        )
    )

    assert page.navigated == ["http://www.msftconnecttest.com/connecttest.txt"]
    assert response["total_steps"] == 0
    assert core._debug_sessions, "会话应正常建成"


def test_debug_start_skips_navigation_when_no_start_url(monkeypatch, tmp_path):
    """任务 url 与 Profile 认证地址都为空（直连渠道）→ 跳过首导航，不 goto("")。"""
    page = _FakeDebugPage()
    core = _debug_start_core(monkeypatch, tmp_path, page)

    asyncio.run(
        core.handle_debug_start(
            {
                "task_config": {
                    "task_id": "no-url",
                    "url": "{{LOGIN_URL}}",
                    "steps": [],
                    "variables": {},
                },
                "auth_url": "",
                "trigger_url": "",
            }
        )
    )

    assert page.navigated == []
    assert core._debug_sessions, "会话应正常建成（不再因无效 URL 中断启动）"


def test_login_system_variables_skip_missing_keys(monkeypatch):
    """登录命令未提供的 Profile 键不注入（避免空串覆盖任务自定义变量）。

    handle_execute_login_attempt 统一复用 _system_variables 后的行为选择：
    键缺失跳过，而非旧手写版的空串默认值；{{LOGIN_URL}} 仍回落 auth_url。
    """
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
        return FakeResult()

    monkeypatch.setattr(core, "_run_task", fake_run_task)

    # 仅传 username + auth_url：PASSWORD/ISP 缺失 → 不注入空串，任务自定义值保留
    asyncio.run(
        core.handle_execute_login_attempt(
            {
                "username": "profile-user",
                "auth_url": "http://10.0.0.1/login",
                "task_config": {
                    "variables": {"PASSWORD": "custom-pass", "ISP": "custom-isp"}
                },
            }
        )
    )

    assert captured == {
        "USERNAME": "profile-user",
        "PASSWORD": "custom-pass",
        "ISP": "custom-isp",
        "LOGIN_URL": "http://10.0.0.1/login",
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


class _FramingPage:
    """记录截图调用并落盘占位 PNG 的调试页替身（供调试补拍断言）。"""

    def __init__(self, *, fail: bool = False) -> None:
        self.shots: list[dict] = []
        self.fail = fail

    async def screenshot(self, **kwargs):
        self.shots.append(kwargs)
        if self.fail:
            raise RuntimeError("截图失败")
        Path(kwargs["path"]).write_bytes(b"\x89PNG\r\n\x1a\n")
        return b""


def _framing_core(monkeypatch, tmp_path, page, events):
    """装配带截图能力的调试会话（不启动真实浏览器）。"""
    core = WorkerCore()
    core.emit = lambda event_type, data: events.append((event_type, data))
    task = TaskConfig(task_id="debug-frame", steps=[_step("first"), _step("second")])
    session = DebugSession(
        session_id="frame-session",
        page=page,
        task_config=task,
        context=StepContext(page=page, step_delay=0.0),
        task_id=task.task_id,
        steps_info=_build_steps_info(task),
    )
    core._debug_sessions[session.session_id] = session
    monkeypatch.setattr(playwright_worker, "_debug_screenshot_dir", lambda: tmp_path)

    async def fake_run_step(_page, _step, _context, **_kwargs) -> None:
        return None

    monkeypatch.setattr(playwright_worker, "run_step_async", fake_run_step)
    return core, session


def test_debug_step_captures_frame_after_each_step(monkeypatch, tmp_path):
    """单步结束后补拍一帧并推送带步骤序号的截图事件。

    回归："实时截图"此前只有 debug_start 的初始帧，普通步骤（input/click/…）
    执行后什么都不推，面板预览停在启动画面，表现为"点下一步浏览器不刷新"。
    """
    events: list[tuple[str, dict]] = []
    page = _FramingPage()
    core, session = _framing_core(monkeypatch, tmp_path, page, events)

    asyncio.run(core.handle_debug_step({"session_id": session.session_id}))
    asyncio.run(core.handle_debug_step({"session_id": session.session_id}))

    shots = [data for ev, data in events if ev == "screenshot"]
    assert [data["step_index"] for data in shots] == [0, 1]
    # 步骤补拍取视口截图（整页留给会话初始帧），并有独立文件
    assert all(shot["full_page"] is False for shot in page.shots)
    assert len({shot["path"] for shot in shots}) == 2
    assert all(shot["path"].endswith(".png") for shot in shots)
    # 路径已登记，会话结束时统一清理
    assert session.context.screenshots == [shot["path"] for shot in shots]


def test_debug_run_all_captures_frame_per_step(monkeypatch, tmp_path):
    """批量执行时每步各补拍一帧，预览随批量进度推进。"""
    events: list[tuple[str, dict]] = []
    core, session = _framing_core(monkeypatch, tmp_path, _FramingPage(), events)

    asyncio.run(core.handle_debug_run_all({"session_id": session.session_id}))

    assert [data["step_index"] for ev, data in events if ev == "screenshot"] == [0, 1]


def test_debug_step_tolerates_screenshot_failure(monkeypatch, tmp_path):
    """补拍失败（页面已关闭/超时）只记日志：步骤结果与响应不受影响。"""
    events: list[tuple[str, dict]] = []
    core, session = _framing_core(monkeypatch, tmp_path, _FramingPage(fail=True), events)

    response = asyncio.run(core.handle_debug_step({"session_id": session.session_id}))

    assert response["results"][0]["success"] is True
    assert [ev for ev, _ in events if ev == "screenshot"] == []
    assert session.context.screenshots == []


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
