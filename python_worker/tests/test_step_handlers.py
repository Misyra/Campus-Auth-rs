"""步骤处理器纯逻辑测试：错误分类、取消、模板变量解析。

不启动浏览器，仅验证 step_handlers 中不依赖 Page 的纯函数。
"""

from __future__ import annotations

import threading

import pytest

from models import Outcome, StepConfig
from step_handlers import (
    StepCancelled,
    StepContext,
    WorkerError,
    _check_cancel,
    _get_handler,
    _resolve,
)


# ── WorkerError / StepCancelled 分类 ──

def test_worker_error_normalizes_outcome():
    err = WorkerError(Outcome.SELECTOR_FAILED, "选择器缺失")
    assert err.outcome == "selector_failed"
    assert err.message == "选择器缺失"
    # 字符串 outcome 直接透传
    err2 = WorkerError("network_error", "网络错误")
    assert err2.outcome == "network_error"


def test_step_cancelled_default_outcome():
    err = StepCancelled()
    assert err.outcome == Outcome.CANCELLED.value
    assert isinstance(err, WorkerError)


# ── 取消检查 ──

def test_check_cancel_raises_when_set():
    ctx = StepContext(page=None)
    ctx.cancel_event = threading.Event()
    ctx.cancel_event.set()
    with pytest.raises(StepCancelled):
        _check_cancel(ctx)


def test_check_cancel_noop_without_event():
    ctx = StepContext(page=None)
    _check_cancel(ctx)  # 不应抛出


# ── 处理器别名映射 ──

def test_handler_alias_mapping():
    assert _get_handler("input") is not None
    assert _get_handler("sleep") is _get_handler("wait")
    assert _get_handler("eval") is _get_handler("evaluate")
    assert _get_handler("custom_js") is _get_handler("evaluate")
    assert _get_handler("goto") is _get_handler("navigate")
    assert _get_handler("unknown_type") is None


# ── 模板变量解析 ──

def test_resolve_without_variables_unchanged():
    raw = StepConfig.from_dict({"id": "s1", "type": "click", "selector": "#{{A}}"})
    ctx = StepContext(page=None)
    resolved = _resolve(raw, ctx)
    assert resolved.selector == "#{{A}}"


def test_resolve_script_and_code():
    raw = StepConfig.from_dict({
        "id": "s1", "type": "evaluate", "code": "return '{{X}}'",
    })
    ctx = StepContext(page=None, variables={"X": "42"})
    resolved = _resolve(raw, ctx)
    assert resolved.code == "return '42'"
    assert resolved.effective_script == "return '42'"


def test_resolve_returns_copy_without_mutating_original():
    raw = StepConfig.from_dict({
        "id": "s1", "type": "input",
        "selector": "#{{FIELD}}", "value": "{{PASSWORD}}",
    })
    ctx = StepContext(page=None, variables={"FIELD": "u", "PASSWORD": "secret"})
    resolved = _resolve(raw, ctx)
    # 返回副本而非原地改写：调试会话重跑同一步骤不会二次变量解析
    assert resolved is not raw
    assert raw.selector == "#{{FIELD}}"
    assert raw.value == "{{PASSWORD}}"
    assert resolved.selector == "#u"
    assert resolved.value == "secret"


# ── B4/P3: _safe_op 非超时 playwright Error → SELECTOR_FAILED ──

def test_safe_op_maps_playwright_error_to_selector_failed():
    import asyncio

    from playwright.async_api import Error as PlaywrightError
    from step_handlers import Outcome, _safe_op

    async def boom():
        raise PlaywrightError("element is not attached to the DOM")

    async def run():
        with pytest.raises(WorkerError) as ei:
            await _safe_op(boom(), Outcome.SELECTOR_FAILED)
        return ei.value

    err = asyncio.run(run())
    # 非超时 playwright Error → SELECTOR_FAILED（可重试、不回收 Worker）
    assert err.outcome == Outcome.SELECTOR_FAILED.value


def test_safe_op_still_maps_timeout():
    import asyncio

    from playwright.async_api import TimeoutError as PlaywrightTimeoutError
    from step_handlers import Outcome, _safe_op

    async def boom():
        raise PlaywrightTimeoutError("Timeout 30000ms exceeded")

    async def run():
        with pytest.raises(WorkerError) as ei:
            await _safe_op(boom(), Outcome.SELECTOR_FAILED)
        return ei.value

    err = asyncio.run(run())
    assert err.outcome == Outcome.SELECTOR_FAILED.value
    assert "超时" in err.message


# ── B4/P7: 导航错误分类 ──

def test_classify_navigation_error_connection_errors():
    from step_handlers import Outcome, _classify_navigation_error

    # 连接级错误（即使以 TimeoutError 形式出现）→ NETWORK_ERROR
    err = _classify_navigation_error(
        Exception("net::ERR_CONNECTION_TIMED_OUT at https://x"), "https://x"
    )
    assert err.outcome == Outcome.NETWORK_ERROR.value
    err2 = _classify_navigation_error(
        Exception("net::ERR_NAME_NOT_RESOLVED"), "https://x"
    )
    assert err2.outcome == Outcome.NETWORK_ERROR.value


def test_classify_navigation_error_plain_timeout():
    from playwright.async_api import TimeoutError as PlaywrightTimeoutError
    from step_handlers import Outcome, _classify_navigation_error

    # 无连接错误代码的 Playwright TimeoutError → NAVIGATION_TIMEOUT
    err = _classify_navigation_error(
        PlaywrightTimeoutError("Timeout 15000ms exceeded"), "https://x"
    )
    assert err.outcome == Outcome.NAVIGATION_TIMEOUT.value


def test_classify_navigation_error_generic():
    from step_handlers import Outcome, _classify_navigation_error

    # 其他异常 → NETWORK_ERROR
    err = _classify_navigation_error(
        ValueError("page closed"), "https://x"
    )
    assert err.outcome == Outcome.NETWORK_ERROR.value


# ── G1: _safe_op 缺口（四处裸调用应归类而非 UNKNOWN_ERROR）──


def _playwright_error_classes():
    from playwright.async_api import Error as PlaywrightError
    from playwright.async_api import TimeoutError as PlaywrightTimeoutError
    return PlaywrightError, PlaywrightTimeoutError


def test_input_fallback_wait_for_maps_to_selector_failed():
    """input 降级路径的 wait_for 包 _safe_op：瞬时未 attach → SELECTOR_FAILED 可重试。"""
    import asyncio

    from step_handlers import Outcome, handle_input

    PlaywrightError, _PT = _playwright_error_classes()

    class FlakyLocator:
        async def fill(self, value, timeout=None):
            raise PlaywrightError("element is not visible")

        async def press_sequentially(self, value, timeout=None):
            raise PlaywrightError("element is not visible")

        @property
        def first(self):
            return self

        async def wait_for(self, state=None, timeout=None):
            # 元素未 attach 的瞬时失败（页面刷新间隙）
            raise PlaywrightError("element is not attached to the DOM")

        async def evaluate(self, *args, **kwargs):
            raise AssertionError("wait_for 失败后不应再走 _force_input")

    class FakePage:
        def locator(self, selector):
            return FlakyLocator()

    async def run():
        step = StepConfig.from_dict(
            {"id": "s1", "type": "input", "selector": "#u", "value": "x", "clear": True}
        )
        ctx = StepContext(page=FakePage(), default_timeout=500)
        with pytest.raises(WorkerError) as ei:
            await handle_input(FakePage(), step, ctx)
        return ei.value

    err = asyncio.run(run())
    assert err.outcome == Outcome.SELECTOR_FAILED.value
    assert "元素操作失败" in err.message


def test_screenshot_failure_maps_to_selector_failed():
    """handle_screenshot 的 page.screenshot 包 _safe_op：截图步骤失败可重试。"""
    import asyncio
    import tempfile
    from pathlib import Path

    from step_handlers import Outcome, handle_screenshot

    PlaywrightError, _PT = _playwright_error_classes()

    class FakePage:
        async def screenshot(self, **kwargs):
            raise PlaywrightError("Target closed")

    async def run(tmp: Path):
        step = StepConfig.from_dict({"id": "s1", "type": "screenshot"})
        ctx = StepContext(page=FakePage(), screenshot_dir=tmp)
        with pytest.raises(WorkerError) as ei:
            await handle_screenshot(FakePage(), step, ctx)
        return ei.value

    with tempfile.TemporaryDirectory() as td:
        err = asyncio.run(run(Path(td)))
    assert err.outcome == Outcome.SELECTOR_FAILED.value


def test_ocr_locator_screenshot_maps_to_selector_failed(monkeypatch):
    """handle_ocr 的 locator.screenshot 包 _safe_op：瞬时失败 → SELECTOR_FAILED。"""
    import asyncio
    import tempfile
    from pathlib import Path

    import step_handlers
    from step_handlers import Outcome, handle_ocr

    PlaywrightError, _PT = _playwright_error_classes()

    # 屏蔽真实 ddddocr 加载（本测试只关心截图分类）
    monkeypatch.setattr(step_handlers, "_get_ocr", lambda old, char_range=None: object())

    class FlakyLocator:
        async def wait_for(self, state=None, timeout=None):
            return None

        async def screenshot(self):
            raise PlaywrightError("element was detached")

    class FakePage:
        def locator(self, selector):
            return FlakyLocator()

    async def run(tmp: Path):
        step = StepConfig.from_dict(
            {"id": "s1", "type": "ocr", "selector": "#captcha"}
        )
        ctx = StepContext(page=FakePage(), screenshot_dir=tmp)
        with pytest.raises(WorkerError) as ei:
            await handle_ocr(FakePage(), step, ctx)
        return ei.value

    with tempfile.TemporaryDirectory() as td:
        err = asyncio.run(run(Path(td)))
    assert err.outcome == Outcome.SELECTOR_FAILED.value


def test_wait_url_page_closed_maps_to_navigation_timeout():
    """handle_wait_url 读取 page.url 抛 Target closed → NAVIGATION_TIMEOUT（可重试）。"""
    import asyncio

    from step_handlers import Outcome, handle_wait_url

    PlaywrightError, _PT = _playwright_error_classes()

    class ClosedPage:
        @property
        def url(self):
            raise PlaywrightError("Target page has been closed")

    async def run():
        step = StepConfig.from_dict(
            {"id": "s1", "type": "wait_url", "pattern": "ok"}
        )
        ctx = StepContext(page=ClosedPage())
        with pytest.raises(WorkerError) as ei:
            await handle_wait_url(ClosedPage(), step, ctx)
        return ei.value

    err = asyncio.run(run())
    assert err.outcome == Outcome.NAVIGATION_TIMEOUT.value
    assert "读取页面 URL 失败" in err.message


# ── B2: frame 字段断链修复 ──


def test_run_step_async_sets_and_restores_frame():
    """每个步骤执行前注入自身 frame，结束后恢复前值（context 跨步骤共享）。"""
    import asyncio

    import step_handlers
    from step_handlers import run_step_async

    seen = []

    async def probe(page, step, context):
        seen.append(context.frame)

    original = step_handlers._STEP_HANDLERS.get("probe")
    step_handlers._STEP_HANDLERS["probe"] = probe
    try:
        ctx = StepContext(page=None)
        ctx.frame = "PREV"
        s1 = StepConfig.from_dict({"id": "a", "type": "probe", "frame": "#f1"})
        s2 = StepConfig.from_dict({"id": "b", "type": "probe"})
        s3 = StepConfig.from_dict({"id": "c", "type": "probe", "frame": ""})

        async def run():
            await run_step_async(None, s1, ctx)
            await run_step_async(None, s2, ctx)
            await run_step_async(None, s3, ctx)

        asyncio.run(run())
    finally:
        if original is None:
            step_handlers._STEP_HANDLERS.pop("probe", None)
        else:
            step_handlers._STEP_HANDLERS["probe"] = original

    # 步骤内可见自身 frame；空串归一 None；步骤间互不泄漏
    assert seen == ["#f1", None, None]
    # 步骤结束后 context.frame 恢复前值
    assert ctx.frame == "PREV"


def test_run_step_async_restores_frame_on_error():
    """步骤抛错时 finally 同样恢复 frame 前值。"""
    import asyncio

    import step_handlers
    from step_handlers import run_step_async

    async def boom(page, step, context):
        raise WorkerError(Outcome.SELECTOR_FAILED, "失败")

    original = step_handlers._STEP_HANDLERS.get("probe_boom")
    step_handlers._STEP_HANDLERS["probe_boom"] = boom
    try:
        ctx = StepContext(page=None)
        ctx.frame = "PREV"
        step = StepConfig.from_dict({"id": "a", "type": "probe_boom", "frame": "#f"})

        async def run():
            with pytest.raises(WorkerError):
                await run_step_async(None, step, ctx)

        asyncio.run(run())
    finally:
        if original is None:
            step_handlers._STEP_HANDLERS.pop("probe_boom", None)
        else:
            step_handlers._STEP_HANDLERS["probe_boom"] = original
    assert ctx.frame == "PREV"


def test_frame_scoped_locators_use_frame_locator():
    """步骤声明 frame 时（经 run_step_async 注入），定位链路走
    page.frame_locator(...).locator(...)。"""
    import asyncio

    from step_handlers import run_step_async

    class FakeScopedLocator:
        def __init__(self, page):
            self._page = page

        @property
        def first(self):
            return self

        async def click(self, timeout=None):
            self._page.clicks.append("click")
            return None

        async def evaluate(self, script, arg=None, timeout=None):
            self._page.evaluates.append((script, arg))

        async def wait_for(self, state=None, timeout=None):
            return None

    class FakePage:
        def __init__(self):
            self.frame_calls = []
            self.locator_calls = []
            self.clicks = []
            self.evaluates = []

        def frame_locator(self, frame):
            self.frame_calls.append(frame)
            return self

        def locator(self, selector):
            self.locator_calls.append(selector)
            return FakeScopedLocator(self)

    async def run():
        # click：frame 生效 → frame_locator 被调用（frame 由 run_step_async 注入）
        page = FakePage()
        click_step = StepConfig.from_dict(
            {"id": "c1", "type": "click", "selector": "#btn", "frame": "#my-iframe"}
        )
        await run_step_async(page, click_step, StepContext(page=page))
        assert page.frame_calls == ["#my-iframe"]
        assert page.locator_calls == ["#btn"]
        assert page.clicks, "点击应在 frame 作用域内执行"

        # input（reveal_hidden 强制注入路径）：evaluate 在 frame 定位的 locator 上执行
        page2 = FakePage()
        input_step = StepConfig.from_dict(
            {"id": "i1", "type": "input", "selector": "#u", "value": "v",
             "frame": "iframe[name=x]"}
        )
        ctx = StepContext(page=page2, reveal_hidden=True)
        await run_step_async(page2, input_step, ctx)
        # handle_input 顶部预定位 + _force_input 注入各走一次 frame_locator
        assert page2.frame_calls == ["iframe[name=x]", "iframe[name=x]"]
        assert page2.evaluates, "_force_input 注入应经 frame_locator 定位后在框架内执行"

    asyncio.run(run())

def test_assert_text_passes_arg_as_function_parameter():
    """assert_text 的 wait_for_function 必须用带形参的箭头函数（E2E 回归）。

    无参形式 `() => ...includes(arg)` 会抛 ReferenceError: arg is not defined
    （Playwright 的 arg= 经函数入参注入），真实页面断言全部失败。
    """
    import asyncio

    from step_handlers import handle_assert_text

    class FakePage:
        def __init__(self):
            self.captured_js = None

        async def wait_for_function(self, js, arg=None, timeout=None):
            self.captured_js = js
            # 模拟 Playwright 语义：形参未声明时 arg 不可见
            if "arg =>" not in js:
                raise RuntimeError("ReferenceError: arg is not defined")

    page = FakePage()
    step = StepConfig.from_dict(
        {"id": "s1", "type": "assert_text", "selector": "#result", "value": "登录成功"}
    )
    asyncio.run(handle_assert_text(page, step, StepContext(page=page, default_timeout=500)))
    assert page.captured_js.startswith("arg =>"), f"JS 应声明 arg 形参: {page.captured_js}"
