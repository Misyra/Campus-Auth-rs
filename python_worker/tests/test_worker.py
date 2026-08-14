"""Worker 核心逻辑测试：浏览器参数构建、超时归一化、取消注册表、
IPC 响应/事件序列化，以及状态真值判定。

这些均为纯逻辑，不启动真实浏览器，可在无 Playwright 运行时下运行。
"""

from __future__ import annotations

import json
import sys
import threading

import pytest


# ── 超时归一化 ──

def test_to_ms_seconds_vs_millis():
    from playwright_worker import _to_ms
    # <1000 视为秒 → ×1000；>=1000 视为毫秒 → 原样
    assert _to_ms({"timeout": 15}, "timeout", 10000) == 15000
    assert _to_ms({"timeout": 5000}, "timeout", 10000) == 5000
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

class _Capture:
    """捕获 stdout 写入的辅助对象。"""

    def __init__(self) -> None:
        self.lines: list[str] = []

    def write(self, s: str) -> None:
        self.lines.append(s)

    def flush(self) -> None:
        pass


def _capture_stdout():
    capture = _Capture()
    original = sys.stdout
    sys.stdout = capture
    return capture, original


def test_emit_response_format(monkeypatch):
    import worker_main
    capture, original = _capture_stdout()
    monkeypatch.setattr(worker_main, "_stdout_lock", threading.Lock())
    try:
        worker_main.emit_response(42, {"success": True, "data": {"a": 1}, "error": None})
    finally:
        sys.stdout = original
    line = capture.lines[0].strip()
    msg = json.loads(line)
    assert msg["id"] == 42
    assert msg["result"]["success"] is True
    assert msg["result"]["data"] == {"a": 1}


def test_emit_event_format(monkeypatch):
    import worker_main
    capture, original = _capture_stdout()
    monkeypatch.setattr(worker_main, "_stdout_lock", threading.Lock())
    try:
        worker_main.emit_event("screenshot", {"path": "a.png"})
    finally:
        sys.stdout = original
    msg = json.loads(capture.lines[0].strip())
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