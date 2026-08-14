"""变量模板解析测试：variable_resolver.resolve 的替换、链式递归与循环保护。"""

from __future__ import annotations

from variable_resolver import resolve


def test_no_placeholder_unchanged():
    assert resolve("hello", {"A": "1"}) == "hello"
    assert resolve("没有占位符", {}) == "没有占位符"


def test_simple_substitution():
    assert resolve("{{USERNAME}}", {"USERNAME": "admin"}) == "admin"
    assert resolve("user={{USERNAME}}&pwd={{PASSWORD}}", {"USERNAME": "u", "PASSWORD": "p"}) == "user=u&pwd=p"


def test_whitespace_in_placeholder():
    assert resolve("{{ USERNAME }}", {"USERNAME": "admin"}) == "admin"


def test_missing_variable_keeps_placeholder():
    assert resolve("{{UNKNOWN}}", {}) == "{{UNKNOWN}}"


def test_chained_reference():
    # username -> {{USERNAME}} -> admin
    variables = {"username": "{{USERNAME}}", "USERNAME": "admin"}
    assert resolve("{{username}}", variables) == "admin"


def test_circular_reference_preserved():
    # a -> b -> a，循环引用应保留原占位符而非无限递归
    variables = {"a": "{{b}}", "b": "{{a}}"}
    assert resolve("{{a}}", variables) == "{{a}}"


def test_depth_limit_safe():
    # 超深链式引用不抛异常，返回模板
    variables = {f"k{i}": f"{{{{k{i+1}}}}}" for i in range(0, 20)}
    variables["k20"] = "end"
    out = resolve("{{k0}}", variables)
    assert isinstance(out, str)


def test_non_string_input_returns_as_is():
    assert resolve(123, {}) == 123
    assert resolve(None, {}) is None