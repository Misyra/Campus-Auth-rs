"""Worker 产物路径锚定与启动残留清理（WorkerCore 拆分）。

自 playwright_worker.py 迁出。所有写盘产物（debug 截图 / 反馈快照 / AI 捕获 /
浏览器持久数据）统一锚定到 Worker 工程目录或应用数据目录，避免依赖 Rust spawn
继承的 CWD（未设 current_dir，可能是任意目录）。测试经 monkeypatch 替换
``_debug_screenshot_dir`` / ``_browser_data_dir`` / ``_capture_dir`` 重定向落盘。
"""

from __future__ import annotations

import logging
import os
import shutil
import time
from pathlib import Path

logger = logging.getLogger(__name__)

# Worker 脚本所在目录（所有伴生模块与本文件同目录，解析结果一致）：
# debug 截图等相对目录一律锚定到此，避免依赖 Rust spawn 继承的 CWD
#（未设 current_dir，可能是任意目录）
_WORKER_DIR = Path(__file__).resolve().parent


def _browser_data_dir() -> Path:
    """浏览器持久化数据目录（按 channel 隔离，锚定到应用数据目录）。

    优先使用 Rust 侧注入的 ``CAMPUS_AUTH_BASE_PATH``（spawn 时设置），即
    ``<base_path>/config/browser-data``，与 Python 原版 ``config/browser-data`` 对齐，
    避免放在 Worker 脚本目录（便携包更新/重建时会被清空登录态）。
    环境变量缺失时回退到 Worker 脚本目录。
    """
    base = os.environ.get("CAMPUS_AUTH_BASE_PATH")
    root = Path(base).resolve() if base else _WORKER_DIR
    return root / "config" / "browser-data"


def _runtime_worker_project_dir() -> Path:
    """运行时 worker 工程目录：与 Rust 侧 ``worker_project_dir`` 首选候选对齐。

    debug 截图与 AI 捕获产物均由 Rust 按 ``<base_path>/python_worker`` 读盘，
    Python 写入侧必须锚定同一目录——resources 提取 / 只读安装布局下脚本目录
    （``_WORKER_DIR``）与运行时工程目录可能不同。env 缺失或目录不存在时回退
    脚本目录（dev 场景两者本就相同，Rust 的多级兜底也落在同一处）。
    """
    base = os.environ.get("CAMPUS_AUTH_BASE_PATH")
    if base:
        candidate = Path(base).resolve() / "python_worker"
        if candidate.exists():
            return candidate
    return _WORKER_DIR


def _debug_screenshot_dir() -> Path:
    """调试截图目录（锚定运行时 worker 工程目录，与 Rust 读盘侧一致）。"""
    return _runtime_worker_project_dir() / "debug"


def _feedback_capture_dir(stamp: str) -> Path:
    """问题报告页面快照目录（位于 Rust 允许读取的调试目录内）。"""
    return _debug_screenshot_dir() / f"feedback-{stamp}"


def _capture_dir() -> Path:
    """AI 任务生成的页面捕获目录（同上锚定，latest 每次覆盖）。"""
    return _runtime_worker_project_dir() / "captures" / "latest"


# 模块加载时刻：启动清理时用于判定“上次会话残留”（mtime 早于该时刻的文件）
_MODULE_LOAD_TIME = time.time()


def _purge_stale_debug_screenshots() -> None:
    """Worker 启动时清理上次会话残留的截图文件（A7）。

    Worker 进程被强杀时，任务级（_run_task）与调试级（_cleanup_debug_screenshots）
    清理均不会执行，debug/ 目录会残留可能含明文凭据的截图。启动时
    best-effort 删除修改时间早于本进程启动（模块加载时刻）的 PNG/JPEG，
    以及完整的 ``feedback-*`` 快照目录；
    多 Worker 并发启动时，正被其他进程写入的新文件（mtime 较新）不受影响。
    """
    directory = _debug_screenshot_dir()
    try:
        entries = list(directory.iterdir())
    except FileNotFoundError:
        return
    except Exception as exc:  # noqa: BLE001
        logger.warning(f"启动清理残留截图失败（忽略）: {exc}")
        return
    for entry in entries:
        try:
            if entry.is_dir() and entry.name.startswith("feedback-"):
                if entry.stat().st_mtime < _MODULE_LOAD_TIME:
                    shutil.rmtree(entry)
                    logger.info("已清理上次会话残留反馈快照: %s", entry.name)
                continue
            if entry.suffix.lower() not in {".png", ".jpg", ".jpeg"} or not entry.is_file():
                continue
            if entry.stat().st_mtime >= _MODULE_LOAD_TIME:
                continue
            entry.unlink(missing_ok=True)
            logger.info("已清理上次会话残留截图: %s", entry.name)
        except Exception as exc:  # noqa: BLE001
            logger.debug(f"清理残留截图失败 {entry}: {exc}")


__all__ = [
    "_MODULE_LOAD_TIME",
    "_WORKER_DIR",
    "_browser_data_dir",
    "_capture_dir",
    "_debug_screenshot_dir",
    "_feedback_capture_dir",
    "_purge_stale_debug_screenshots",
    "_runtime_worker_project_dir",
]
