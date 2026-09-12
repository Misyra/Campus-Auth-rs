"""模板变量解析。

将步骤配置中的 ``{{VAR}}`` 占位符替换为实际值。变量来源为 Profile 字段
（USERNAME / PASSWORD / ISP / LOGIN_URL 等）与任务自定义的 ``variables``。

支持链式引用（递归解析）：任务 ``variables`` 中可定义
``{"username": "{{USERNAME}}"}``，``USERNAME`` 再由外部凭证映射提供实际值。
解析时遇到替换结果仍含 ``{{...}}`` 会继续展开，并检测循环引用与深度限制。
"""

from __future__ import annotations

import json
import logging
import re
from typing import Any

logger = logging.getLogger(__name__)

# {{VAR}} 占位符匹配：变量名仅允许字母/数字/下划线
_VAR_PATTERN = re.compile(r"\{\{\s*([A-Za-z_][A-Za-z0-9_]*)\s*\}\}")

# 递归展开最大深度，防止恶意/误配置导致无限递归
_MAX_DEPTH = 8


def resolve(
    template: str,
    variables: dict[str, str],
    _depth: int = 0,
    _visited: set[str] | None = None,
) -> str:
    """将字符串模板中的 ``{{KEY}}`` 替换为变量值（支持链式递归）。

    参数:
        template: 含占位符的模板字符串。
        variables: 变量名 → 值的映射。值本身可能含 ``{{...}}``，将递归展开。
        _depth: 内部使用的当前递归深度。
        _visited: 内部使用的已访问变量名集合，用于检测循环引用。

    返回:
        替换后的字符串。未匹配的占位符、循环引用、超深度均保持原样（不报错）。
    """
    if not template or not isinstance(template, str):
        return template
    if "{{" not in template:
        return template
    if _depth > _MAX_DEPTH:
        logger.debug("变量解析超过最大深度 %d，保留原样: %.100s", _MAX_DEPTH, template)
        return template
    visited = _visited or set()

    def _sub(match: re.Match[str]) -> str:
        key = match.group(1)
        if key in visited:
            # 循环引用：保留原占位符，避免无限递归
            logger.debug("变量循环引用，保留原占位符: %s", key)
            return match.group(0)
        val = variables.get(key)
        if val is None:
            # 未找到：保留原占位符
            logger.debug("变量未命中，保留原占位符: %s", key)
            return match.group(0)
        # 值中若仍含 {{...}}，递归展开（如 username -> {{USERNAME}} -> admin）
        if isinstance(val, str) and "{{" in val:
            return resolve(val, variables, _depth + 1, visited | {key})
        return str(val)

    return _VAR_PATTERN.sub(_sub, template)


_REGEX_PREFIX_TOKENS = {
    None,
    "(", "[", "{", ",", ":", ";", "=", "!", "?", "&", "|",
    "+", "-", "*", "%", "^", "~", "<", ">",
    "return", "throw", "case", "delete", "void", "typeof", "new", "in",
    "of", "yield", "await", "else", "do",
}


def _javascript_context_at(source: str, offset: int) -> str:
    """词法扫描到 offset，返回 code/string/template/comment/regex 上下文。

    这里只判定占位符所处语法区域，不尝试构建完整 AST；仍需识别注释、正则和
    模板字符串表达式，否则注释中的引号会把后续代码误判为字符串，重新打开注入
    或脚本损坏窗口。
    """
    frames: list[dict[str, Any]] = [{"mode": "code", "depth": None, "token": None}]
    index = 0
    while index < offset:
        frame = frames[-1]
        mode = frame["mode"]
        char = source[index]
        following = source[index + 1] if index + 1 < offset else ""

        if mode in {"single", "double"}:
            if char == "\\":
                index += 2
                continue
            expected = "'" if mode == "single" else '"'
            if char == expected:
                frames.pop()
                frames[-1]["token"] = "value"
            index += 1
            continue

        if mode == "template":
            if char == "\\":
                index += 2
                continue
            if char == "`":
                frames.pop()
                frames[-1]["token"] = "value"
                index += 1
                continue
            if char == "$" and following == "{":
                frames.append({"mode": "code", "depth": 1, "token": None})
                index += 2
                continue
            index += 1
            continue

        if mode == "line_comment":
            if char in "\r\n":
                frames.pop()
            index += 1
            continue

        if mode == "block_comment":
            if char == "*" and following == "/":
                frames.pop()
                index += 2
            else:
                index += 1
            continue

        if mode == "regex":
            if char == "\\":
                index += 2
                continue
            if char == "[":
                frame["char_class"] = True
            elif char == "]":
                frame["char_class"] = False
            elif char == "/" and not frame.get("char_class", False):
                frames.pop()
                frames[-1]["token"] = "value"
            index += 1
            continue

        # code（顶层或 `${...}` 表达式）
        if char.isspace():
            index += 1
            continue
        if char == "/" and following == "/":
            frames.append({"mode": "line_comment"})
            index += 2
            continue
        if char == "/" and following == "*":
            frames.append({"mode": "block_comment"})
            index += 2
            continue
        if char == "/" and frame.get("token") in _REGEX_PREFIX_TOKENS:
            frames.append({"mode": "regex", "char_class": False})
            index += 1
            continue
        if char in ("'", '"', "`"):
            mode_name = {"'": "single", '"': "double", "`": "template"}[char]
            frames.append({"mode": mode_name})
            index += 1
            continue
        if frame.get("depth") is not None:
            if char == "{":
                frame["depth"] += 1
            elif char == "}":
                frame["depth"] -= 1
                if frame["depth"] == 0:
                    frames.pop()
                    index += 1
                    continue
        if char.isalpha() or char in "_$":
            end = index + 1
            while end < offset and (source[end].isalnum() or source[end] in "_$"):
                end += 1
            frame["token"] = source[index:end]
            index = end
            continue
        frame["token"] = char if char not in ")]" else "value"
        index += 1

    mode = frames[-1]["mode"]
    return mode


def _escape_javascript_string(value: str, quote: str) -> str:
    """把变量值编码为当前 JavaScript 字符串字面量中的安全内容。"""
    escaped = value.replace("\\", "\\\\")
    escaped = escaped.replace("\r", "\\r").replace("\n", "\\n")
    escaped = escaped.replace("\u2028", "\\u2028").replace("\u2029", "\\u2029")
    if quote == "'":
        return escaped.replace("'", "\\'")
    if quote == '"':
        return escaped.replace('"', '\\"')
    # 模板字符串还需阻断 `${...}` 插值，避免凭据被当作表达式执行。
    return escaped.replace("`", "\\`").replace("${", "\\${")


def resolve_javascript(template: str, variables: dict[str, str]) -> str:
    """解析 JavaScript 模板，并按所在语法位置编码变量值。

    字符串字面量内仅写入转义后的内容；字面量外写入 JSON 字符串字面量。
    这样既兼容历史 ``'{{PASSWORD}}'`` 写法，也避免引号、反斜杠和换行破坏脚本。
    """
    if not template or "{{" not in template:
        return template

    def _sub(match: re.Match[str]) -> str:
        key = match.group(1)
        if key not in variables:
            logger.debug("JavaScript 变量未命中，保留原占位符: %s", key)
            return match.group(0)
        raw = variables[key]
        value = resolve(str(raw), variables) if isinstance(raw, str) else str(raw)
        context = _javascript_context_at(template, match.start())
        quote = {"single": "'", "double": '"', "template": "`"}.get(context)
        if quote is not None:
            return _escape_javascript_string(value, quote)
        if context in {"comment", "line_comment", "block_comment", "regex"}:
            logger.debug("JavaScript 占位符位于 %s 中，保留原值: %s", context, key)
            return match.group(0)
        return json.dumps(value, ensure_ascii=False)

    return _VAR_PATTERN.sub(_sub, template)
