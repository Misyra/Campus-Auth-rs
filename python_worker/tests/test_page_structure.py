"""页面结构化摘要的 frame 层级与局部 HTML 总预算回归测试。"""

from __future__ import annotations

import asyncio

from playwright_worker import _STRUCTURE_MAX_LOCAL_HTML, _capture_page_structure


class _FakeFrame:
    """只实现结构提取需要的 Frame 接口。"""

    def __init__(self, name: str, url: str, parent=None, html: str = ""):
        self.name = name
        self.url = url
        self.parent_frame = parent
        self._html = html

    async def evaluate(self, _script, limits):
        assert limits["controls"] > 0
        return {
            "forms": [{"action": "/login"}],
            "controls": [{"tag": "input", "name": "username"}],
            "captcha_candidates": [],
            "has_shadow_dom": False,
            "local_html": self._html,
        }


class _FakePage:
    def __init__(self):
        main = _FakeFrame("", "https://portal.test", html="A" * 30_000)
        child = _FakeFrame("login", "https://portal.test/frame", main, "B" * 30_000)
        self.frames = [main, child]
        self.main_frame = main


def test_capture_page_structure_keeps_hierarchy_and_global_html_budget():
    result = asyncio.run(_capture_page_structure(_FakePage()))
    assert result["frames"][0]["ref"] == "main"
    assert result["frames"][1]["parent"] == "main"
    assert result["frames"][1]["ref"] == "login"
    total = sum(len(frame["local_html"]) for frame in result["frames"])
    assert total == _STRUCTURE_MAX_LOCAL_HTML
