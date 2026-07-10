"""自定义脚本执行器 — 在子进程中执行脚本任务。

本模块为自包含实现，去除了旧项目 ``app`` 包的依赖。
说明：脚本任务（type=script/shell）在 Rust 侧由 TaskExecutor 直接执行，
不经过 Python Worker；本模块保留为独立可用的工具，供 Worker 内部或调试场景调用。
"""

from __future__ import annotations

import contextlib
import os
import platform
import re
import subprocess
import sys
import tempfile
import time
from pathlib import Path
import logging

logger = logging.getLogger(__name__)

# 默认脚本超时（秒）
DEFAULT_TIMEOUT = 60

# 正则提取解释器名：匹配字母前缀 + 可选版本号（如 python3、python3.12）
_EXEC_NAME_RE = re.compile(r"^([a-zA-Z]+)(?:\d+\.?\d*)?$")

# 解释器 → 临时文件后缀映射
_BINARY_EXT_MAP = {
    "python": ".py",
    "python3": ".py",
    "node": ".js",
    "ruby": ".rb",
    "php": ".php",
    "perl": ".pl",
    "raku": ".raku",
    "lua": ".lua",
    "r": ".R",
    "rscript": ".R",
    "cmd": ".bat",
    "powershell": ".ps1",
    "pwsh": ".ps1",
    "bash": ".sh",
    "sh": ".sh",
    "zsh": ".sh",
    "fish": ".fish",
}


def _get_interpreter_name(binary: str) -> str:
    """从解释器路径中提取语言名称（小写）。"""
    stem = Path(binary).stem
    match = _EXEC_NAME_RE.match(stem)
    if match:
        return match.group(1).lower()
    return stem.lower()


def _get_temp_extension(binary: str) -> str:
    """根据解释器名推断临时文件后缀。"""
    return _BINARY_EXT_MAP.get(_get_interpreter_name(binary), "")


def detect_available_binaries() -> list[dict[str, str]]:
    """探测系统中可用的脚本解释器。

    返回:
        解释器信息列表，每项含 ``{"name", "path"}``。
    """
    candidates = [
        "python",
        "python3",
        "node",
        "ruby",
        "php",
        "perl",
        "pwsh",
        "powershell",
        "bash",
        "sh",
        "cmd",
    ]
    found: list[dict[str, str]] = []
    for name in candidates:
        path = shutil_which(name)
        if path:
            found.append({"name": name, "path": path})
    return found


def shutil_which(name: str) -> str | None:
    """跨平台查找可执行文件路径。"""
    import shutil

    return shutil.which(name)


def _build_cmd(binary_path: str, script_file: str) -> list[str]:
    """根据解释器类型构建执行命令。"""
    exe_name = _get_interpreter_name(binary_path)
    if platform.system() == "Windows":
        if exe_name in ("powershell", "pwsh"):
            return [
                binary_path,
                "-NoProfile",
                "-ExecutionPolicy",
                "Bypass",
                "-WindowStyle",
                "Hidden",
                "-File",
                script_file,
            ]
        if exe_name == "cmd":
            return [binary_path, "/c", script_file]
    else:
        if exe_name in ("bash", "sh", "zsh", "fish"):
            return [binary_path, script_file]
    return [binary_path, script_file]


def _build_minimal_env() -> dict[str, str]:
    """构建子进程最小环境变量（仅系统基础变量）。"""
    safe: dict[str, str] = {}
    base_keys = {"PATH", "HOME", "USER", "TEMP", "TMP"}
    if platform.system() == "Windows":
        base_keys.update(
            {
                "SystemRoot",
                "SystemDrive",
                "ComSpec",
                "windir",
                "USERPROFILE",
                "APPDATA",
                "LOCALAPPDATA",
            }
        )
    else:
        base_keys.update({"LANG", "LC_ALL", "SHELL", "XDG_RUNTIME_DIR"})
    for key in base_keys:
        val = os.environ.get(key)
        if val:
            safe[key] = val
    safe["PYTHONIOENCODING"] = "utf-8"
    return safe


def run_script(
    script_path: str | Path,
    args: list[str] | None = None,
    timeout: int = DEFAULT_TIMEOUT,
    binary_path: str = "",
) -> dict:
    """执行脚本文件并返回结构化结果。

    参数:
        script_path: 脚本文件路径。
        args: 命令行参数列表。
        timeout: 超时秒数。
        binary_path: 解释器路径，为空时使用 ``sys.executable``。

    返回:
        字典：``{"exit_code", "stdout", "stderr", "duration_ms", "success"}``。
        其中 success 表示脚本是否正常执行完毕（exit code 0）。
    """
    binary = binary_path or sys.executable
    script = str(script_path)
    cmd = _build_cmd(binary, script) + list(args or [])
    env = _build_minimal_env()
    cwd = str(Path(script).parent)

    start = time.perf_counter()
    try:
        proc = subprocess.run(
            cmd,
            capture_output=True,
            text=True,
            timeout=timeout,
            env=env,
            cwd=cwd,
            check=False,
        )
    except subprocess.TimeoutExpired:
        logger.warning(f"脚本执行超时 ({timeout}s): {script}")
        return {
            "exit_code": -1,
            "stdout": "",
            "stderr": f"执行超时 ({timeout}s)",
            "duration_ms": int((time.perf_counter() - start) * 1000),
            "success": False,
        }
    except FileNotFoundError as exc:
        logger.warning(f"脚本或解释器不存在: {exc}")
        return {
            "exit_code": -1,
            "stdout": "",
            "stderr": f"脚本或解释器不存在: {exc}",
            "duration_ms": int((time.perf_counter() - start) * 1000),
            "success": False,
        }
    except Exception as exc:  # noqa: BLE001
        logger.exception(f"脚本执行异常: {exc}")
        return {
            "exit_code": -1,
            "stdout": "",
            "stderr": f"执行异常: {exc}",
            "duration_ms": int((time.perf_counter() - start) * 1000),
            "success": False,
        }

    elapsed = int((time.perf_counter() - start) * 1000)
    stdout = (proc.stdout or "")[:500]
    stderr = (proc.stderr or "")[:500]
    success = proc.returncode == 0
    if success:
        logger.info(f"脚本执行成功 (耗时 {elapsed}ms)")
    else:
        logger.warning(f"脚本执行失败 (exit {proc.returncode}): {stderr or stdout}")
    return {
        "exit_code": proc.returncode,
        "stdout": stdout,
        "stderr": stderr,
        "duration_ms": elapsed,
        "success": success,
    }


if __name__ == "__main__":
    # 命令行直接调用：python script_runner.py <script> [args...]
    import json

    if len(sys.argv) < 2:
        print(json.dumps({"error": "缺少脚本路径参数"}))
        sys.exit(2)
    result = run_script(sys.argv[1], args=sys.argv[2:])
    print(json.dumps(result, ensure_ascii=False))
    sys.exit(0 if result["success"] else 1)
