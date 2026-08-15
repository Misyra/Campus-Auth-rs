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