"""Worker 健壮性修复回归测试。

覆盖：命令级超时预算 0.9x 全程权威、browser_settings 非正值超时回退、
畸形命令（params/method）防护、_close_session / 热恢复清 _wired_page_ids、
close_browser 内部兜底先于命令级自愈、feedback_capture 取消与 CDP 释放、
资源快照 iframe 覆盖、page_capture 分阶段取消、错误文本 URL token 剥离。

全部为纯逻辑 / stub 测试，不启动真实浏览器。
"""

from __future__ import annotations

import asyncio
import json
import tempfile
from pathlib import Path

import pytest

import playwright_worker
import worker_main
from playwright_worker import (
    _PAGE_RESOURCE_PROBE_JS,
    _capture_page_resources,
    _iter_cdp_frame_resources,
    _to_ms,
    WorkerCore,
    cancel_registry,
)
from step_handlers import StepCancelled, WorkerError, _safe_op, _sanitize_error


def _run(coro):
    return asyncio.run(coro)


# ── 修复 1：命令级超时预算 0.9x 全程权威 ──


def test_command_timeout_budget_600s_yields_540s():
    """预算 600s（任务级钳制上限）→ 540s，不再被 270s 地板或放大基准截断。"""
    assert worker_main._command_timeout({"rust_timeout_ms": 600_000}) == 540.0
    # 放大基准（timeout=20 → 400s）与 270s 地板都不得把长预算往下压
    assert (
        worker_main._command_timeout(
            {"browser_settings": {"timeout": 20}, "rust_timeout_ms": 600_000}
        )
        == 540.0
    )


def test_command_timeout_budget_8s_yields_7_2s():
    """短预算（close_browser 8s）→ 7.2s：Python 自愈先于 Rust 超时。"""
    assert worker_main._command_timeout({"rust_timeout_ms": 8_000}) == pytest.approx(7.2)


def test_command_timeout_no_budget_falls_back_to_base():
    """无预算字段（旧主程序）回退固定兜底公式（与既有 270s 测试互为补充）。"""
    assert worker_main._command_timeout({"browser_settings": {"timeout": 3}}) == 270.0
    assert worker_main._command_timeout({"rust_timeout_ms": 0}) == 270.0
    assert worker_main._command_timeout({"rust_timeout_ms": -1}) == 270.0


# ── 修复 2：_to_ms 非正值回退 default_ms ──


def test_to_ms_non_positive_falls_back_to_default():
    """timeout=0 不能变成 1ms/0ms 瞬间失败：缺省、非法、非正值统一回退。"""
    assert _to_ms({"timeout": 0}, "timeout", 10000) == 10000
    assert _to_ms({"timeout": -5}, "timeout", 10000) == 10000
    assert _to_ms({"navigation_timeout": 0}, "navigation_timeout", 15000) == 15000
    assert _to_ms({"timeout": 0.0}, "timeout", 10000) == 10000
    # 正常值行为不变
    assert _to_ms({"timeout": 3}, "timeout", 10000) == 3000
    assert _to_ms({"timeout": "15"}, "timeout", 10000) == 15000


# ── 修复 3：畸形命令防护 ──


def test_dispatch_guarded_rejects_non_dict_params(monkeypatch, capsys):
    """params 为列表 → 回错误响应且 Worker 存活（后续命令正常执行）。"""
    _run(_assert_malformed_rejected(monkeypatch, capsys))


async def _assert_malformed_rejected(monkeypatch, capsys):
    await worker_main._dispatch_guarded({"id": 1, "method": "ping", "params": []})
    # method 不可哈希：老实现 COMMANDS.get 抛 TypeError 且永不回包
    await worker_main._dispatch_guarded({"id": 2, "method": ["x"], "params": {}})
    lines = capsys.readouterr().out.strip().splitlines()
    assert len(lines) == 2
    for line, msg_id in zip(lines, (1, 2)):
        msg = json.loads(line)
        assert msg["id"] == msg_id
        assert msg["result"]["success"] is False
        assert msg["result"]["data"]["outcome"] == "unknown_error"

    # Worker 存活：后续合法命令仍正常执行并成功回包
    await worker_main._dispatch_guarded({"id": 3, "method": "worker_health_check"})
    lines = capsys.readouterr().out.strip().splitlines()
    assert len(lines) == 1
    msg = json.loads(lines[0])
    assert msg["id"] == 3
    assert msg["result"]["success"] is True
    assert msg["result"]["data"]["healthy"] is True


def test_dispatch_guarded_error_response_after_emit_failure(monkeypatch):
    """首发回包写出失败（emit 抛异常）→ 守卫解除，异常兜底补发错误响应。"""

    async def _run_case():
        calls: list[tuple[object, dict]] = []

        def flaky_emit_response(mid, payload):
            calls.append((mid, payload))
            if calls and len(calls) == 1:
                raise RuntimeError("模拟 stdout 写出失败")
            return None

        monkeypatch.setattr(worker_main, "emit_response", flaky_emit_response)

        async def failing_handler(params):
            raise WorkerError("selector_failed", "boom")

        worker_main.COMMANDS["test_emit_boom"] = failing_handler
        try:
            await worker_main._dispatch_guarded(
                {"id": 901, "method": "test_emit_boom", "params": {}}
            )
        finally:
            del worker_main.COMMANDS["test_emit_boom"]

        # 首发失败 + 兜底补发，同 id 恰好一个成功回包
        assert len(calls) == 2
        msg_id, payload = calls[1]
        assert msg_id == 901
        assert payload["success"] is False
        assert "boom" in payload["error"]

        # Worker 存活：下一条命令正常回包
        await worker_main._dispatch_guarded(
            {"id": 902, "method": "worker_health_check", "params": {}}
        )
        assert calls[-1][0] == 902
        assert calls[-1][1]["success"] is True

    _run(_run_case())


# ── 修复 4：_close_session / 热恢复清 _wired_page_ids ──


class _WirePage:
    """可统计 dialog 绑定次数的页面对象替身。"""

    def __init__(self):
        self.handlers: dict[str, object] = {}

    def on(self, event, cb):
        self.handlers.setdefault(event, []).append(cb)

    async def close(self):
        pass


class _WireContext:
    def __init__(self):
        self.pages: list = []

    async def new_page(self):
        return _WirePage()

    async def close(self):
        pass

    def on(self, event, cb):
        pass


def test_close_session_clears_wired_page_ids():
    """_close_session 关页后清空防重复绑定集合，新页不会被误判为已绑定。"""

    async def _run_case():
        core = WorkerCore()
        page = _WirePage()
        core._page = page
        core._context = _WireContext()
        core._browser = object()
        core._wired_page_ids.add(id(page))
        await core._close_session()
        assert core._wired_page_ids == set()

        # persistent 模式（context 保留）：同样要清，防止对象 id 复用漏绑新页
        core2 = WorkerCore()
        ctx = _WireContext()
        core2._context = ctx
        core2._browser = None
        core2._wired_page_ids.add(123456)
        await core2._close_session()
        assert core2._wired_page_ids == set()
        assert core2._context is ctx

    _run(_run_case())


def test_hot_recovery_clears_wired_page_ids_before_new_context():
    """热恢复重建 context 前清空 _wired_page_ids（旧页对象 id 可能被复用）。"""

    async def _run_case():
        core = WorkerCore()

        class FakeBrowser:
            def is_connected(self):
                return True

            async def new_context(self, **kwargs):
                return _WireContext()

        core._browser = FakeBrowser()
        core._context = None
        core._last_browser_settings = {"pure_mode": True}
        core._wired_page_ids.add(999999)
        await core.ensure_browser({"browser_settings": {"pure_mode": True}})
        assert 999999 not in core._wired_page_ids
        # 新页正常装上 dialog 处理器
        new_page = core._page
        assert new_page.handlers.get("dialog"), "热恢复新建页应绑定 dialog 处理器"

    _run(_run_case())


# ── 修复 5：close_browser 内部兜底先于命令级自愈 ──


def test_close_browser_internal_timeout_yields_to_command_budget(monkeypatch):
    """有 rust_timeout_ms 时内部兜底 = 0.9×预算 - 0.5s（8s 预算 → 6.7s < 7.2s 自愈）。"""

    async def _run_case():
        import playwright_worker as pw

        core = WorkerCore()
        real_wait_for = asyncio.wait_for
        timeouts: list[float] = []

        async def fake_wait_for(coro, timeout):
            timeouts.append(timeout)
            return await real_wait_for(coro, timeout)

        monkeypatch.setattr(pw.asyncio, "wait_for", fake_wait_for)
        released = []

        async def fake_session(self):
            released.append(True)

        orig_env = pw._WORKER_KEEP_ALIVE
        orig_session = WorkerCore._close_session
        pw._WORKER_KEEP_ALIVE = True
        WorkerCore._close_session = fake_session
        try:
            await core.handle_close_browser({"rust_timeout_ms": 8_000})
        finally:
            pw._WORKER_KEEP_ALIVE = orig_env
            WorkerCore._close_session = orig_session
        assert released == [True]
        assert timeouts == [pytest.approx(7.2 - 0.5)]

        # 无预算（旧主程序）：保持 8s 固定兜底
        timeouts.clear()
        released.clear()
        pw._WORKER_KEEP_ALIVE = True
        WorkerCore._close_session = fake_session
        try:
            await core.handle_close_browser({})
        finally:
            pw._WORKER_KEEP_ALIVE = orig_env
            WorkerCore._close_session = orig_session
        assert timeouts == [pw._WAIT_TIMEOUT_SECS]

    _run(_run_case())


# ── 修复 6：feedback_capture 取消注册 / 空闲释放 / detach finally ──


class _FeedbackCdp:
    def __init__(self, *, detach_error=False):
        self.detach_error = detach_error
        self.detach_calls = 0

    async def send(self, method, params=None):
        if method == "Page.enable":
            return {}
        if method == "Page.captureSnapshot":
            return {"data": "MIME-Version: 1.0\r\n"}
        if method == "Page.getResourceTree":
            return {"frameTree": {"frame": {"id": "F"}, "resources": []}}
        raise AssertionError(f"unexpected CDP method: {method}")

    async def detach(self):
        self.detach_calls += 1
        if self.detach_error:
            raise RuntimeError("detach failed")


class _FeedbackPage:
    """feedback_capture 所需的最小页面替身（content/screenshot/CDP/枚举）。"""

    def __init__(self, first_cdp=None):
        self.context = self
        self.cdps: list[_FeedbackCdp] = []
        self._pending_first = first_cdp

    def make_cdp(self, **kwargs):
        cdp = _FeedbackCdp(**kwargs)
        self.cdps.append(cdp)
        return cdp

    async def new_cdp_session(self, _page):
        # 首条会话可注入特殊替身（如 detach 必失败），其余正常新造
        if self._pending_first is not None:
            cdp = self._pending_first
            self._pending_first = None
            self.cdps.append(cdp)
            return cdp
        return self.make_cdp()

    async def evaluate(self, script, arg=None):
        assert script == _PAGE_RESOURCE_PROBE_JS
        return []

    async def content(self):
        return "<html><body>portal</body></html>"

    async def screenshot(self, full_page=True):
        return b"png-bytes"


def _feedback_core(monkeypatch, tmp_path: Path, page: _FeedbackPage) -> WorkerCore:
    core = WorkerCore()
    core._page = page
    core._last_browser_settings = {"browser_channel": "chromium"}
    monkeypatch.setattr(playwright_worker, "_debug_screenshot_dir", lambda: tmp_path)
    return core


def test_feedback_capture_detach_failure_does_not_leak(monkeypatch, tmp_path):
    """CDP detach 抛异常不能中断捕获，也不能泄漏 CDP 会话（detach 仍已执行）。"""

    async def _run_case():
        page = _FeedbackPage(first_cdp=_FeedbackCdp(detach_error=True))
        core = _feedback_core(monkeypatch, tmp_path, page)
        result = await core.handle_feedback_capture({})
        assert result["png_path"]
        assert result["mhtml_path"]
        mhtml_cdp = page.cdps[0]
        assert mhtml_cdp.detach_calls == 1, "MHTML 会话必须被 detach（即使 detach 抛异常）"
        assert all(c.detach_calls >= 1 for c in page.cdps), "其余 CDP 会话不得泄漏"
        # 产物写在 feedback-<stamp>/ 子目录内（_feedback_capture_dir 锚定 debug 目录）
        feedback_dirs = list(tmp_path.glob("feedback-*"))
        assert len(feedback_dirs) == 1
        assert (feedback_dirs[0] / "screenshot.png").exists()
        assert (feedback_dirs[0] / "page.mhtml").exists()

    _run(_run_case())


def test_feedback_capture_cancellable(monkeypatch, tmp_path):
    """注册 cancel_id：取消先于注册到达时立即以 StepCancelled 终止并注销。"""

    async def _run_case():
        page = _FeedbackPage()
        core = _feedback_core(monkeypatch, tmp_path, page)
        cancel_registry.trigger("fb-cancel")
        with pytest.raises(StepCancelled) as ei:
            await core.handle_feedback_capture({"cancel_id": "fb-cancel"})
        assert "已取消" in ei.value.message
        # finally 中注销：注册表不再残留
        assert "fb-cancel" not in cancel_registry._events
        assert "fb-cancel" not in cancel_registry._pending

    _run(_run_case())


# ── 修复 7：资源快照覆盖 iframe ──


class _TreeCdp:
    def __init__(self, tree, contents):
        self._tree = tree
        self._contents = contents
        self.frame_ids: list[str] = []
        self.detached = False

    async def send(self, method, params=None):
        if method == "Page.enable":
            return {}
        if method == "Page.getResourceTree":
            return self._tree
        if method == "Page.getResourceContent":
            self.frame_ids.append((params or {}).get("frameId", ""))
            url = (params or {}).get("url")
            if url not in self._contents:
                raise RuntimeError("cache evicted")
            return self._contents[url]
        raise AssertionError(f"unexpected CDP method: {method}")

    async def detach(self):
        self.detached = True


def test_iter_cdp_frame_resources_walks_child_frames():
    """两层 frameTree：递归收集主 frame 与子 frame 的 (frameId, resources)。"""
    tree = {
        "frameTree": {
            "frame": {"id": "F1"},
            "resources": [{"url": "https://a/main.css", "type": "Stylesheet"}],
            "childFrames": [
                {
                    "frame": {"id": "F2"},
                    "resources": [{"url": "https://a/child.js", "type": "Script"}],
                    "childFrames": [
                        {
                            "frame": {"id": "F3"},
                            "resources": [
                                {"url": "https://a/deep.css", "type": "Stylesheet"}
                            ],
                        }
                    ],
                }
            ],
        }
    }
    pairs = _iter_cdp_frame_resources(tree["frameTree"])
    assert [fid for fid, _ in pairs] == ["F1", "F2", "F3"]
    assert sum(len(entries) for _, entries in pairs) == 3


def test_cdp_resource_snapshot_covers_iframe_resources(tmp_path):
    """CDP 路径：iframe 门户的子 frame 资源也按各自 frameId 取回正文。"""
    main_css = "https://a.example/main.css"
    child_js = "https://a.example/child.js"
    tree = {
        "frameTree": {
            "frame": {"id": "F1"},
            "resources": [
                {"url": main_css, "type": "Stylesheet", "mimeType": "text/css"}
            ],
            "childFrames": [
                {
                    "frame": {"id": "F2"},
                    "resources": [
                        {
                            "url": child_js,
                            "type": "Script",
                            "mimeType": "application/javascript",
                        }
                    ],
                }
            ],
        }
    }
    contents = {
        main_css: {"content": "body{}", "base64Encoded": False},
        child_js: {"content": "console.log(1)", "base64Encoded": False},
    }
    cdp = _TreeCdp(tree, contents)

    class Page:
        def __init__(self):
            # context 即自身：_cdp_resource_snapshot 经 page.context.new_cdp_session 取会话
            self.context = self

        async def new_cdp_session(self, _page):
            return cdp

        async def evaluate(self, script, arg=None):
            # DOM 枚举为空：断言完全由 CDP 资源树驱动
            return []

    async def _run_case():
        with tempfile.TemporaryDirectory() as td:
            saved, _, note = await _capture_page_resources(
                Page(), Path(td) / "resources", bs={"browser_channel": "chromium"}
            )
            return saved, note, cdp

    saved, note, cdp = _run(_run_case())
    assert note is None
    assert set(saved) == {main_css, child_js}
    # 子 frame 的资源必须用其自身 frameId 请求正文
    assert cdp.frame_ids == ["F1", "F2"]


class _FrameDom:
    def __init__(self, dom):
        self._dom = dom

    async def evaluate(self, script, arg=None):
        assert script == _PAGE_RESOURCE_PROBE_JS, "枚举脚本契约变更，stub 需同步"
        return self._dom


class _FrameHttpResponse:
    def __init__(self, body=b"", content_type="text/css"):
        self._body = body
        self.status = 200
        self.ok = True
        self.headers = {"content-type": content_type}

    async def body(self):
        return self._body


class _FrameHttpRequest:
    def __init__(self, routes):
        self._routes = routes
        self.calls: list[str] = []

    async def get(self, url, timeout=None):
        self.calls.append(url)
        return self._routes[url]


class _FrameHttpPage:
    """多 frame 页面替身：逐 frame 枚举 + context.request 回补。"""

    def __init__(self, frames, routes):
        self.frames = frames
        self.context = self
        self.request = _FrameHttpRequest(routes)


def test_http_resource_snapshot_aggregates_frames(tmp_path):
    """HTTP 回补路径：逐 frame 枚举并按 URL 去重汇总，iframe 资源进离线副本。"""
    main_css = "https://a.example/main.css"
    child_js = "https://a.example/child.js"
    dup_css = "https://a.example/shared.css"
    page = _FrameHttpPage(
        frames=[
            _FrameDom([[main_css, "main.css", "stylesheet"]]),
            _FrameDom(
                [
                    [child_js, "child.js", "script"],
                    [dup_css, "shared.css", "stylesheet"],
                ]
            ),
            # 同一 URL 在多个 frame 出现：只保留首见，不重复回补
            _FrameDom([[dup_css, "shared.css", "stylesheet"]]),
        ],
        routes={
            main_css: _FrameHttpResponse(b"a{}"),
            child_js: _FrameHttpResponse(b"void 0", "application/javascript"),
            dup_css: _FrameHttpResponse(b"s{}"),
        },
    )

    async def _run_case():
        with tempfile.TemporaryDirectory() as td:
            saved, _, note = await _capture_page_resources(
                page, Path(td) / "resources", bs={"browser_channel": "firefox"}
            )
            return saved, note

    saved, note = _run(_run_case())
    # firefox 渠道固定带「不支持 CDP」说明；枚举/回补本身无额外降级
    assert note is not None and "不支持 CDP" in note
    assert set(saved) == {main_css, child_js, dup_css}
    # 去重：同一 URL 只回补一次
    assert sorted(page.request.calls) == sorted([main_css, child_js, dup_css])


def test_http_resource_snapshot_all_frames_failed_reports_error(tmp_path):
    """全部 frame 枚举失败 → 返回枚举失败说明（兜底整体不可用）。"""

    class Broken:
        async def evaluate(self, script, arg=None):
            raise RuntimeError("frame detached")

    class BrokenPage:
        frames = [Broken()]
        context = None
        request = _FrameHttpRequest({})

    async def _run_case():
        with tempfile.TemporaryDirectory() as td:
            return await _capture_page_resources(
                BrokenPage(), Path(td) / "resources", bs={"browser_channel": "firefox"}
            )

    saved, _, note = _run(_run_case())
    assert saved == {}
    assert note is not None
    assert "页面资源枚举失败" in note


# ── 修复 8：page_capture 分阶段取消检查 ──


def test_page_capture_cancelled_between_stages(monkeypatch, tmp_path):
    """截图阶段边界响应取消：捕获中途取消即终止并注销 cancel_id。"""

    async def _run_case():
        core = WorkerCore()
        core.ensure_browser = lambda config: _noop()
        core._prepare_session_page = _noop
        core._navigate = _noop
        class FakeContext:
            async def clear_cookies(self):
                pass

        class FakeCapturePage:
            def __init__(self):
                self.context = FakeContext()

            async def wait_for_load_state(self, state, timeout=None):
                pass

            async def content(self):
                # 捕获中途取消到达（模拟 Rust 在 HTML 落盘后发送 cancel）
                cancel_registry.trigger("pc-cancel")
                return "<html></html>"

            async def screenshot(self, full_page=True):
                return b"png"

        core._page = FakeCapturePage()
        monkeypatch.setattr(
            playwright_worker, "_capture_dir", lambda: tmp_path / "latest"
        )
        # finally 会武装空闲回收：置 keep_alive 使其成为 no-op，避免悬挂任务
        monkeypatch.setattr(playwright_worker, "_WORKER_KEEP_ALIVE", True)

        with pytest.raises(StepCancelled):
            await core.handle_page_capture({"url": "http://portal.example.com/", "cancel_id": "pc-cancel"})
        assert "pc-cancel" not in cancel_registry._events
        assert "pc-cancel" not in cancel_registry._pending

    async def _noop(*args, **kwargs):
        return None

    _run(_run_case())


# ── 修复 9：错误文本剥 URL token ──


def test_sanitize_error_strips_query_and_fragment():
    """query/fragment（含 token）被剥掉，非 URL 部分与 scheme+path 保留。"""
    text = "导航失败: net::ERR_TIMED_OUT at http://portal.example.com/srun?token=abc123&x=1#login"
    out = _sanitize_error(text)
    assert "token" not in out
    assert "abc123" not in out
    assert "#login" not in out
    assert out.startswith("导航失败: net::ERR_TIMED_OUT at http://portal.example.com/srun")
    # 无 query/fragment 的 URL 原样保留
    plain = "see https://a.example/path/x.js for details"
    assert _sanitize_error(plain) == plain
    # 空/无 URL 文本原样返回
    assert _sanitize_error("") == ""
    assert _sanitize_error("纯文本错误") == "纯文本错误"


def test_classify_navigation_error_sanitizes_url():
    """导航错误出口：异常文本与目标 URL 的 token 都被剥掉。"""
    from step_handlers import _classify_navigation_error

    token_url = "http://portal.example.com/srun?token=secret#ok"
    exc = RuntimeError(f"Navigation failed to {token_url}: net::ERR_CONNECTION_RESET")
    err = _classify_navigation_error(exc, token_url)
    assert err.outcome == "network_error"
    assert "secret" not in err.message
    assert "token" not in err.message
    # 连接错误模式判定不受剥洗影响
    assert "ERR_CONNECTION_RESET" in err.message


def test_safe_op_sanitizes_timeout_message():
    """_safe_op 出口：Playwright 超时异常里的 token URL 被剥掉。"""
    from playwright.async_api import TimeoutError as PlaywrightTimeoutError

    async def _run_case():
        async def boom():
            raise PlaywrightTimeoutError(
                "Timeout 1000ms exceeded. http://portal.example.com/?token=tok1"
            )

        with pytest.raises(WorkerError) as ei:
            await _safe_op(boom(), "selector_failed")
        assert ei.value.outcome == "selector_failed"
        assert "tok1" not in ei.value.message
        assert "http://portal.example.com/" in ei.value.message

    _run(_run_case())


def test_error_result_and_structured_result_sanitize():
    """worker_main 两个响应出口：错误消息与 error 字段均剥 token。"""
    message = "未捕获异常 at http://portal.example.com/srun?token=leak&x=1"
    err = worker_main._error_result(message)
    assert "leak" not in json.dumps(err, ensure_ascii=False)
    assert "http://portal.example.com/srun" in err["error"]

    exc = WorkerError("unknown_error", message)
    resp = worker_main._structured_result(exc, success=False)
    dumped = json.dumps(resp, ensure_ascii=False)
    assert "leak" not in dumped
    assert "http://portal.example.com/srun" in resp["error"]
