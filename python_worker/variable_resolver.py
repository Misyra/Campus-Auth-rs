"""模板变量解析。

将步骤配置中的 ``{{VAR}}`` 占位符替换为实际值。变量来源为 Profile 字段
（USERNAME / PASSWORD / ISP / LOGIN_URL 等）与任务自定义的 ``variables``。

支持链式引用（递归解析）：任务 ``variables`` 中可定义
``{"username": "{{USERNAME}}"}``，``USERNAME`` 再由外部凭证映射提供实际值。
解析时遇到替换结果仍含 ``{{...}}`` 会继续展开，并检测循环引用与深度限制。
"""

from __future__ import annotations

import re
from typing import Any

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
        return template
    visited = _visited or set()

    def _sub(match: re.Match[str]) -> str:
        key = match.group(1)
        if key in visited:
            # 循环引用：保留原占位符，避免无限递归
            return match.group(0)
        val = variables.get(key)
        if val is None:
            # 未找到：保留原占位符
            return match.group(0)
        # 值中若仍含 {{...}}，递归展开（如 username -> {{USERNAME}} -> admin）
        if isinstance(val, str) and "{{" in val:
            return resolve(val, variables, _depth + 1, visited | {key})
        return str(val)

    return _VAR_PATTERN.sub(_sub, template)


def resolve_value(value: Any, variables: dict[str, str]) -> Any:
    """递归解析任意结构中的模板占位符。

    - 字符串：逐处替换 ``{{VAR}}``。
    - 列表 / 字典：递归处理其元素。
    - 其他类型：原样返回。

    参数:
        value: 待解析的任意 JSON 值。
        variables: 变量映射。

    返回:
        解析后的同构值。
    """
    if isinstance(value, str):
        return resolve(value, variables)
    if isinstance(value, list):
        return [resolve_value(v, variables) for v in value]
    if isinstance(value, dict):
        return {k: resolve_value(v, variables) for k, v in value.items()}
    return value
