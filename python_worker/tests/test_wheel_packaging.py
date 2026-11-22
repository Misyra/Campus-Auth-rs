"""Wheel 打包面钉子：hatch include 通配必须覆盖门面模块的全部本地 import。

历史缺陷：wheel 配置曾为 ``packages = ["."]`` 配只列 7 个文件的 include——
hatchling 语义中 packages / only-include 会整体压掉 include（include 是死配置），
伴生 Mixin 拆分后顶层模块远多于 7 个。本文件不 import 任何 Worker 模块，
纯静态（tomllib + ast + fnmatch）钉住三件事：
配置由 include 驱动且根锚定、门面与入口的全部本地 import 被覆盖、
顶层模块全量被覆盖（新增伴生模块漏配在此尽早失败）。
"""

from __future__ import annotations

import ast
import tomllib
from fnmatch import fnmatch
from pathlib import Path

# 仓库内 python_worker/ 根（tests/ 的上一级），打包面锚定以此为根
WORKER_ROOT = Path(__file__).resolve().parent.parent


def _wheel_config() -> dict:
    """读取 hatch wheel 目标的文件选择配置。"""
    pyproject = tomllib.loads(
        (WORKER_ROOT / "pyproject.toml").read_text(encoding="utf-8")
    )
    return pyproject["tool"]["hatch"]["build"]["targets"]["wheel"]


def _top_level_local_imports(source: Path) -> set[str]:
    """静态扫描源文件 import 的顶层本地模块名（ast 解析，不执行 import）。

    仅保留仓库根存在同名 .py 的模块——stdlib / playwright 等外部依赖
    不在 wheel 打包面断言口径内。
    """
    tree = ast.parse(source.read_text(encoding="utf-8"))
    roots: set[str] = set()
    for node in ast.walk(tree):
        if isinstance(node, ast.Import):
            roots.update(alias.name.split(".")[0] for alias in node.names)
        elif isinstance(node, ast.ImportFrom):
            # 相对导入（level > 0）不在打包面语义内；__future__ 无同名根模块，
            # 由下方 is_file 过滤兜底
            if node.level == 0 and node.module:
                roots.add(node.module.split(".")[0])
    return {name for name in roots if (WORKER_ROOT / f"{name}.py").is_file()}


def _covered(filename: str, include: list[str]) -> bool:
    """按 hatchling 根锚定语义判断 filename 是否被 include 模式覆盖。

    断言已保证模式以 / 锚定仓库根；顶层模块文件名不含路径分隔符，
    去锚后按 fnmatch 匹配即可忠实还原 hatchling 的命中结果。
    """
    return any(fnmatch(filename, pattern.lstrip("/")) for pattern in include)


def test_wheel_selection_is_include_driven_and_root_anchored():
    """include 必须真实生效：无 packages/only-include 覆盖项，且模式根锚定。

    - packages / only-include 一旦出现，include 整体被压掉（死配置陷阱）；
    - hatchling 的 ``*`` 可跨越路径分隔符，未锚定的 ``*.py`` 会把 tests/
      一并打进 wheel，故模式必须以 ``/`` 锚定仓库根。
    """
    wheel = _wheel_config()
    assert "packages" not in wheel, (
        "packages 会整体压掉 include（hatchling 语义），include 退化为死配置"
    )
    assert "only-include" not in wheel, (
        "only-include 会整体压掉 include（hatchling 语义），include 退化为死配置"
    )
    include = [str(p) for p in wheel.get("include", [])]
    assert include, "wheel 打包面必须由 include 通配显式钉住"
    assert all(p.startswith("/") for p in include), (
        f"include 模式必须以 / 锚定仓库根（未锚定通配会带入子目录）: {include}"
    )


def test_wheel_include_covers_facade_and_entry_local_imports():
    """组装门面与 IPC 入口的全部本地 import 都必须在 include 覆盖内。

    门面 playwright_worker.py 聚合全部伴生 Mixin 与再导出符号属主，
    入口 worker_main.py 再从中取用——任一本地 import 未随 wheel 发布，
    安装即在 import 期崩溃。
    """
    include = [str(p) for p in _wheel_config()["include"]]
    for filename in ("worker_main.py", "playwright_worker.py"):
        assert _covered(filename, include), f"{filename} 自身必须随 wheel 发布"
    for source in ("playwright_worker.py", "worker_main.py"):
        imports = _top_level_local_imports(WORKER_ROOT / source)
        assert imports, f"{source} 静态扫描结果为空，扫描逻辑疑似失效"
        missing = sorted(f"{m}.py" for m in imports if not _covered(f"{m}.py", include))
        assert not missing, f"{source} 的本地 import 未被 wheel include 覆盖: {missing}"


def test_wheel_include_covers_all_top_level_modules():
    """顶层模块全量口径：改回显式清单时，新增伴生模块漏列在此尽早失败。

    当前 ``/*.py`` 通配天然覆盖；断言的意义在于防止未来把通配收窄成
    显式清单时静默漏掉某个无人 import 却被动态引用的模块。
    """
    include = [str(p) for p in _wheel_config()["include"]]
    top_level = sorted(p.name for p in WORKER_ROOT.glob("*.py"))
    assert len(top_level) >= 20, (
        f"顶层模块数异常（预期 21 个伴生/门面模块）: {len(top_level)}"
    )
    missing = [name for name in top_level if not _covered(name, include)]
    assert not missing, f"顶层模块未被 wheel include 覆盖: {missing}"
