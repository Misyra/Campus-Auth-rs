"""Worker 核心逻辑测试：浏览器参数构建、超时归一化、取消注册表、
IPC 响应/事件序列化，以及状态真值判定。

这些均为纯逻辑，不启动真实浏览器，可在无 Playwright 运行时下运行。
"""

from __future__ import annotations

import asyncio
import json

import pytest


# ── 超时归一化 ──

def test_to_ms_seconds_vs_millis():
    from playwright_worker import _to_ms
    # Rust 侧 BrowserSettings 的 timeout/navigation_timeout 为 u32 秒，统一 ×1000
    assert _to_ms({"timeout": 15}, "timeout", 10000) == 15000
    # 缺省值
    assert _to_ms({}, "timeout", 10000) == 10000
    # 非法值回退缺省
    assert _to_ms({"timeout": "abc"}, "timeout", 10000) == 10000


# ── 状态真值判定 ──

def test_is_truthy():
    from playwright_worker import _is_truthy
    assert _is_truthy(True) is True
    assert _is_truthy(False) is False
    assert _is_truthy(None) is False
    for falsy in ["false", "FALSE", " false ", "0", "", "no", "off", "No"]:
        assert _is_truthy(falsy) is False, f"{falsy!r} 应为假"
    for truthy in ["true", "1", "yes", "on", "success"]:
        assert _is_truthy(truthy) is True, f"{truthy!r} 应为真"
    assert _is_truthy(0) is False
    assert _is_truthy(1) is True
    assert _is_truthy(0.0) is False
    assert _is_truthy(3.14) is True
    assert _is_truthy([1]) is True
    assert _is_truthy([]) is False


# ── 浏览器启动参数构建 ──

def test_build_launch_args_base_chromium():
    from playwright_worker import WorkerCore
    core = WorkerCore()
    args = core._build_launch_args({})
    assert "--no-sandbox" in args
    assert "--disable-gpu" in args


def test_build_launch_args_firefox_skips_chromium_flags():
    from playwright_worker import WorkerCore
    core = WorkerCore()
    args = core._build_launch_args({}, channel="firefox")
    assert "--no-sandbox" not in args
    assert "--disable-gpu" not in args


def test_build_launch_args_filters_blocked_and_dedupes():
    from playwright_worker import WorkerCore
    core = WorkerCore()
    bs = {
        "browser_args": (
            "--remote-debugging-port=9222\n"  # 黑名单：过滤
            "--proxy-server=http://x:8080\n"  # 黑名单：过滤
            "--no-sandbox\n"                  # 重复：去重
            "--new-flag=1\n"                  # 放行
            "#comment\n"                      # 注释：跳过
            "\n"                              # 空行：跳过
        )
    }
    args = core._build_launch_args(bs)
    assert "--remote-debugging-port=9222" not in args
    assert "--proxy-server=http://x:8080" not in args
    assert args.count("--no-sandbox") == 1
    assert "--new-flag=1" in args


def test_build_launch_args_low_resource_and_web_security():
    from playwright_worker import WorkerCore
    core = WorkerCore()
    bs = {"low_resource_mode": True, "disable_web_security": True}
    args = core._build_launch_args(bs)
    assert "--blink-settings=imagesEnabled=false" in args
    assert "--disable-web-security" in args


# ── 取消注册表 ──

def test_cancel_registry_basic():
    from playwright_worker import CancelRegistry
    reg = CancelRegistry()
    ev = reg.register("c1")
    assert ev.is_set() is False
    reg.trigger("c1")
    assert ev.is_set() is True
    reg.unregister("c1")


def test_cancel_registry_cancel_before_register():
    from playwright_worker import CancelRegistry
    reg = CancelRegistry()
    reg.trigger("c2")  # 取消先于注册到达
    ev = reg.register("c2")
    assert ev.is_set() is True  # 注册时立即置位


def test_cancel_registry_pending_cap():
    from playwright_worker import CancelRegistry
    reg = CancelRegistry()
    # 超过上限后旧 pending 被清空，不永久堆积
    for i in range(reg._MAX_PENDING + 10):
        reg.trigger(f"pend_{i}")
    assert len(reg._pending) <= reg._MAX_PENDING


# ── IPC 响应/事件序列化 ──

def test_emit_response_format(capsys):
    import worker_main
    worker_main.emit_response(42, {"success": True, "data": {"a": 1}, "error": None})
    line = capsys.readouterr().out.strip()
    msg = json.loads(line)
    assert msg["id"] == 42
    assert msg["result"]["success"] is True
    assert msg["result"]["data"] == {"a": 1}


def test_emit_event_format(capsys):
    import worker_main
    worker_main.emit_event("screenshot", {"path": "a.png"})
    msg = json.loads(capsys.readouterr().out.strip())
    assert msg["event"] == "screenshot"
    assert msg["data"] == {"path": "a.png"}


@pytest.mark.parametrize("success,outcome,message", [
    (True, "cancelled", "已取消"),
    (False, "selector_failed", "选择器缺失"),
])
def test_structured_result_format(success, outcome, message):
    from step_handlers import WorkerError
    from worker_main import _structured_result
    exc = WorkerError(outcome, message)
    d = _structured_result(exc, success=success)
    assert d["success"] is success
    assert d["data"]["outcome"] == outcome
    assert d["data"]["message"] == message
    assert d["data"]["duration_ms"] == 0
    assert d["data"]["screenshots"] == []
    assert d["error"] == (None if success else message)


def test_error_result_format():
    from worker_main import _error_result
    d = _error_result("未知命令: foo")
    assert d == {"success": False, "data": None, "error": "未知命令: foo"}


# ── 5.5: debug 会话空 session_id 回退（Rust 从不传 session_id）──

def _make_debug_session(sid: str):
    from playwright_worker import DebugSession, StepContext
    return DebugSession(
        session_id=sid, page=None, task_config=None, context=StepContext(page=None)
    )


def test_debug_session_for_single_session_fallback():
    from playwright_worker import WorkerCore
    core = WorkerCore()
    s1 = _make_debug_session("s1")
    core._debug_sessions["s1"] = s1
    # 空 session_id 且恰有一个活跃会话 → 回退到它
    assert core._debug_session_for("") is s1
    # 显式 id 正常命中
    assert core._debug_session_for("s1") is s1


def test_debug_session_for_multiple_sessions_raises():
    from playwright_worker import WorkerCore
    from step_handlers import WorkerError
    core = WorkerCore()
    core._debug_sessions["s1"] = _make_debug_session("s1")
    core._debug_sessions["s2"] = _make_debug_session("s2")
    # 空 session_id 且存在多个会话 → 报错（与 Rust 单会话语义对齐）
    with pytest.raises(WorkerError):
        core._debug_session_for("")


def test_debug_session_for_no_session_raises():
    from playwright_worker import WorkerCore
    from step_handlers import WorkerError
    core = WorkerCore()
    with pytest.raises(WorkerError):
        core._debug_session_for("")


def test_debug_session_for_unknown_id_raises():
    from playwright_worker import WorkerCore
    from step_handlers import WorkerError
    core = WorkerCore()
    core._debug_sessions["s1"] = _make_debug_session("s1")
    with pytest.raises(WorkerError):
        core._debug_session_for("nope")


# ── 5.7: OCR 模型缓存复用实例 ──

def test_get_ocr_caches_instance(monkeypatch):
    import sys, types
    from step_handlers import _get_ocr

    fake = types.ModuleType("ddddocr")

    class FakeDdddOcr:
        def __init__(self, old, show_ad):
            pass

    fake.DdddOcr = FakeDdddOcr
    monkeypatch.setitem(sys.modules, "ddddocr", fake)
    a = _get_ocr(False)
    b = _get_ocr(False)
    assert a is b  # 两次获取返回同一实例
    c = _get_ocr(True)
    assert c is not a  # 不同 old 参数单独缓存


def test_structured_result_duration_ms_with_start():
    import time
    from step_handlers import WorkerError
    from worker_main import _structured_result
    exc = WorkerError("selector_failed", "失败")
    start = time.perf_counter()
    _structured_result(exc, success=False, start=start)
    # 未传 start 时保持 0（兼容直接调用）
    d0 = _structured_result(exc, success=False)
    assert d0["data"]["duration_ms"] == 0


# ── A1: Worker 挂起死锁自愈 ──

def test_dispatch_guarded_timeout_emits_unknown_error(monkeypatch, capsys):
    """命令超时后按 unknown_error 回错误响应，且不堵死后续命令。"""

    async def _run():
        import worker_main
        # 缩短命令级超时，避免测试等待默认 300s
        monkeypatch.setattr(worker_main, "_command_timeout", lambda params: 0.1)

        async def hang_handler(params, core):
            await asyncio.sleep(3600)  # 永久挂起
            return {}

        worker_main.COMMANDS["test_hang"] = hang_handler
        try:
            await worker_main._dispatch_guarded(
                {"id": 1, "method": "test_hang", "params": {}}
            )
            lines = capsys.readouterr().out.strip().splitlines()
            assert len(lines) == 1
            msg = json.loads(lines[0])
            assert msg["id"] == 1
            assert msg["result"]["success"] is False
            assert msg["result"]["data"]["outcome"] == "unknown_error"
        finally:
            del worker_main.COMMANDS["test_hang"]

        # 自愈后下一条命令仍能正常执行（不死锁）
        called = []
        worker_main.COMMANDS["test_ok"] = lambda params, core: called.append(1) or {}
        try:
            await worker_main._dispatch_guarded(
                {"id": 2, "method": "test_ok", "params": {}}
            )
            assert called == [1]
        finally:
            del worker_main.COMMANDS["test_ok"]

    asyncio.run(_run())


def test_handle_evaluate_hung_script_times_out():
    """handle_evaluate 挂起脚本被单独超时中断。"""

    async def _run():
        from models import StepConfig
        from step_handlers import StepContext, WorkerError, handle_evaluate

        class HungPage:
            async def evaluate(self, script):
                await asyncio.sleep(3600)  # while(true){} 模拟

            async def close(self):
                pass

        page = HungPage()
        step = StepConfig(id="1", step_type="evaluate", code="while(true){}")
        with pytest.raises(WorkerError) as ei:
            await handle_evaluate(page, step, StepContext(page=page, default_timeout=100))
        assert ei.value.outcome == "unknown_error"
        assert "超时" in ei.value.message

    asyncio.run(_run())