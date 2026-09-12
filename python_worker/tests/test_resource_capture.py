"""_capture_page_resources 逐项容错：类型过滤 / 超限跳过 / base64 解码。

以 stub CDP 会话替代真实浏览器，覆盖资源快照的过滤与落盘语义：
非法类型与超限内容跳过、可取回的 script/stylesheet 落盘并建映射。
"""

from __future__ import annotations

import asyncio
import base64
import tempfile
from pathlib import Path

from playwright_worker import (
    _RESOURCE_MAX_BYTES,
    _capture_page_resources,
    WorkerCore,
)


def _run(coro):
    return asyncio.run(coro)


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


class _FakePage:
    def __init__(self, cdp):
        self.context = self
        self._cdp = cdp

    async def new_cdp_session(self, _page):
        return self._cdp


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
            return await _capture_page_resources(_FakePage(cdp), target)

        saved, note = _run(_run_capture())
        assert note is None
        assert set(saved) == {js_url, css_url}
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
                _FakePage(_FakeCdp(tree, contents)), tmp_path / "resources"
            )

        saved, _ = _run(_run_capture())
        assert set(saved) == {small_url}


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
