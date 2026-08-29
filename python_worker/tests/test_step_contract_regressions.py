"""浏览器任务步骤契约回归测试。

这些测试不启动真实浏览器，专门覆盖录制器/任务指南与 Python Worker 之间曾经断链的
行为：运行时变量、复杂 CSS 候选、wait/select/click_select、frame 与任务顶层字段。
"""

from __future__ import annotations

import asyncio

import pytest

from models import StepConfig, TaskConfig
from step_handlers import (
    StepContext,
    WorkerError,
    _choose_text_index,
    _frame_scope,
    _resolve,
    _split_selector_candidates,
    handle_click_select,
    handle_select,
    handle_wait,
    handle_wait_url,
)


def test_task_config_keeps_rust_timeout_fields():
    task = TaskConfig.from_dict(
        {
            "name": "demo",
            "timeout": 45678,
            "navigation_wait": 2.5,
            "steps": [],
        }
    )
    assert task.timeout == 45678
    assert task.navigation_wait == 2.5
    assert task.step_delay == 0.5
    assert "timeout" not in task.extras
    assert "navigation_wait" not in task.extras


def test_resolve_uses_runtime_results_and_all_selector_fields():
    raw = StepConfig.from_dict(
        {
            "id": "s1",
            "type": "goto",
            "description": "打开 {{PAGE}}",
            "selector": "#{{FIELD}}",
            "value": "{{RUNTIME}}",
            "pattern": "{{PATTERN}}",
            "path": "{{FILE}}",
            "frame": "{{FRAME}}",
            "option_selector": ".{{OPTION}}",
            "target_selector": "#{{TARGET}}",
            "script": "() => '{{RUNTIME}}'",
            "url": "https://example.test/{{RUNTIME}}",
            "nested": {"value": "{{RUNTIME}}"},
        }
    )
    ctx = StepContext(
        page=None,
        variables={
            "PAGE": "登录页",
            "FIELD": "user",
            "RUNTIME": "static",
            "PATTERN": "success",
            "FILE": "a.txt",
            "FRAME": "main",
            "OPTION": "option",
            "TARGET": "captcha",
        },
        results={"RUNTIME": "dynamic"},
    )

    resolved = _resolve(raw, ctx)

    assert resolved is not raw
    assert resolved.description == "打开 登录页"
    assert resolved.selector == "#user"
    assert resolved.value == "dynamic"
    assert resolved.pattern == "success"
    assert resolved.path == "a.txt"
    assert resolved.frame == "main"
    assert resolved.option_selector == ".option"
    assert resolved.target_selector == "#captcha"
    assert resolved.effective_script == "() => 'dynamic'"
    assert resolved.extra_fields["url"] == "https://example.test/dynamic"
    assert resolved.extra_fields["nested"]["value"] == "dynamic"
    # 原步骤不能被调试重跑时的模板解析污染
    assert raw.value == "{{RUNTIME}}"
    assert raw.extra_fields["url"] == "https://example.test/{{RUNTIME}}"


def test_runtime_result_none_resolves_to_empty_string():
    raw = StepConfig.from_dict({"id": "s", "type": "input", "value": "{{VALUE}}"})
    resolved = _resolve(raw, StepContext(page=None, results={"VALUE": None}))
    assert resolved.value == ""


def test_split_selector_candidates_keeps_nested_commas():
    selector = ":is(.a,.b), [data-x='a,b'], button:not(.x,.y), #final"
    assert _split_selector_candidates(selector) == [
        ":is(.a,.b)",
        "[data-x='a,b']",
        "button:not(.x,.y)",
        "#final",
    ]


def test_choose_text_index_prefers_unique_exact_then_unique_substring():
    assert _choose_text_index(["校园网", "中国电信", "中国移动"], "中国电信") == 1
    assert _choose_text_index(["校园网-中国电信", "中国移动"], "电信") == 0
    assert _choose_text_index(["电信 A", "电信 B"], "电信") is None


class _FakeWaitLocator:
    def __init__(self) -> None:
        self.waits: list[tuple[str | None, int | None]] = []

    @property
    def first(self):
        return self

    async def wait_for(self, state=None, timeout=None):
        self.waits.append((state, timeout))


class _FakeWaitPage:
    def __init__(self) -> None:
        self.target = _FakeWaitLocator()

    def locator(self, _selector):
        return self.target


def test_wait_with_selector_waits_for_visible_element():
    page = _FakeWaitPage()
    step = StepConfig.from_dict(
        {"id": "w", "type": "wait", "selector": "#ready", "timeout": 321}
    )
    asyncio.run(handle_wait(page, step, StepContext(page=page)))
    assert page.target.waits == [("visible", 321)]


def test_wait_without_selector_keeps_legacy_sleep_semantics():
    step = StepConfig.from_dict({"id": "w", "type": "wait", "duration": 0})
    asyncio.run(handle_wait(None, step, StepContext(page=None)))


class _FakeOptions:
    def __init__(self, items):
        self.items = items

    async def evaluate_all(self, _script):
        return self.items


class _FakeSelectLocator:
    def __init__(self, items):
        self.items = items
        self.selected: list[str] = []

    @property
    def first(self):
        return self

    async def wait_for(self, state=None, timeout=None):
        return None

    def locator(self, selector):
        assert selector == "option"
        return _FakeOptions(self.items)

    async def select_option(self, value=None, timeout=None):
        self.selected.append(value)


class _FakeSelectPage:
    def __init__(self, items):
        self.select = _FakeSelectLocator(items)

    def locator(self, _selector):
        return self.select


def test_select_matches_unique_substring_by_option_text():
    page = _FakeSelectPage(
        [
            {"value": "", "text": "请选择"},
            {"value": "telecom", "text": "中国电信校园网"},
            {"value": "mobile", "text": "中国移动"},
        ]
    )
    step = StepConfig.from_dict(
        {"id": "isp", "type": "select", "selector": "#isp", "value": "电信"}
    )
    asyncio.run(handle_select(page, step, StepContext(page=page)))
    assert page.select.selected == ["telecom"]


def test_optional_select_skips_ambiguous_match():
    page = _FakeSelectPage(
        [
            {"value": "a", "text": "电信 A"},
            {"value": "b", "text": "电信 B"},
        ]
    )
    step = StepConfig.from_dict(
        {
            "id": "isp",
            "type": "select",
            "selector": "#isp",
            "value": "电信",
            "required": False,
        }
    )
    asyncio.run(handle_select(page, step, StepContext(page=page)))
    assert page.select.selected == []


def test_required_select_fails_ambiguous_match():
    page = _FakeSelectPage(
        [
            {"value": "a", "text": "电信 A"},
            {"value": "b", "text": "电信 B"},
        ]
    )
    step = StepConfig.from_dict(
        {"id": "isp", "type": "select", "selector": "#isp", "value": "电信"}
    )
    with pytest.raises(WorkerError):
        asyncio.run(handle_select(page, step, StepContext(page=page)))


class _FakeClickTarget:
    def __init__(self) -> None:
        self.click_count = 0

    @property
    def first(self):
        return self

    async def click(self, timeout=None):
        self.click_count += 1

    async def wait_for(self, state=None, timeout=None):
        return None

    async def dispatch_event(self, _event):
        self.click_count += 1


class _FakeOptionLocator:
    def __init__(self, texts):
        self.texts = texts
        self.targets = [_FakeClickTarget() for _ in texts]

    async def count(self):
        return len(self.texts)

    async def all_inner_texts(self):
        return self.texts

    def nth(self, idx):
        return self.targets[idx]


class _FakeClickSelectPage:
    def __init__(self) -> None:
        self.trigger = _FakeClickTarget()
        self.options = _FakeOptionLocator(["中国移动", "中国电信", "中国联通"])
        self.frames = []

    def locator(self, selector):
        if selector == "#trigger":
            return self.trigger
        if selector == ".option":
            return self.options
        raise AssertionError(selector)


def test_click_select_uses_value_inside_option_selector_scope():
    page = _FakeClickSelectPage()
    step = StepConfig.from_dict(
        {
            "id": "isp",
            "type": "click_select",
            "selector": "#trigger",
            "option_selector": ".option",
            "value": "电信",
            "select_delay": 0,
            "timeout": 1000,
        }
    )
    asyncio.run(handle_click_select(page, step, StepContext(page=page)))
    assert page.trigger.click_count == 1
    assert [item.click_count for item in page.options.targets] == [0, 1, 0]


class _FakeFrame:
    def __init__(self, name: str, url: str) -> None:
        self.name = name
        self.url = url


class _FakeFramePage:
    def __init__(self) -> None:
        self.main = _FakeFrame("mainFrame", "https://portal.test/login")
        self.frames = [self.main]
        self.frame_locator_calls: list[str] = []

    def frame_locator(self, selector):
        self.frame_locator_calls.append(selector)
        return ("css-frame", selector)


def test_frame_scope_supports_name_url_and_css():
    page = _FakeFramePage()
    ctx = StepContext(page=page, frame="mainFrame")
    assert _frame_scope(ctx) is page.main

    ctx.frame = "url=/login"
    assert _frame_scope(ctx) is page.main

    ctx.frame = "#login-frame"
    assert _frame_scope(ctx) == ("css-frame", "#login-frame")
    assert page.frame_locator_calls == ["#login-frame"]


def test_wait_url_rejects_invalid_regex_as_config_error():
    class Page:
        url = "https://example.test"

    step = StepConfig.from_dict({"id": "u", "type": "wait_url", "pattern": "["})
    with pytest.raises(WorkerError) as exc:
        asyncio.run(handle_wait_url(Page(), step, StepContext(page=Page())))
    assert exc.value.outcome == "unknown_error"
    assert "正则非法" in exc.value.message
