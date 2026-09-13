"""Worker 核心逻辑测试：浏览器参数构建、超时归一化、取消注册表、
IPC 响应/事件序列化，以及状态真值判定。

这些均为纯逻辑，不启动真实浏览器，可在无 Playwright 运行时下运行。
"""

from __future__ import annotations

import asyncio
import builtins
import json
import os
import tempfile
from pathlib import Path
from types import SimpleNamespace

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


def test_custom_browser_health_requires_existing_file():
    from playwright_worker import _ensure_browser

    with tempfile.TemporaryDirectory() as td:
        executable = Path(td) / "browser.exe"
        assert _ensure_browser("custom", str(executable)) is False
        executable.write_bytes(b"fake")
        assert _ensure_browser("custom", str(executable)) is True
        assert _ensure_browser(" CUSTOM ", f"  {executable}  ") is True


def test_task_screenshot_cleanup_is_deferred(monkeypatch):
    import playwright_worker
    from playwright_worker import WorkerCore
    from step_handlers import StepContext

    monkeypatch.setattr(playwright_worker, "TASK_SCREENSHOT_RETENTION_SECS", 0)

    async def run(path: Path):
        core = WorkerCore()
        path.write_bytes(b"png")
        context = StepContext(page=None, screenshots=[str(path)])
        core._defer_task_screenshot_cleanup(context)
        assert path.exists()
        assert context.screenshots == []
        await asyncio.gather(*list(core._screenshot_cleanup_tasks))
        assert not path.exists()

    with tempfile.TemporaryDirectory() as td:
        asyncio.run(run(Path(td) / "shot.png"))


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
    # 默认反检测（无副作用；其余推荐参数走前端按钮显式写入）
    assert "--disable-blink-features=AutomationControlled" in args


def test_build_launch_args_firefox_skips_chromium_flags():
    from playwright_worker import WorkerCore
    core = WorkerCore()
    args = core._build_launch_args({}, channel="firefox")
    assert "--no-sandbox" not in args
    assert "--disable-gpu" not in args
    assert "--disable-blink-features=AutomationControlled" not in args


def test_is_chromium_channel_covers_custom_engine():
    from playwright_worker import WorkerCore
    assert WorkerCore._is_chromium_channel({}, "chromium") is True
    assert WorkerCore._is_chromium_channel({}, "playwright") is True
    assert WorkerCore._is_chromium_channel({}, "msedge") is True
    assert WorkerCore._is_chromium_channel({}, "firefox") is False
    assert WorkerCore._is_chromium_channel({}, "webkit") is False
    # custom 路径按引擎细分：firefox/webkit 引擎不算 Chromium
    assert WorkerCore._is_chromium_channel({}, "custom") is True
    assert WorkerCore._is_chromium_channel({"custom_browser_engine": "firefox"}, "custom") is False
    assert WorkerCore._is_chromium_channel({"custom_browser_engine": "webkit"}, "custom") is False


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


def test_force_interrupt_tears_down_debug_session(tmp_path):
    from playwright_worker import WorkerCore, cancel_registry

    class FakePage:
        def __init__(self):
            self.closed = False

        async def close(self):
            self.closed = True

    page = FakePage()
    screenshot = tmp_path / "credential.jpg"
    screenshot.write_bytes(b"sensitive")
    cancel_id = "debug-force-interrupt-test"
    cancel_registry.register(cancel_id)
    session = SimpleNamespace(
        session_id="session-1",
        page=page,
        context=SimpleNamespace(screenshots=[str(screenshot)]),
        cancel_id=cancel_id,
    )
    core = WorkerCore()
    core._page = page
    core._debug_sessions[session.session_id] = session

    asyncio.run(core.force_interrupt_pending())

    assert page.closed is True
    assert core._page is None
    assert core._debug_sessions == {}
    assert screenshot.exists() is False
    assert session.context.screenshots == []
    assert session.cancel_id == ""
    assert cancel_id not in cancel_registry._events


def test_purge_stale_debug_screenshots_covers_jpeg(monkeypatch, tmp_path):
    import playwright_worker as worker

    files = [tmp_path / "old.png", tmp_path / "old.jpg", tmp_path / "old.jpeg"]
    keep = tmp_path / "note.txt"
    for path in [*files, keep]:
        path.write_bytes(b"x")
        # 固定早于模块加载时刻，避免文件系统时间精度导致边界不稳定。
        os.utime(path, (1, 1))
    monkeypatch.setattr(worker, "_debug_screenshot_dir", lambda: tmp_path)
    monkeypatch.setattr(worker, "_MODULE_LOAD_TIME", 2)

    worker._purge_stale_debug_screenshots()

    assert all(not path.exists() for path in files)
    assert keep.exists()


def test_purge_stale_debug_screenshots_removes_feedback_directory(monkeypatch, tmp_path):
    import playwright_worker as worker

    stale = tmp_path / "feedback-old"
    stale.mkdir()
    (stale / "page.html").write_text("secret", encoding="utf-8")
    os.utime(stale, (1, 1))
    monkeypatch.setattr(worker, "_debug_screenshot_dir", lambda: tmp_path)
    monkeypatch.setattr(worker, "_MODULE_LOAD_TIME", 2)

    worker._purge_stale_debug_screenshots()

    assert not stale.exists()


# ── 反馈资源快照（feedback_capture 纯函数）──


def test_feedback_capture_dir_matches_rust_path_guard(monkeypatch, tmp_path):
    from playwright_worker import _feedback_capture_dir

    # Rust PathGuard 只允许 <base>/python_worker/debug，Worker 必须落盘在同一边界内
    (tmp_path / "python_worker").mkdir()
    monkeypatch.setenv("CAMPUS_AUTH_BASE_PATH", str(tmp_path))

    assert _feedback_capture_dir("123") == (
        tmp_path / "python_worker" / "debug" / "feedback-123"
    )


def test_resource_ext_by_mime():
    from playwright_worker import _resource_ext

    assert _resource_ext("text/css") == "css"
    assert _resource_ext("text/css; charset=utf-8") == "css"
    for js in ("application/javascript", "text/javascript", "application/x-javascript"):
        assert _resource_ext(js) == "js", js
    # 未知/缺失类型统一 txt 兜底
    assert _resource_ext("image/png") == "txt"
    assert _resource_ext("") == "txt"
    assert _resource_ext(None) == "txt"


def test_rewrite_resource_urls_raw_and_escaped():
    from playwright_worker import _rewrite_resource_urls

    mapping = {
        "https://cdn.example.com/app.js?v=1&t=2": "resources/abc123.js",
        "https://cdn.example.com/style.css": "resources/def456.css",
    }
    html = (
        '<script src="https://cdn.example.com/app.js?v=1&amp;t=2"></script>'
        '<link rel="stylesheet" href="https://cdn.example.com/style.css">'
    )
    out = _rewrite_resource_urls(html, mapping)
    # 原样与 &amp; 转义两种形态都要被改写为本地相对路径
    assert "cdn.example.com/app.js" not in out
    assert "cdn.example.com/style.css" not in out
    assert 'src="resources/abc123.js"' in out
    assert 'href="resources/def456.css"' in out
    # 空映射原样返回
    assert _rewrite_resource_urls(html, {}) == html


def test_rewrite_resource_urls_protocol_relative():
    from playwright_worker import _rewrite_resource_urls

    # DOM 属性常见协议相对写法（//host/path），资源树上报绝对 URL，也需改写
    mapping = {"https://s1.example.com/bfs/app.js": "resources/abc123.js"}
    html = '<script src="//s1.example.com/bfs/app.js"></script>'
    out = _rewrite_resource_urls(html, mapping)
    assert 'src="resources/abc123.js"' in out
    assert "s1.example.com" not in out


def test_url_scheme_variants_order():
    from playwright_worker import _url_scheme_variants

    # 绝对形态在前、协议相对在后（http:// 内含 // 不能先被替换）
    assert _url_scheme_variants("https://a.com/x.js") == [
        "https://a.com/x.js",
        "http://a.com/x.js",
        "//a.com/x.js",
    ]
    assert _url_scheme_variants("//a.com/x.js") == ["//a.com/x.js"]


# ── OCR 依赖主线程预加载 ──


def _patch_imports(monkeypatch, *, ddddocr_ok, numpy_ok):
    """按需拦截 ddddocr / numpy 顶层 import，返回调用记录。"""
    calls = {"ddddocr": 0, "numpy": 0}
    # 补丁前先捕获真实 __import__：补丁期间任何其他 import（如 importlib.util）
    # 经 builtins.__import__ 递归回 fake_import 会直接 RecursionError
    real_import = builtins.__import__

    def fake_import(name, *args, **kwargs):
        if name == "ddddocr":
            calls["ddddocr"] += 1
            if not ddddocr_ok:
                raise ImportError("no ddddocr")
            mod = type("mod", (), {})()
        elif name == "numpy":
            calls["numpy"] += 1
            if not numpy_ok:
                raise ImportError("no numpy")
            mod = type("mod", (), {})()
        else:
            # 其余模块（含 sys/logger 依赖）照常走真实 import
            return real_import(name, *args, **kwargs)
        return mod

    monkeypatch.setattr("builtins.__import__", fake_import)
    return calls


def test_preload_ocr_deps_full_load_when_ddddocr_present(monkeypatch):
    import builtins

    import worker_main
    calls = _patch_imports(monkeypatch, ddddocr_ok=True, numpy_ok=True)
    # force=True 直达完整加载路径（force=False 启动期先走能力探测门控，见下方门控测试）
    worker_main._preload_ocr_deps(force=True)
    # 完整加载路径：仅 import ddddocr 一次即返回，不再额外 import numpy
    assert calls["ddddocr"] == 1
    assert calls["numpy"] == 0
    # 任务 10：完整加载 → 能力上报 ocr=True
    assert worker_main.OCR_CAPABILITIES == {"ocr": True}
    assert builtins.__import__ is not None


def test_preload_ocr_deps_falls_back_to_numpy(monkeypatch):
    import worker_main
    calls = _patch_imports(monkeypatch, ddddocr_ok=False, numpy_ok=True)
    worker_main._preload_ocr_deps(force=True)
    # ddddocr 缺失（未安装/不完整）时退化为仅预加载 numpy
    assert calls["ddddocr"] == 1
    assert calls["numpy"] == 1
    # numpy-only 不具备 OCR 识别能力（任务 10）
    assert worker_main.OCR_CAPABILITIES == {"ocr": False}


def test_preload_ocr_deps_silent_when_missing(monkeypatch):
    import worker_main
    calls = _patch_imports(monkeypatch, ddddocr_ok=False, numpy_ok=False)
    # 两者都缺失时静默跳过，不抛异常（Worker 仍能正常启动）
    worker_main._preload_ocr_deps(force=True)
    assert calls["ddddocr"] == 1
    assert calls["numpy"] == 1
    assert worker_main.OCR_CAPABILITIES == {"ocr": False}


def test_preload_ocr_deps_gate_probes_only_without_force(monkeypatch):
    import importlib.util

    import worker_main
    calls = _patch_imports(monkeypatch, ddddocr_ok=True, numpy_ok=True)
    # 模拟 ddddocr 未安装（与真实 venv 是否装有无关）：探测层 find_spec 短路，
    # 不触发任何被拦截的 import，能力保持 False 且不产生 import WARN
    monkeypatch.setattr(importlib.util, "find_spec", lambda _name: None)
    worker_main._preload_ocr_deps()
    assert calls == {"ddddocr": 0, "numpy": 0}
    assert worker_main.OCR_CAPABILITIES == {"ocr": False}


def test_preload_ocr_deps_survives_dll_load_errors(monkeypatch):
    import builtins

    import worker_main
    real_import = builtins.__import__

    def fake_import(name, *args, **kwargs):
        if name in ("ddddocr", "numpy"):
            raise OSError(f"{name} DLL load failed")
        return real_import(name, *args, **kwargs)

    monkeypatch.setattr("builtins.__import__", fake_import)
    # 损坏的 OCR DLL 不得阻止 Worker 启动，否则普通手动登录也会一起失效。
    worker_main._preload_ocr_deps(force=True)


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


def test_oversized_request_id_only_reads_bounded_prefix():
    import worker_main

    assert worker_main._oversized_request_id('{"id":42,"method":"ocr_recognize"}') == 42
    assert worker_main._oversized_request_id('{"method":"x","id":42}') is None
    assert worker_main._oversized_request_id("not-json") is None


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
    # P6：补 outcome 字段，保证 IPC 响应结构一致
    assert d == {
        "success": False,
        "data": {
            "outcome": "unknown_error",
            "message": "未知命令: foo",
            "duration_ms": 0,
            "screenshots": [],
        },
        "error": "未知命令: foo",
    }


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
            self.ranges = None

        def set_ranges(self, value):
            self.ranges = value

    fake.DdddOcr = FakeDdddOcr
    monkeypatch.setitem(sys.modules, "ddddocr", fake)
    a = _get_ocr(False)
    b = _get_ocr(False)
    assert a is b  # 两次获取返回同一实例
    c = _get_ocr(True)
    assert c is not a  # 不同 old 参数单独缓存
    digits = _get_ocr(False, "0123456789")
    assert digits is not a  # 字符范围不同必须使用独立实例，避免污染默认模型
    assert digits.ranges == "0123456789"
    assert _get_ocr(False, "0123456789") is digits
    assert _get_ocr(False, ["invalid"]) is a  # 脏 JSON 值退回默认模型


def test_worker_health_check_does_not_probe_browser():
    """轻量健康检查：不探测浏览器，且携带版本与能力上报（任务 10）。"""
    import asyncio
    from playwright_worker import WORKER_VERSION, worker_core

    worker_core.capabilities = {"ocr": True}
    result = asyncio.run(worker_core.handle_worker_health_check({}))
    assert result["healthy"] is True
    # 向后兼容新增字段：version 与 pyproject 同步常量一致；capabilities 透传
    assert result["version"] == WORKER_VERSION
    assert result["capabilities"] == {"ocr": True}
    # 未注入能力时 capabilities 为空 dict（Rust 侧回退文件探测）
    worker_core.capabilities = {}
    result2 = asyncio.run(worker_core.handle_worker_health_check({}))
    assert result2["capabilities"] == {}


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

        async def hang_handler(params):
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
        worker_main.COMMANDS["test_ok"] = lambda params: called.append(1) or {}
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


# ── B5: 任务间脏状态防护 ──

def test_new_page_registers_dialog_accept():
    """_new_page 注册 dialog accept 处理器，自动点“确定”继续，防残留 alert 卡死后续导航。"""

    async def _run():
        from playwright_worker import WorkerCore

        core = WorkerCore()

        class FakeContext:
            async def new_page(self):
                return FakePage()

        class FakePage:
            def __init__(self):
                self.handlers = {}

            def on(self, event, handler):
                self.handlers[event] = handler

        core._context = FakeContext()
        page = await core._new_page()
        assert "dialog" in page.handlers, "应注册 dialog 处理器"
        # 触发 handler：accept 被调用
        accepted = []

        class FakeDialog:
            message = "账号或密码错误！"

            async def accept(self):
                accepted.append(True)

        page.handlers["dialog"](FakeDialog())
        # 给 ensure_future 调度时间
        await asyncio.sleep(0.05)
        assert accepted == [True], "dialog 处理器应调用 accept"
        # 弹窗文案被收集进当前任务列表（随 StructuredResult 上报给 Rust 登录日志）
        assert core._task_dialogs == ["账号或密码错误！"]

    asyncio.run(_run())


def test_handle_close_browser_closes_and_keeps_worker():
    """close_browser 命令：关闭浏览器资源，Worker 进程语义保留（不触碰 shutdown_event）。"""

    async def _run():
        from playwright_worker import WorkerCore

        core = WorkerCore()
        closed = []
        orig = WorkerCore.close_browser

        async def fake_close(self):
            closed.append(True)

        WorkerCore.close_browser = fake_close
        try:
            result = await core.handle_close_browser({})
        finally:
            WorkerCore.close_browser = orig
        assert result == {}
        assert closed == [True], "应调用 close_browser 清理浏览器"

    asyncio.run(_run())


def test_handle_close_browser_preserve_state_keeps_everything():
    """preserve_state=true（keep_alive 且登录成功）：页面与登录状态原样保留。"""

    async def _run():
        import playwright_worker as pw
        from playwright_worker import WorkerCore

        core = WorkerCore()
        core._page = object()
        core._context = object()
        core._last_browser_settings = {"headless": True}

        async def _fail_close(self):
            raise AssertionError("preserve_state 时不应调用 close_browser")

        async def _fail_session(self):
            raise AssertionError("preserve_state 时不应调用 _close_session")

        orig_close, orig_session = WorkerCore.close_browser, WorkerCore._close_session
        WorkerCore.close_browser = _fail_close
        WorkerCore._close_session = _fail_session
        try:
            result = await core.handle_close_browser({"preserve_state": True})
        finally:
            WorkerCore.close_browser = orig_close
            WorkerCore._close_session = orig_session
        assert result == {}
        # 页面、context 与配置快照全部原样保留
        assert core._page is not None and core._context is not None
        assert core._last_browser_settings == {"headless": True}

    asyncio.run(_run())


def test_handle_close_browser_keep_alive_session_release():
    """keep_alive 环境开启（非成功终态）：会话级释放，保留浏览器进程。"""

    async def _run():
        import playwright_worker as pw
        from playwright_worker import WorkerCore

        core = WorkerCore()
        released = []

        async def fake_session(self):
            released.append(True)

        async def _fail_close(self):
            raise AssertionError("keep_alive 会话级档不应全量 close_browser")

        orig_env = pw._WORKER_KEEP_ALIVE
        orig_session, orig_close = WorkerCore._close_session, WorkerCore.close_browser
        pw._WORKER_KEEP_ALIVE = True
        WorkerCore._close_session = fake_session
        WorkerCore.close_browser = _fail_close
        try:
            result = await core.handle_close_browser({})
        finally:
            pw._WORKER_KEEP_ALIVE = orig_env
            WorkerCore._close_session = orig_session
            WorkerCore.close_browser = orig_close
        assert result == {}
        assert released == [True]

    asyncio.run(_run())


def test_close_session_keeps_browser_process():
    """_close_session：只关页面与非持久化 context，保留浏览器进程与配置快照。"""

    async def _run():
        from playwright_worker import WorkerCore

        core = WorkerCore()
        closed = []

        class FakeRes:
            def __init__(self, name):
                self.name = name

            async def close(self):
                closed.append(self.name)

        browser = FakeRes("browser")
        core._page = FakeRes("page")
        core._context = FakeRes("context")
        core._browser = browser
        core._last_browser_settings = {"headless": True}
        await core._close_session()
        assert closed == ["page", "context"], "应只关闭页面与 context"
        assert core._page is None and core._context is None
        assert core._browser is browser, "浏览器进程应保留"
        assert core._last_browser_settings == {"headless": True}, "配置快照应保留"

    asyncio.run(_run())


def test_close_session_persistent_keeps_context():
    """persistent_context 模式（_browser 为 None）：context 即浏览器本体，不可关。"""

    async def _run():
        from playwright_worker import WorkerCore

        core = WorkerCore()
        closed = []

        class FakeRes:
            async def close(self):
                closed.append(True)

        ctx = FakeRes()
        core._page = None
        core._context = ctx
        core._browser = None
        await core._close_session()
        assert closed == [], "persistent 模式不应关闭 context"
        assert core._context is ctx, "persistent context 应保留"

    asyncio.run(_run())


def test_ensure_browser_hot_recovery_reuses_process():
    """会话级释放后热恢复：进程存活且配置未变 → 只重建 context+page，不重启进程。"""

    async def _run():
        from playwright_worker import WorkerCore

        class FakePage:
            def __init__(self):
                self.handlers = {}

            def on(self, event, cb):
                self.handlers[event] = cb

        class FakeCtx:
            async def new_page(self):
                return FakePage()

        class FakeBrowser:
            def __init__(self):
                self.context_calls = 0

            def is_connected(self):
                return True

            async def new_context(self, **kwargs):
                self.context_calls += 1
                return FakeCtx()

        core = WorkerCore()
        browser = FakeBrowser()
        core._browser = browser
        core._context = None
        core._last_browser_settings = {"pure_mode": True}
        launched = []

        async def fail_start(self, config):
            launched.append(True)

        orig_start = WorkerCore._start_browser
        WorkerCore._start_browser = fail_start
        try:
            await core.ensure_browser({"browser_settings": {"pure_mode": True}})
        finally:
            WorkerCore._start_browser = orig_start
        assert launched == [], "热恢复不应重启浏览器进程"
        assert browser.context_calls == 1, "应只重建 context"
        assert core._context is not None and core._page is not None

    asyncio.run(_run())


def test_ensure_browser_hot_recovery_falls_back_on_error():
    """热恢复建 context 失败 → 回退完整重建（close_browser + _start_browser）。"""

    async def _run():
        from playwright_worker import WorkerCore

        class FakeBrowser:
            def is_connected(self):
                return True

            async def new_context(self, **kwargs):
                raise RuntimeError("模拟建 context 失败")

        core = WorkerCore()
        core._browser = FakeBrowser()
        core._context = None
        core._last_browser_settings = {"pure_mode": True}
        started = []

        async def fake_start(self, config):
            started.append(True)

        async def fake_close(self):
            pass

        orig_start, orig_close = WorkerCore._start_browser, WorkerCore.close_browser
        WorkerCore._start_browser = fake_start
        WorkerCore.close_browser = fake_close
        try:
            await core.ensure_browser({"browser_settings": {"pure_mode": True}})
        finally:
            WorkerCore._start_browser = orig_start
            WorkerCore.close_browser = orig_close
        assert started == [True], "热恢复失败应回退完整重建"

    asyncio.run(_run())


def test_browser_idle_release_fires_and_cancels():
    """浏览器空闲自动释放：到期触发全量关闭；取消后不再触发；keep_alive 时不武装。"""

    async def _run():
        import playwright_worker as pw
        from playwright_worker import WorkerCore

        core = WorkerCore()
        core._browser = object()
        closed = []

        async def fake_close(self):
            closed.append(True)

        orig_close = WorkerCore.close_browser
        orig_secs = pw.BROWSER_IDLE_RELEASE_SECS
        orig_env = pw._WORKER_KEEP_ALIVE
        WorkerCore.close_browser = fake_close
        pw.BROWSER_IDLE_RELEASE_SECS = 0.05
        pw._WORKER_KEEP_ALIVE = False
        try:
            # 到期触发
            core._arm_browser_idle_release()
            assert core._browser_idle_task is not None
            await asyncio.sleep(0.15)
            assert closed == [True], "空闲到期应自动释放浏览器"
            assert core._browser_idle_task is None
            # 取消后不触发
            core._browser = object()
            core._arm_browser_idle_release()
            core._cancel_browser_idle_release()
            await asyncio.sleep(0.1)
            assert closed == [True], "取消后不应触发"
            # keep_alive 时武装为 no-op
            pw._WORKER_KEEP_ALIVE = True
            core._arm_browser_idle_release()
            assert core._browser_idle_task is None, "keep_alive 时不装空闲回收"
        finally:
            WorkerCore.close_browser = orig_close
            pw.BROWSER_IDLE_RELEASE_SECS = orig_secs
            pw._WORKER_KEEP_ALIVE = orig_env

    asyncio.run(_run())


# ── 托管渠道浏览器探测（_ensure_browser / registry + 安装完成标记）──
#
# 判定语义与 Rust 侧 environment::browser_registry 一致：同一批用例两端各有测试，
# 任一侧漂移（例如退回「目录非空」判据、或漏掉 headless shell）都会被这里抓到。


def _install_fake_sync_playwright(monkeypatch, handler):
    """用假模块替换 playwright.sync_api：handler(sync_playwright 调用时) 决定行为。"""
    import sys
    import types

    fake_api = types.ModuleType("playwright.sync_api")
    fake_api.sync_playwright = handler
    monkeypatch.setitem(sys.modules, "playwright.sync_api", fake_api)


def _chromium_registry(tmp: Path, revision: str = "1234") -> Path:
    """写一份最小 registry（含 chromium 与 headless shell 两条默认安装项）"""
    registry = tmp / "browsers.json"
    registry.write_text(
        json.dumps(
            {
                "browsers": [
                    {"name": "chromium", "revision": revision, "installByDefault": True},
                    {
                        "name": "chromium-headless-shell",
                        "revision": revision,
                        "installByDefault": True,
                    },
                ]
            }
        ),
        encoding="utf-8",
    )
    return registry


def _bind_probe_paths(monkeypatch, pw, cache_dir: Path, registry: Path | None) -> None:
    """把缓存目录与 registry 指到临时位置，并清空必需目录名缓存"""
    monkeypatch.setattr(pw, "_browser_cache_dir", lambda: cache_dir)
    monkeypatch.setattr(pw, "_registry_path", lambda: registry)
    monkeypatch.setattr(pw, "_REQUIRED_DIRS_CACHE", {})


def _write_browser_dir(cache_dir: Path, name: str, *, complete: bool) -> None:
    """写入浏览器目录；complete=True 时带 Playwright 的安装完成标记"""
    from playwright_worker import _INSTALLATION_COMPLETE

    target = cache_dir / name
    (target / "payload").mkdir(parents=True, exist_ok=True)
    if complete:
        (target / _INSTALLATION_COMPLETE).write_bytes(b"")


def test_managed_probe_requires_installation_marker(monkeypatch):
    """核心回归：目录非空但缺完成标记 → 未安装（旧实现据此误报就绪）"""
    import playwright_worker as pw

    with tempfile.TemporaryDirectory() as tmp:
        tmp_path = Path(tmp)
        cache = tmp_path / "cache"
        _write_browser_dir(cache, "chromium-1234", complete=False)
        _write_browser_dir(cache, "chromium_headless_shell-1234", complete=False)
        _bind_probe_paths(monkeypatch, pw, cache, _chromium_registry(tmp_path))

        assert pw._ensure_browser("playwright") is False
        assert pw._ensure_browser("chromium") is False


def test_managed_probe_requires_headless_shell(monkeypatch):
    """只装 headful chromium、缺 headless shell → 未安装

    默认后台运行走 headless，旧的 executable_path 判据只认 headful 路径，会漏判此情形。
    """
    import playwright_worker as pw

    with tempfile.TemporaryDirectory() as tmp:
        tmp_path = Path(tmp)
        cache = tmp_path / "cache"
        _write_browser_dir(cache, "chromium-1234", complete=True)
        _bind_probe_paths(monkeypatch, pw, cache, _chromium_registry(tmp_path))

        assert pw._ensure_browser("chromium") is False


def test_managed_probe_accepts_complete_components(monkeypatch):
    """两套组件都有完成标记 → 可用（并验证历史别名 playwright 同渠道、大小写无关）"""
    import playwright_worker as pw

    with tempfile.TemporaryDirectory() as tmp:
        tmp_path = Path(tmp)
        cache = tmp_path / "cache"
        _write_browser_dir(cache, "chromium-1234", complete=True)
        _write_browser_dir(cache, "chromium_headless_shell-1234", complete=True)
        _bind_probe_paths(monkeypatch, pw, cache, _chromium_registry(tmp_path))

        assert pw._ensure_browser("playwright") is True
        assert pw._ensure_browser("chromium") is True
        assert pw._ensure_browser("  PLAYWRIGHT  ") is True


def test_managed_probe_unknown_channel_is_unavailable(monkeypatch):
    """未知渠道不可用（与 Rust is_channel_available 一致，不再笼统当作 chromium）"""
    import playwright_worker as pw

    with tempfile.TemporaryDirectory() as tmp:
        cache = Path(tmp) / "cache"
        _bind_probe_paths(monkeypatch, pw, cache, None)
        assert pw._ensure_browser("safari") is False
        assert pw._ensure_browser("chrome-headless") is False


def test_managed_probe_does_not_start_driver(monkeypatch):
    """探测不再冷启 sync_playwright driver（旧实现为取 executable_path 需 200-500ms）"""
    import playwright_worker as pw

    with tempfile.TemporaryDirectory() as tmp:
        tmp_path = Path(tmp)
        cache = tmp_path / "cache"
        _write_browser_dir(cache, "chromium-1234", complete=True)
        _write_browser_dir(cache, "chromium_headless_shell-1234", complete=True)
        _bind_probe_paths(monkeypatch, pw, cache, _chromium_registry(tmp_path))

        def _boom(*args, **kwargs):
            raise AssertionError("新判定不应启动 sync_playwright driver")

        _install_fake_sync_playwright(monkeypatch, _boom)
        assert pw._ensure_browser("playwright") is True


def test_managed_probe_honours_browsers_path_override(monkeypatch):
    """PLAYWRIGHT_BROWSERS_PATH 生效（不替换 _browser_cache_dir，走真实解析）"""
    import playwright_worker as pw

    with tempfile.TemporaryDirectory() as tmp:
        tmp_path = Path(tmp)
        cache = tmp_path / "cache"
        _write_browser_dir(cache, "chromium-1234", complete=True)
        _write_browser_dir(cache, "chromium_headless_shell-1234", complete=True)
        monkeypatch.setenv("PLAYWRIGHT_BROWSERS_PATH", str(cache))
        monkeypatch.setattr(pw, "_registry_path", lambda: _chromium_registry(tmp_path))
        monkeypatch.setattr(pw, "_REQUIRED_DIRS_CACHE", {})

        assert pw._browser_cache_dir() == cache
        assert pw._ensure_browser("chromium") is True


def test_managed_probe_fallback_without_registry(monkeypatch):
    """registry 不可读 → 回退：任一匹配前缀目录安装完成即视为可用"""
    import playwright_worker as pw

    with tempfile.TemporaryDirectory() as tmp:
        cache = Path(tmp) / "cache"
        _bind_probe_paths(monkeypatch, pw, cache, None)
        # 空目录 → 未安装
        assert pw._ensure_browser("chromium") is False
        # 非空但无完成标记 → 仍未安装（旧实现会误判）
        _write_browser_dir(cache, "chromium-1234", complete=False)
        assert pw._ensure_browser("chromium") is False
        # 补完成标记 → 可用
        _write_browser_dir(cache, "chromium-1234", complete=True)
        assert pw._ensure_browser("chromium") is True


def test_managed_probe_platform_overrides_fall_back(monkeypatch):
    """带 revisionOverrides 的引擎（webkit）走回退判定而非精确 revision"""
    import playwright_worker as pw

    with tempfile.TemporaryDirectory() as tmp:
        tmp_path = Path(tmp)
        cache = tmp_path / "cache"
        registry = tmp_path / "browsers.json"
        registry.write_text(
            json.dumps(
                {
                    "browsers": [
                        {
                            "name": "webkit",
                            "revision": "2336",
                            "installByDefault": True,
                            "revisionOverrides": {"mac14": "2251"},
                        }
                    ]
                }
            ),
            encoding="utf-8",
        )
        _bind_probe_paths(monkeypatch, pw, cache, registry)
        _write_browser_dir(cache, "webkit-2251", complete=True)
        assert pw._ensure_browser("webkit") is True
        assert pw._ensure_browser("firefox") is False


def test_missing_components_falls_back_to_prefixes(monkeypatch):
    """未就绪时诊断信息不为空：不可精确推导（如 webkit 平台差异）时回退为期望前缀"""
    import playwright_worker as pw

    with tempfile.TemporaryDirectory() as tmp:
        tmp_path = Path(tmp)
        cache = tmp_path / "cache"
        _bind_probe_paths(monkeypatch, pw, cache, None)
        assert pw._missing_components("webkit") == ["webkit-*"]
        assert pw._missing_components("chromium") == [
            "chromium-*",
            "chromium_headless_shell-*",
        ]

        # registry 可读时给出精确目录名（只装了 headful → 报缺 headless shell）
        _write_browser_dir(cache, "chromium-1234", complete=True)
        _bind_probe_paths(monkeypatch, pw, cache, _chromium_registry(tmp_path))
        assert pw._missing_components("chromium") == ["chromium_headless_shell-1234"]


# ── B3: 调试会话期间拒绝登录/浏览器任务（Python 半防御）──


def test_execute_login_attempt_rejected_while_debug_session_active():
    """调试会话存续期内 execute_login_attempt 快速失败（BUSY 语义→UNKNOWN_ERROR）。"""

    async def _run():
        from models import Outcome
        from playwright_worker import DebugSession, WorkerCore, StepContext
        from step_handlers import WorkerError

        core = WorkerCore()
        core._debug_sessions["d1"] = DebugSession(
            session_id="d1", page=None, task_config=None, context=StepContext(page=None)
        )
        with pytest.raises(WorkerError) as ei:
            await core.handle_execute_login_attempt({})
        # 无 BUSY 变体（避免破坏与 Rust 的 serde 契约），复用 UNKNOWN_ERROR，
        # 消息明确说明「调试会话进行中」
        assert ei.value.outcome == Outcome.UNKNOWN_ERROR.value
        assert "调试会话进行中" in ei.value.message

    asyncio.run(_run())


def test_execute_browser_task_rejected_while_debug_session_active():
    """调试会话存续期内 execute_browser_task 同样快速失败。"""

    async def _run():
        from playwright_worker import DebugSession, WorkerCore, StepContext
        from step_handlers import WorkerError

        core = WorkerCore()
        core._debug_sessions["d1"] = DebugSession(
            session_id="d1", page=None, task_config=None, context=StepContext(page=None)
        )
        with pytest.raises(WorkerError) as ei:
            await core.handle_execute_browser_task({})
        assert "调试会话进行中" in ei.value.message

    asyncio.run(_run())


# ── B6: 命令级超时兜底为 Rust 默认的 0.9 倍 ──


def test_command_timeout_floor_is_270s():
    """无 browser_settings 时兜底 270s（Rust 默认 300s × 0.9，先于 Rust 触发自愈）。"""
    import worker_main

    assert worker_main._command_timeout({}) == 270.0
    assert worker_main._command_timeout({"browser_settings": {}}) == 270.0


def test_command_timeout_scales_with_step_timeout():
    """超大 step timeout 配置仍按基准放大（不低于 270s 兜底）。"""
    import worker_main

    # timeout 单位为秒（_to_ms ×1000），20s × 20 = 400s
    assert worker_main._command_timeout(
        {"browser_settings": {"timeout": 20}}
    ) == 400.0
