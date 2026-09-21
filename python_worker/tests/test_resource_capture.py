"""_capture_page_resources 双路径语义：CDP 过滤/容错 + 引擎无关 HTTP 回补。

以 stub CDP 会话与 stub APIRequestContext 替代真实浏览器，覆盖：
非法类型与超限内容跳过、缓存逐出项经 HTTP 回补补齐、非 Chromium 渠道不触碰
CDP 并给出可操作说明、回补失败逐项跳过不中断整体。
"""

from __future__ import annotations

import asyncio
import base64
import tempfile
from pathlib import Path

from playwright_worker import (
    _PAGE_RESOURCE_PROBE_JS,
    _RESOURCE_MAX_BYTES,
    _capture_page_resources,
    _channel_supports_cdp,
    WorkerCore,
)


def _run(coro):
    return asyncio.run(coro)


def _chromium():
    return {"browser_channel": "chromium"}


class _FakeCdp:
    """最小 CDP 会话 stub：按构造表应答三种 Page 方法。"""

    def __init__(self, tree, contents):
        self._tree = tree
        self._contents = contents
        self.detached = False

    async def send(self, method, params=None):
        if method == "Page.enable":
            return {}
        if method == "Page.getResourceTree":
            return self._tree
        if method == "Page.getResourceContent":
            url = (params or {}).get("url")
            if url not in self._contents:
                raise RuntimeError("cache evicted")
            return self._contents[url]
        raise AssertionError(f"unexpected CDP method: {method}")

    async def detach(self):
        self.detached = True


class _FakeResponse:
    """stub APIResponse：仅回补路径用到的 ok/status/headers/body。"""

    def __init__(self, body=b"", status=200, content_type="text/css"):
        self._body = body
        self.status = status
        self.ok = 200 <= status < 300
        self.headers = {"content-type": content_type}

    async def body(self):
        return self._body


class _FakeRequest:
    """stub APIRequestContext：按 URL 表返回响应，表里是异常则抛出。"""

    def __init__(self, routes=None):
        self._routes = routes or {}
        self.calls: list[str] = []

    async def get(self, url, timeout=None):
        self.calls.append(url)
        result = self._routes.get(url)
        if isinstance(result, BaseException):
            raise result
        if result is None:
            return _FakeResponse(status=404)
        return result


class _FakePage:
    """stub 页面：同时充当 context，提供 CDP 会话、evaluate 与 request。"""

    def __init__(self, cdp=None, *, dom=None, routes=None, cdp_forbidden=False):
        self.context = self
        self._cdp = cdp
        self._dom = dom if dom is not None else []
        self._cdp_forbidden = cdp_forbidden
        self.request = _FakeRequest(routes)

    async def new_cdp_session(self, _page):
        if self._cdp_forbidden:
            raise AssertionError("非 Chromium 渠道不应尝试 CDP")
        return self._cdp

    async def evaluate(self, script, arg=None):
        assert script == _PAGE_RESOURCE_PROBE_JS, "枚举脚本契约变更，stub 需同步"
        return self._dom


class _FakeMhtmlCdp:
    """MHTML 捕获 stub，可独立模拟抓取或释放失败。"""

    def __init__(self, *, send_error=False, detach_error=False):
        self.send_error = send_error
        self.detach_error = detach_error
        self.detached = False

    async def send(self, method, params=None):
        assert method == "Page.captureSnapshot"
        assert params == {"format": "mhtml"}
        if self.send_error:
            raise RuntimeError("capture failed")
        return {"data": "MIME-Version: 1.0\r\n"}

    async def detach(self):
        self.detached = True
        if self.detach_error:
            raise RuntimeError("detach failed")


def _tree(resources):
    return {"frameTree": {"frame": {"id": "F"}, "resources": resources}}


def _res(url, rtype, mime=""):
    return {"url": url, "type": rtype, "mimeType": mime}


def test_channel_supports_cdp():
    """CDP 可用性口径：仅 Chromium 系为真，custom 按所配引擎判定。"""
    assert _channel_supports_cdp(None) is True
    assert _channel_supports_cdp({}) is True
    assert _channel_supports_cdp({"browser_channel": "playwright"}) is True
    assert _channel_supports_cdp({"browser_channel": "chromium"}) is True
    assert _channel_supports_cdp({"browser_channel": "msedge"}) is True
    assert _channel_supports_cdp({"browser_channel": "chrome"}) is True
    assert _channel_supports_cdp({"browser_channel": "firefox"}) is False
    assert _channel_supports_cdp({"browser_channel": "webkit"}) is False
    assert (
        _channel_supports_cdp(
            {"browser_channel": "custom", "custom_browser_engine": "firefox"}
        )
        is False
    )
    assert (
        _channel_supports_cdp(
            {"browser_channel": "custom", "custom_browser_engine": "chromium"}
        )
        is True
    )


def test_filters_types_and_evicted_entries():
    """仅 script/stylesheet 落盘；图片/data 链接与缓存逐出项跳过。"""
    with tempfile.TemporaryDirectory() as td:
        tmp_path = Path(td)
        js_url = "https://cdn.example.com/app.js"
        css_url = "https://cdn.example.com/app.css"
        tree = _tree(
            [
                _res(js_url, "Script", "application/javascript"),
                _res(css_url, "Stylesheet", "text/css"),
                _res("https://cdn.example.com/logo.png", "Image", "image/png"),
                _res("data:text/plain,hi", "Script", "text/plain"),
                _res("https://cdn.example.com/gone.js", "Script", "text/javascript"),
            ]
        )
        css_raw = "body{color:red}".encode("utf-8")
        contents = {
            js_url: {"content": "console.log(1)", "base64Encoded": False},
            css_url: {
                "content": base64.b64encode(css_raw).decode(),
                "base64Encoded": True,
            },
        }
        cdp = _FakeCdp(tree, contents)
        target = tmp_path / "resources"

        async def _run_capture():
            return await _capture_page_resources(
                _FakePage(cdp), target, bs=_chromium()
            )

        saved, aliases, note = _run(_run_capture())
        assert note is None
        assert set(saved) == {js_url, css_url}
        # CDP 只给绝对 URL：没有原始书写形态可作别名
        assert aliases == {}
        assert cdp.detached is True
        for url, rel in saved.items():
            assert rel.startswith("resources/")
            data = (tmp_path / rel).read_bytes()
            assert data == (css_raw if url == css_url else b"console.log(1)")


def test_oversized_content_skipped():
    """超单文件上限的资源跳过，不建映射、不落盘。"""
    with tempfile.TemporaryDirectory() as td:
        tmp_path = Path(td)
        big_url = "https://cdn.example.com/big.js"
        small_url = "https://cdn.example.com/small.js"
        tree = _tree(
            [
                _res(big_url, "Script", "text/javascript"),
                _res(small_url, "Script", "text/javascript"),
            ]
        )
        contents = {
            big_url: {
                "content": "x" * (_RESOURCE_MAX_BYTES + 1),
                "base64Encoded": False,
            },
            small_url: {"content": "ok", "base64Encoded": False},
        }

        async def _run_capture():
            return await _capture_page_resources(
                _FakePage(_FakeCdp(tree, contents)), tmp_path / "resources", bs=_chromium()
            )

        saved, _, _ = _run(_run_capture())
        assert set(saved) == {small_url}


def test_evicted_resource_supplemented_via_http():
    """CDP 内存缓存已逐出的资源由 HTTP 回补补齐（Chromium 下的补缺路径）。"""
    with tempfile.TemporaryDirectory() as td:
        tmp_path = Path(td)
        evicted = "https://cdn.example.com/evicted.js"
        tree = _tree([_res(evicted, "Script", "application/javascript")])
        page = _FakePage(
            _FakeCdp(tree, {}),
            dom=[[evicted, "/js/evicted.js", "script"]],
            routes={
                evicted: _FakeResponse(
                    b"console.log('refetched')",
                    content_type="application/javascript",
                )
            },
        )

        async def _run_capture():
            return await _capture_page_resources(page, tmp_path / "resources", bs=_chromium())

        saved, aliases, note = _run(_run_capture())
        assert note is None
        assert set(saved) == {evicted}
        assert (tmp_path / saved[evicted]).read_bytes() == b"console.log('refetched')"
        assert page.request.calls == [evicted]
        # HTML 里的相对写法必须同步成别名，否则离线副本改不动引用
        assert aliases == {"/js/evicted.js": saved[evicted]}


def test_cdp_saved_resource_still_learns_relative_alias():
    """CDP 已落盘的资源不再回补，但其相对写法仍要在枚举阶段登记为别名。"""
    with tempfile.TemporaryDirectory() as td:
        tmp_path = Path(td)
        css_url = "https://cdn.example.com/static/css/app.css"
        tree = _tree([_res(css_url, "Stylesheet", "text/css")])
        page = _FakePage(
            _FakeCdp(tree, {css_url: {"content": "a{}", "base64Encoded": False}}),
            dom=[[css_url, "static/css/app.css", "stylesheet"]],
        )

        async def _run_capture():
            return await _capture_page_resources(page, tmp_path / "resources", bs=_chromium())

        saved, aliases, note = _run(_run_capture())
        assert note is None
        assert aliases == {"static/css/app.css": saved[css_url]}
        assert page.request.calls == []


def test_firefox_skips_cdp_and_reports_actionable_note():
    """非 Chromium 渠道不触碰 CDP，改用 HTTP 回补并给出可操作说明。"""
    with tempfile.TemporaryDirectory() as td:
        tmp_path = Path(td)
        css_url = "https://cdn.example.com/static/css/app.css"
        js_url = "https://cdn.example.com/static/js/app.js"
        page = _FakePage(
            dom=[
                [css_url, "static/css/app.css", "stylesheet"],
                [js_url, "static/js/app.js", "script"],
                ["data:text/javascript,1", "x.js", "script"],
                [css_url, "static/css/app.css", "stylesheet"],
            ],
            routes={
                css_url: _FakeResponse(b"body{margin:0}", content_type="text/css"),
                js_url: _FakeResponse(b"void 0", content_type="application/javascript"),
            },
            cdp_forbidden=True,
        )

        async def _run_capture():
            return await _capture_page_resources(
                page, tmp_path / "resources", bs={"browser_channel": "firefox"}
            )

        saved, aliases, note = _run(_run_capture())
        assert set(saved) == {css_url, js_url}
        assert aliases == {
            "static/css/app.css": saved[css_url],
            "static/js/app.js": saved[js_url],
        }
        assert note is not None
        assert "firefox" in note
        assert "不支持 CDP" in note
        assert "Chromium / Chrome / Edge" in note
        # 去重后同一 URL 只请求一次
        assert page.request.calls == [css_url, js_url]
        for url, rel in saved.items():
            assert (tmp_path / rel).read_bytes()


def test_fallback_failures_are_reported_without_dropping_succeeded_ones():
    """回补逐项容错：失败的资源计数上报，成功的照常落盘。"""
    with tempfile.TemporaryDirectory() as td:
        tmp_path = Path(td)
        ok_url = "https://cdn.example.com/ok.css"
        missing_url = "https://cdn.example.com/missing.js"
        broken_url = "https://cdn.example.com/broken.js"
        page = _FakePage(
            dom=[
                [ok_url, "ok.css", "stylesheet"],
                [missing_url, "missing.js", "script"],
                [broken_url, "broken.js", "script"],
            ],
            routes={
                ok_url: _FakeResponse(b"a{}", content_type="text/css"),
                broken_url: RuntimeError("connection reset"),
            },
            cdp_forbidden=True,
        )

        async def _run_capture():
            return await _capture_page_resources(
                page, tmp_path / "resources", bs={"browser_channel": "webkit"}
            )

        saved, _, note = _run(_run_capture())
        assert set(saved) == {ok_url}
        assert note is not None
        assert "1 个资源经 HTTP 回补成功" in note
        assert "2 个失败" in note
        assert "HTTP 404" in note


def test_probe_failure_does_not_break_capture():
    """页面枚举本身失败时返回说明，而不是把异常抛给调用方。"""

    class _BrokenProbePage(_FakePage):
        async def evaluate(self, script, arg=None):
            raise RuntimeError("frame detached")

    with tempfile.TemporaryDirectory() as td:
        tmp_path = Path(td)
        page = _BrokenProbePage(cdp_forbidden=True)

        async def _run_capture():
            return await _capture_page_resources(
                page, tmp_path / "resources", bs={"browser_channel": "firefox"}
            )

        saved, aliases, note = _run(_run_capture())
        assert saved == {}
        assert aliases == {}
        assert note is not None
        assert "页面资源枚举失败" in note


def test_mhtml_capture_detaches_and_keeps_success_when_detach_fails():
    """已成功写盘后，即使 CDP detach 失败也不能把捕获结果改成失败。"""
    with tempfile.TemporaryDirectory() as td:
        target = Path(td) / "page.mhtml"
        cdp = _FakeMhtmlCdp(detach_error=True)

        captured = _run(WorkerCore._capture_mhtml(_FakePage(cdp), target))

        assert captured is True
        assert cdp.detached is True
        assert target.read_text(encoding="utf-8") == "MIME-Version: 1.0\n"


def test_mhtml_capture_failure_still_detaches():
    """抓取失败也必须释放已创建的 CDP 会话。"""
    with tempfile.TemporaryDirectory() as td:
        target = Path(td) / "page.mhtml"
        cdp = _FakeMhtmlCdp(send_error=True)

        captured = _run(WorkerCore._capture_mhtml(_FakePage(cdp), target))

        assert captured is False
        assert cdp.detached is True
        assert not target.exists()
