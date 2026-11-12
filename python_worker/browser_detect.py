"""浏览器环境探测：托管引擎 registry 判定与系统渠道可执行文件探测（WorkerCore 拆分）。

自 playwright_worker.py 迁出（原 playwright_bootstrap 注释块随之迁移）。纯文件
系统 / registry 检查，不冷启 Playwright driver；判定语义与 Rust 侧
``environment::browser_registry`` 必须保持一致（同一批用例在两端各有测试，
见 tests/test_worker.py 的「浏览器探测」用例；任一侧漂移都会被测试抓到）：

- 唯一事实源 = Playwright 包内 ``driver/package/browsers.json``（精确 revision）
- 完成判据   = 目录内含 ``INSTALLATION_COMPLETE``（Playwright registry 的 isInstalled 判据）
- chromium   = 要求 ``chromium-<rev>`` 与 ``chromium_headless_shell-<rev>`` 两套都完整

为什么不再用 ``browser_type.executable_path``：它返回的是 **headful** 路径
（``chromium-<rev>/chrome-win64/chrome.exe``），而后台运行默认 headless，实际使用的是
``chromium_headless_shell-<rev>``——旧实现据此判定会把「缺 headless shell」误判为可用。
新判定只做文件系统检查，无需冷启 sync_playwright driver（省 200-500ms 冷启开销）。
"""

from __future__ import annotations

import importlib.util
import json
import logging
import os
import shutil
import sys
from pathlib import Path

logger = logging.getLogger(__name__)

#: 安装完成标记（Playwright registry 的 isInstalled 判据）
_INSTALLATION_COMPLETE = "INSTALLATION_COMPLETE"

#: 引擎 → 需要的 registry 条目名（chromium 需 headful + headless shell 两套）
_REQUIRED_REGISTRY_NAMES: dict[str, tuple[str, ...]] = {
    "chromium": ("chromium", "chromium-headless-shell"),
    "firefox": ("firefox",),
    "webkit": ("webkit",),
}

#: 回退用的目录前缀（registry 不可读或带平台差异化修订时；headless shell 目录名是下划线）
_FALLBACK_PREFIXES: dict[str, tuple[str, ...]] = {
    "chromium": ("chromium-", "chromium_headless_shell-"),
    "firefox": ("firefox-",),
    "webkit": ("webkit-",),
}

#: registry 必需目录名缓存：engine -> (registry mtime, 目录名元组或 None)。
#: registry 只随 Playwright 版本变化，按 mtime 失效即可。
_REQUIRED_DIRS_CACHE: dict[str, tuple[float, tuple[str, ...] | None]] = {}


def _browser_cache_dir() -> Path | None:
    """Playwright 浏览器缓存根目录（PLAYWRIGHT_BROWSERS_PATH 优先，空 / "0" 走 OS 默认）"""
    override = os.environ.get("PLAYWRIGHT_BROWSERS_PATH", "")
    if override and override != "0":
        return Path(override)
    if os.name == "nt":
        base = os.environ.get("LOCALAPPDATA")
        return Path(base) / "ms-playwright" if base else None
    if sys.platform == "darwin":
        return Path.home() / "Library" / "Caches" / "ms-playwright"
    return Path.home() / ".cache" / "ms-playwright"


def _registry_path() -> Path | None:
    """Playwright 包内 browsers.json 路径（按包定位，不猜 venv 目录布局）"""
    try:
        spec = importlib.util.find_spec("playwright")
    except (ImportError, ValueError):
        return None
    if spec is None or not spec.origin:
        return None
    return Path(spec.origin).parent / "driver" / "package" / "browsers.json"


def _required_dir_names(engine: str, registry: Path) -> tuple[str, ...] | None:
    """从 registry 推导必需目录名；不可读 / 带平台差异化修订时返回 None（走回退）"""
    wanted = _REQUIRED_REGISTRY_NAMES.get(engine)
    if not wanted:
        return None
    try:
        mtime = registry.stat().st_mtime
    except OSError:
        mtime = 0.0
    cached = _REQUIRED_DIRS_CACHE.get(engine)
    if cached is not None and cached[0] == mtime:
        return cached[1]

    names: tuple[str, ...] | None = None
    try:
        raw = json.loads(registry.read_text(encoding="utf-8"))
        by_name = {item["name"]: item for item in raw.get("browsers", [])}
        if all(name in by_name for name in wanted) and not any(
            "revisionOverrides" in by_name[name] for name in wanted
        ):
            # registry 名 → 目录名：`-` 换成 `_` 再拼 revision
            # （registry 的 chromium-headless-shell 对应磁盘 chromium_headless_shell-1234）
            names = tuple(
                f"{name.replace('-', '_')}-{by_name[name]['revision']}" for name in wanted
            )
    except (OSError, ValueError, KeyError, TypeError) as exc:  # noqa: BLE001
        logger.debug("读取 Playwright registry 失败，回退前缀判定: %s", exc)
        names = None
    _REQUIRED_DIRS_CACHE[engine] = (mtime, names)
    return names


def _fallback_available(cache_dir: Path, engine: str) -> bool:
    """回退判定（registry 不可读）：任一匹配前缀的目录安装完成即可用"""
    prefixes = _FALLBACK_PREFIXES.get(engine)
    if not prefixes:
        return False
    try:
        entries = list(cache_dir.iterdir())
    except OSError:
        return False
    return any(
        entry.is_dir()
        and any(entry.name.startswith(prefix) for prefix in prefixes)
        and (entry / _INSTALLATION_COMPLETE).is_file()
        for entry in entries
    )


def _managed_engine(channel: str) -> str | None:
    """托管渠道 → 引擎名；系统浏览器 / 自定义 / 未知渠道返回 None（与 Rust 侧一致）"""
    if channel in ("chromium", "playwright"):
        return "chromium"
    if channel in ("firefox", "webkit"):
        return channel
    return None


#: 系统通道 → 面向用户的安装/切换建议（渠道名 → 展示名）
_SYSTEM_CHANNEL_LABELS: dict[str, str] = {
    "msedge": "Microsoft Edge",
    "chrome": "Google Chrome",
}


def _unavailable_channel_hint(channel: str, custom_path: str = "") -> str:
    """非托管通道不可用时的可操作提示（供 Rust 侧日志直接展示）

    这些通道没有 Playwright 可下载的二进制，`_missing_components` 无从给出目录名，
    故此处的文案直接指向「装浏览器」或「换通道」两条可执行路径。
    """
    if channel == "custom":
        return f"自定义浏览器不可用（路径 {custom_path or '未填写'}），请检查路径或改用其他浏览器"
    label = _SYSTEM_CHANNEL_LABELS.get(channel)
    if label:
        return f"未检测到 {label}，请安装 {label} 或改用 chromium（托管内核，可自动下载）"
    return f"未知浏览器渠道 {channel!r}，请改用 msedge / chrome / chromium"


def _missing_components(engine: str) -> list[str]:
    """缺失 / 未完成的组件目录名（供 Rust 侧给出可操作的错误信息）

    registry 可读时给出精确目录名；不可读（含带平台差异化修订的引擎）时回退为
    期望前缀 `xxx-*`，与 Rust 侧 `browser_registry::missing_components` 同口径——
    保证「未就绪」时该字段不为空，诊断信息始终可操作。
    """
    cache_dir = _browser_cache_dir()
    registry = _registry_path()
    if cache_dir is not None and registry is not None:
        names = _required_dir_names(engine, registry)
        if names is not None:
            return [
                name
                for name in names
                if not (cache_dir / name / _INSTALLATION_COMPLETE).is_file()
            ]
    return [f"{prefix}*" for prefix in _FALLBACK_PREFIXES.get(engine, ())]


def _engine_available(engine: str) -> bool:
    """判定托管引擎（chromium/firefox/webkit）是否安装完整"""
    cache_dir = _browser_cache_dir()
    if cache_dir is None:
        return False
    registry = _registry_path()
    if registry is not None:
        names = _required_dir_names(engine, registry)
        if names is not None:
            return all(
                (cache_dir / name / _INSTALLATION_COMPLETE).is_file() for name in names
            )
    return _fallback_available(cache_dir, engine)


def _ensure_browser(channel: str = "playwright", custom_path: str = "") -> bool:
    """确保目标浏览器可用；托管渠道按 registry + 安装完成标记判定（语义同 Rust 侧）。"""
    channel = str(channel or "playwright").strip().lower()
    custom_path = str(custom_path or "").strip()
    # 系统浏览器（Edge/Chrome/自定义路径）需真实探测可执行文件，而非恒 True。
    # 否则健康检查假成功，启动时才抛 obscure Playwright error。
    if channel == "custom":
        return bool(custom_path and Path(custom_path).is_file())
    if channel in ("msedge", "chrome"):
        # 复用 Rust 侧 is_edge/chrome_installed 的同口径判定（多路径 + which）
        # 此处为 Python 侧二次校验：优先 which，其次 Windows 固定路径
        if channel == "msedge":
            bins = ["msedge", "microsoft-edge", "microsoft-edge-stable"]
        else:
            bins = ["chrome", "google-chrome", "google-chrome-stable"]
        for b in bins:
            if shutil.which(b):
                return True
        # Windows 固定路径兜底（与 Rust 侧一致，含 LOCALAPPDATA 用户级安装）
        candidates = []
        if os.name == "nt":
            for env_key in ("PROGRAMFILES", "PROGRAMFILES(X86)", "LOCALAPPDATA"):
                base = os.environ.get(env_key)
                if not base:
                    continue
                if channel == "msedge":
                    candidates.append(
                        Path(base) / "Microsoft" / "Edge" / "Application" / "msedge.exe"
                    )
                else:
                    candidates.append(
                        Path(base) / "Google" / "Chrome" / "Application" / "chrome.exe"
                    )
            if any(p.exists() for p in candidates):
                return True
        else:
            # macOS /Applications 与 ~/Applications：按渠道对应 app 名依序探测，
            # 候选顺序保持 /Applications 优先，HOME 未设置时跳过用户级目录
            app_name = "Microsoft Edge.app" if channel == "msedge" else "Google Chrome.app"
            home = os.environ.get("HOME")
            candidates = [Path("/Applications") / app_name]
            if home:
                candidates.append(Path(home) / "Applications" / app_name)
            if any(p.exists() for p in candidates):
                return True
        return False

    # 托管渠道：显式枚举合法值（与 Rust is_channel_available 一致，未知渠道不可用）
    engine = _managed_engine(channel)
    if engine is None:
        logger.debug("未知浏览器渠道，视为不可用: %s", channel)
        return False

    available = _engine_available(engine)
    if not available:
        logger.debug("托管浏览器 %s 未安装完整（缓存目录 %s）", engine, _browser_cache_dir())
    return available


__all__ = [
    "_INSTALLATION_COMPLETE",
    "_ensure_browser",
    "_engine_available",
    "_fallback_available",
    "_managed_engine",
    "_missing_components",
    "_unavailable_channel_hint",
]
