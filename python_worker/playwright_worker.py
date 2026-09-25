"""Playwright Worker 核心：浏览器生命周期管理 + 命令处理。

本模块是 Python 侧浏览器自动化执行平面。Rust 主进程通过 NDJSON IPC
调用此处注册的处理器（见 ``COMMANDS``），每个处理器对应一种命令：
browser_health_check / test_redirect / execute_login_attempt / execute_browser_task /
debug_start / debug_step / debug_stop / ocr_recognize / shutdown。

Worker 仅作为"浏览器动作执行器"，单次动作返回 StructuredResult；
重试、状态机、取消调度由 Rust 侧负责。取消通过 ``cancel_id`` 映射到
``threading.Event``，处理器在步骤边界检查该事件。
"""

from __future__ import annotations

import asyncio
import base64
import hashlib
import importlib.util
import json
import logging
import os
import shutil
import sys
import threading
import time
import uuid
from contextlib import asynccontextmanager
from html import escape as _html_escape
from pathlib import Path
from typing import Any, AsyncIterator, Callable, Iterable
from urllib.parse import urlsplit

from models import (
    Outcome,
    StepConfig,
    StructuredResult,
    TaskConfig,
)
from step_handlers import (
    StepCancelled,
    StepContext,
    WorkerError,
    _check_cancel,
    _classify_navigation_error,
    _sleep_cancellable,
    run_step_async,
)
from ocr_runtime import OCR_TIMEOUT_SECS, _get_ocr, _preprocess_ocr_image
from debug_session import DebugSession, _build_steps_info
from variable_resolver import resolve

logger = logging.getLogger(__name__)


# 按"顶层任务/调试会话"隔离 Web Storage，同时保留 BrowserContext 级 Cookie。
# 新建 Page 使每个会话获得全新的 sessionStorage；标记（marker）让 init 脚本
# 仅在该 Page 内每个 origin 的首个文档上清空本地/会话存储，避免重复清理。
_TASK_STORAGE_ISOLATION_SCRIPT = r"""
(() => {
  const marker = "__campus_auth_storage_isolated_v1__";
  try {
    if (sessionStorage.getItem(marker) === "1") return;
  } catch (_) {}
  try { localStorage.clear(); } catch (_) {}
  try {
    sessionStorage.clear();
    sessionStorage.setItem(marker, "1");
  } catch (_) {}
})();
"""

# 持久化上下文应保留 localStorage 登录态；新建 Page 已天然隔离 sessionStorage，
# 这里只显式清理会话级存储，避免门户把 token 放在 localStorage 时失去持久化意义。
_PERSISTENT_SESSION_STORAGE_ISOLATION_SCRIPT = r"""
(() => {
  try { sessionStorage.clear(); } catch (_) {}
})();
"""

# Worker 脚本所在目录：debug 截图等相对目录一律锚定到此，
# 避免依赖 Rust spawn 继承的 CWD（未设 current_dir，可能是任意目录）
_WORKER_DIR = Path(__file__).resolve().parent

# Worker 版本（任务 10）：与 pyproject.toml 的 project.version 保持同步（手动维护），
# 随 worker_health_check 响应上报给 Rust 侧。单点定义：worker_main 从此处导入。
WORKER_VERSION = "1.0.0"

# 浏览器关闭/会话释放的兜底等待上限（秒）：close 可能挂起（driver 未及时退出等），
# 统一超时跳过，避免单条挂起命令阻塞 Worker 命令队列
_WAIT_TIMEOUT_SECS = 8.0
# 重定向检测首导航完成后继续观察 JS / Meta Refresh / 弹窗跳转的时长（秒）。
_REDIRECT_TEST_SETTLE_SECS = 5.0
# 重定向检测最多读取的可见文本，避免异常页面造成无界 IPC 前内存增长。
_REDIRECT_TEST_TEXT_LIMIT = 64 * 1024


def _classify_redirect_test(
    *,
    trigger_url: str,
    final_url: str,
    response_status: int | None,
    visible_text: str,
    password_inputs: int,
    account_inputs: int,
    forms: int,
) -> dict[str, Any]:
    """按地址变化、表单结构和页面语义综合判断重定向检测结果。

    单独出现“登录”很常见，不能直接判为校园网认证页；只有强校园网语义，或
    地址变化与登录结构/多项关键字相互印证时才返回 ``detected``。
    返回值刻意不包含最终 URL 或页面正文，避免门户临时 token 与账号信息进入 IPC。
    """
    text = " ".join(visible_text.lower().split())
    strong_keywords = (
        "校园网",
        "上网认证",
        "网络认证",
        "统一身份认证",
        "captive portal",
        "network authentication",
    )
    generic_keywords = (
        "登录",
        "认证",
        "账号",
        "用户名",
        "密码",
        "运营商",
        "portal",
        "sign in",
        "log in",
        "username",
        "password",
    )
    strong_hits = sum(1 for keyword in strong_keywords if keyword in text)
    generic_hits = sum(1 for keyword in generic_keywords if keyword in text)

    try:
        trigger = urlsplit(trigger_url)
        final = urlsplit(final_url)
        origin_changed = (
            trigger.scheme.lower(),
            (trigger.hostname or "").lower(),
            trigger.port,
        ) != (
            final.scheme.lower(),
            (final.hostname or "").lower(),
            final.port,
        )
        url_changed = trigger_url.rstrip("/") != final_url.rstrip("/")
    except ValueError:
        origin_changed = False
        url_changed = trigger_url != final_url

    # Windows NCSI 默认触发页在线时返回这段固定文本；204 同样属于直通在线。
    online_marker = "microsoft connect test" in text and len(text) < 512
    structural_score = min(password_inputs, 1) * 3
    structural_score += min(account_inputs, 1)
    structural_score += 1 if forms and (password_inputs or account_inputs) else 0
    redirect_score = 2 if origin_changed else (1 if url_changed else 0)
    semantic_score = min(generic_hits, 3) + min(strong_hits, 1) * 2
    score = structural_score + redirect_score + semantic_score

    detected = (
        score >= 5
        or (strong_hits > 0 and generic_hits >= 2)
        or (password_inputs > 0 and generic_hits > 0)
        or (origin_changed and structural_score >= 2 and generic_hits > 0)
    )
    if detected:
        return {"status": "detected"}
    if response_status == 204 or online_marker:
        return {"status": "online"}
    return {"status": "not_detected"}


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


def _to_ms(bs: dict, key: str, default_ms: int) -> int:
    """从 browser_settings 读取超时并归一化为毫秒。

    Rust 侧 ``BrowserSettings`` 中 ``timeout`` / ``navigation_timeout`` 为
    u32 秒，而 Playwright API 需要毫秒，统一 ×1000；缺省值已是毫秒，原样返回。
    key 缺失、非法值与**非正值（≤0）**统一回退 ``default_ms``：0 会让
    Playwright 每个操作瞬间超时失败（如 close_browser 的 timeout=0 直接把
    所有步骤打成 1ms 超时），此处一处保护所有消费点。
    """
    val = bs.get(key)
    if val is None:
        return default_ms
    try:
        ival = int(val)
    except (TypeError, ValueError):
        return default_ms
    if ival <= 0:
        return default_ms
    return ival * 1000


def _nav_timeout(bs: dict) -> int:
    """从 browser_settings 读取导航超时（毫秒），缺省 15000ms。"""
    return _to_ms(bs, "navigation_timeout", 15000)


# ── 步骤执行器（原 browser_runner.py）──


def _is_truthy(value: Any) -> bool:
    """判定 store_as 变量值的真假（对齐原项目 v4.2.3 _is_truthy）。

    - bool: 直接返回
    - None: False
    - str: "false"/"0"/""/"no"/"off"（忽略大小写与空白）→ False；其他 → True
    - int/float: 非零 → True
    - 其他: bool(value)
    """
    if isinstance(value, bool):
        return value
    if value is None:
        return False
    if isinstance(value, str):
        return value.strip().lower() not in ("false", "0", "", "no", "off")
    if isinstance(value, (int, float)):
        return value != 0
    return bool(value)


def _build_result(outcome: Outcome, message: str, context: StepContext, start: float) -> StructuredResult:
    """汇总执行结果为 StructuredResult。"""
    duration_ms = int((time.perf_counter() - start) * 1000)
    return StructuredResult(
        outcome=outcome.value,
        message=message,
        duration_ms=duration_ms,
        screenshots=list(context.screenshots),
    )


def _normalize_step_failure(
    exc: Exception, cancelled_message: str = "步骤已取消"
) -> tuple[Outcome, str]:
    """把步骤失败异常归一为 (outcome, message)，三处执行路径共用。

    归一规则（逐字段等价于原 run_steps / debug_step / debug_run_all 的内联
    实现；StepCancelled 是 WorkerError 子类，必须先于 WorkerError 判断）：
    - StepCancelled → (CANCELLED, cancelled_message)
    - WorkerError → (Outcome(exc.outcome), exc.message)
    - 其他未预期异常 → (UNKNOWN_ERROR, f"执行异常: {exc}")

    堆栈日志仍由调用方按各自场景记录（日志触发条件：非 WorkerError 即
    未预期异常），本函数不负责日志。
    """
    if isinstance(exc, StepCancelled):
        return Outcome.CANCELLED, cancelled_message
    if isinstance(exc, WorkerError):
        return Outcome(exc.outcome), exc.message
    return Outcome.UNKNOWN_ERROR, f"执行异常: {exc}"


def _resolve_start_url(task_url: str, variables: dict, fallback_url: str = "") -> str:
    """解析首导航地址：任务自身 url 优先，未配置时回落 Profile 有效登录地址。

    任务 url 通常是 ``{{LOGIN_URL}}``（默认任务即如此，解析结果就是登录首导航地址），
    用户在任务里硬编码地址时以其为准——调试与任务执行不比真实登录多绕一层。

    ``resolve`` 在变量未命中时会保留 ``{{...}}`` 字面量，那不是一个可访问地址，按空
    处理；两者皆空时返回空串，由调用方跳过导航而不是把空串交给 Playwright
    （``page.goto("")`` 会以无效 URL 中断启动，会话根本建不起来）。
    """
    resolved = resolve(task_url or "", variables).strip()
    if "{{" in resolved:
        resolved = ""
    return resolved or (fallback_url or "").strip()


def _profile_login_url(params: dict) -> str:
    """Profile 的有效浏览器首导航地址：``trigger_url`` 优先、回落 ``auth_url``。

    与登录首导航口径一致：Rust 侧重定向登录（登录网址留空）会把有效触发地址下发到
    ``trigger_url``，旧版同时保存两个地址时以触发地址为准。
    """
    trigger = str(params.get("trigger_url", "") or "").strip()
    return trigger or str(params.get("auth_url", "") or "").strip()


async def run_steps(page: Any, steps: list[StepConfig], context: StepContext) -> StructuredResult:
    """按序执行步骤列表。

    任务级 ``TaskConfig.timeout`` 由 Rust 调用侧按任务/登录会话语义执行看门狗，
    Python 侧只负责单步超时与可取消的步骤间延迟，避免两层总超时互相竞争。
    """
    start = time.perf_counter()
    if not steps:
        return _build_result(Outcome.UNKNOWN_ERROR, "任务未包含任何步骤，无法执行", context, start)
    failed_ids: list[str] = []
    total = len(steps)
    try:
        for idx, step in enumerate(steps):
            _check_cancel(context)
            if idx > 0 and context.step_delay > 0:
                await _sleep_cancellable(context.step_delay, context)
            try:
                await run_step_async(page, step, context, step_index=idx, total_steps=total)
            except WorkerError as exc:
                if isinstance(exc, StepCancelled):
                    raise
                if step.required:
                    raise
                failed_ids.append(step.id or f"#{idx}")
                logger.warning(f"步骤 {step.id} 失败但非必须，继续执行: {exc.message}")
        if failed_ids:
            # P12：把非必须步骤失败摘要累积进最终 message，便于定位失败的步骤
            summary = f"执行成功；{len(failed_ids)} 个非必须步骤失败: {', '.join(failed_ids)}"
            logger.warning(summary)
            return _build_result(Outcome.SUCCESS, summary, context, start)
        return _build_result(Outcome.SUCCESS, "执行成功", context, start)
    except Exception as exc:  # noqa: BLE001 — 取消/分类失败/未预期异常统一归一，保住已累计截图返回 IPC
        if not isinstance(exc, WorkerError):
            logger.exception("步骤执行未预期异常")
        outcome, message = _normalize_step_failure(exc, "执行已取消")
        return _build_result(outcome, message, context, start)


# ── 浏览器环境探测（原 playwright_bootstrap.py）──

# 判定语义与 Rust 侧 `environment::browser_registry` **必须保持一致**（同一批用例在
# 两端各有测试，见 tests/test_worker.py 的「浏览器探测」用例；任一侧漂移都会被测试抓到）：
#   唯一事实源 = Playwright 包内 `driver/package/browsers.json`（精确 revision）
#   完成判据   = 目录内含 `INSTALLATION_COMPLETE`（Playwright registry 的 isInstalled 判据）
#   chromium   = 要求 `chromium-<rev>` 与 `chromium_headless_shell-<rev>` 两套都完整
#
# 为什么不再用 `browser_type.executable_path`：它返回的是 **headful** 路径
# （`chromium-<rev>/chrome-win64/chrome.exe`），而后台运行默认 headless，实际使用的是
# `chromium_headless_shell-<rev>`——旧实现据此判定会把「缺 headless shell」误判为可用。
# 新判定只做文件系统检查，无需冷启 sync_playwright driver（省 200-500ms 冷启开销）。

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


# ── 反馈资源快照（feedback_capture 的 CSS/JS 落盘辅助）──

#: MIME → 扩展名（仅覆盖资源快照关注的类型，其余按 txt 兜底）
_RESOURCE_EXT_BY_MIME = {
    "text/css": "css",
    "application/javascript": "js",
    "text/javascript": "js",
    "application/x-javascript": "js",
}

#: 单文件与总量上限：防资源列表异常的页面把反馈包撑到不可分发
_RESOURCE_MAX_FILES = 200
_RESOURCE_MAX_BYTES = 5 * 1024 * 1024
_RESOURCE_MAX_TOTAL_BYTES = 50 * 1024 * 1024

#: HTTP 回补单资源超时：资源通常与页面同源、缓存友好，给 10s 足够
_RESOURCE_FETCH_TIMEOUT_MS = 10_000


def _channel_supports_cdp(bs: dict | None) -> bool:
    """当前浏览器渠道是否支持 CDP（仅 Chromium 系）。

    MHTML 完整快照（``Page.captureSnapshot``）与 CDP 资源快照都只存在于
    Chromium：firefox / webkit（含 custom 渠道配这两个引擎）调用
    ``context.new_cdp_session`` 必然抛 "CDP session is only available in
    Chromium"。此处作为唯一判定口径，供启动参数过滤与捕获路径共用，避免
    两条路径各写一份口径漂移。
    """
    settings = bs or {}
    channel = str(settings.get("browser_channel") or "playwright").strip().lower()
    if channel in ("firefox", "webkit"):
        return False
    if channel == "custom":
        engine = str(settings.get("custom_browser_engine") or "auto").strip().lower()
        return engine not in ("firefox", "webkit")
    return True
_STRUCTURE_MAX_CONTROLS = 300
_STRUCTURE_MAX_LOCAL_HTML = 40_000
_CAPTURE_MAX_SCREENSHOT_BYTES = 12 * 1024 * 1024


async def _capture_page_structure(page: Any) -> dict[str, Any]:
    """提取适合模型消费的页面结构，并保留脱敏局部 HTML。

    原始 input value、textarea 内容和 token 类隐藏字段不会进入摘要；局部 HTML
    仍保留标签层级与属性，作为稳定 selector 判断的必要材料。
    """
    script = r"""
    (limits) => {
      const visible = (el) => {
        const s = getComputedStyle(el); const r = el.getBoundingClientRect();
        return s.display !== 'none' && s.visibility !== 'hidden' && Number(s.opacity) !== 0 && r.width > 0 && r.height > 0;
      };
      const escAttr = (v) => String(v || '').replace(/\\/g, '\\\\').replace(/"/g, '\\"');
      const roots = [document];
      for (let i = 0; i < roots.length; i++) {
        for (const el of roots[i].querySelectorAll('*')) if (el.shadowRoot) roots.push(el.shadowRoot);
      }
      const all = (selector) => roots.flatMap((root) => Array.from(root.querySelectorAll(selector)));
      const unique = (selector) => { try { return all(selector).length === 1; } catch (_) { return false; } };
      const selectorCandidates = (el) => {
        const out = [];
        if (el.id) { const s = '#' + CSS.escape(el.id); if (unique(s)) out.push(s); }
        for (const a of ['data-testid', 'data-test', 'name', 'autocomplete', 'placeholder']) {
          const v = el.getAttribute(a); if (!v) continue;
          const s = `${el.localName}[${a}="${escAttr(v)}"]`; if (unique(s)) out.push(s);
        }
        const type = el.getAttribute('type');
        if (type) { const s = `${el.localName}[type="${escAttr(type)}"]`; if (unique(s)) out.push(s); }
        if (!out.length) {
          const parts = []; let node = el;
          while (node && node.nodeType === 1 && parts.length < 5) {
            let part = node.localName;
            if (node.id) { part += '#' + CSS.escape(node.id); parts.unshift(part); break; }
            const siblings = node.parentElement ? Array.from(node.parentElement.children).filter(x => x.localName === node.localName) : [];
            if (siblings.length > 1) part += `:nth-of-type(${siblings.indexOf(node) + 1})`;
            parts.unshift(part); node = node.parentElement;
          }
          if (parts.length) out.push(parts.join(' > '));
        }
        return out.slice(0, 4);
      };
      const labelFor = (el) => {
        const aria = el.getAttribute('aria-label'); if (aria) return aria.trim();
        if (el.labels?.length) return Array.from(el.labels).map(x => x.innerText || x.textContent || '').join(' ').trim();
        const labelled = el.getAttribute('aria-labelledby');
        if (labelled) return labelled.split(/\s+/).map(id => document.getElementById(id)?.textContent || '').join(' ').trim();
        return (el.closest('label')?.innerText || '').trim();
      };
      const controls = all('input,button,select,textarea').slice(0, limits.controls).map((el, index) => ({
        index, tag: el.localName, type: el.getAttribute('type') || '', id: el.id || '', name: el.getAttribute('name') || '',
        label: labelFor(el).slice(0, 200), placeholder: (el.getAttribute('placeholder') || '').slice(0, 200),
        text: (el.innerText || el.textContent || '').trim().slice(0, 200),
        autocomplete: el.getAttribute('autocomplete') || '', required: !!el.required, disabled: !!el.disabled,
        visible: visible(el), selectors: selectorCandidates(el),
        options: el.localName === 'select' ? Array.from(el.options).slice(0, 50).map(o => ({value: o.value, text: (o.textContent || '').trim().slice(0, 200)})) : undefined
      }));
      const forms = all('form').slice(0, 50).map((form, index) => ({
        index, id: form.id || '', name: form.getAttribute('name') || '', method: (form.method || 'get').toLowerCase(),
        action: form.action || '', visible: visible(form), selectors: selectorCandidates(form)
      }));
      const captcha = all('img,input,canvas').filter((el) => {
        const hay = [el.id, el.className, el.getAttribute('name'), el.getAttribute('alt'), el.getAttribute('placeholder'), el.getAttribute('src')].join(' ').toLowerCase();
        return /captcha|verify|valid|checkcode|验证码|校验码/.test(hay);
      }).slice(0, 30).map(el => ({tag: el.localName, id: el.id || '', name: el.getAttribute('name') || '', selectors: selectorCandidates(el)}));
      const sources = forms.length ? all('form') : (() => {
        const pwd = all('input[type="password"]')[0]; return pwd ? [pwd.closest('section,main,div') || pwd.parentElement || pwd] : [];
      })();
      const localHtml = sources.slice(0, 20).map((source) => {
        const clone = source.cloneNode(true);
        for (const el of clone.querySelectorAll('input')) {
          el.removeAttribute('value');
          if (/token|secret|password|pwd/i.test((el.getAttribute('name') || '') + ' ' + (el.id || ''))) el.setAttribute('value', '[REDACTED]');
        }
        for (const el of clone.querySelectorAll('textarea')) el.textContent = '';
        for (const el of clone.querySelectorAll('script,style,noscript')) el.remove();
        return clone.outerHTML;
      }).join('\n').slice(0, limits.localHtml);
      return { forms, controls, captcha_candidates: captcha, has_shadow_dom: roots.length > 1, local_html: localHtml };
    }
    """
    frames: list[dict[str, Any]] = []
    page_frames = list(page.frames)
    for index, frame in enumerate(page_frames):
        frame_ref = "main" if frame == page.main_frame else (frame.name or f"frame-{index}")
        parent = frame.parent_frame
        parent_ref = None
        if parent is not None:
            try:
                parent_index = page_frames.index(parent)
            except ValueError:
                parent_index = -1
            parent_ref = (
                "main"
                if parent == page.main_frame
                else (parent.name or (f"frame-{parent_index}" if parent_index >= 0 else "detached"))
            )
        try:
            detail = await frame.evaluate(
                script,
                {"controls": _STRUCTURE_MAX_CONTROLS, "localHtml": _STRUCTURE_MAX_LOCAL_HTML},
            )
        except Exception as exc:  # noqa: BLE001 — 跨域/销毁中的 frame 可单独跳过
            detail = {"forms": [], "controls": [], "captcha_candidates": [], "local_html": "", "error": str(exc)[:300]}
        frames.append({"ref": frame_ref, "parent": parent_ref, "name": frame.name, "url": frame.url, **detail})
    remaining_html = _STRUCTURE_MAX_LOCAL_HTML
    for frame in frames:
        local_html = str(frame.get("local_html") or "")[:remaining_html]
        frame["local_html"] = local_html
        remaining_html = max(0, remaining_html - len(local_html))
    return {"version": 1, "frames": frames}


def _resource_ext(mime: str, kind: str = "") -> str:
    """按 MIME 推导资源文件扩展名；MIME 缺失时按资源种类兜底，最后才落 txt。

    回补路径拿到的 ``Content-Type`` 可能是通用的 ``application/octet-stream``，
    此时用枚举阶段已知的 script/stylesheet 种类仍能给出可用的扩展名。
    """
    key = (mime or "").split(";")[0].strip().lower()
    ext = _RESOURCE_EXT_BY_MIME.get(key)
    if ext:
        return ext
    if kind == "script":
        return "js"
    if kind == "stylesheet":
        return "css"
    return "txt"


def _url_scheme_variants(url: str) -> list[str]:
    """派生同一资源的 URL 形态：绝对 https/http 与协议相对 `//host/path`。

    DOM 属性里常见协议相对写法（`//s1.example.com/x.js`），而 CDP 资源树
    上报绝对 URL，逐形态替换才能把引用全部改写到本地文件。
    绝对形态在前、协议相对在后：先把长的替换掉，剩余的 `//` 前缀才是
    真正的协议相对用法（避免 `http://` 内含 `//` 被二次误替换）。
    """
    if url.startswith("https://"):
        rest = url[len("https://") :]
        return [url, f"http://{rest}", f"//{rest}"]
    if url.startswith("http://"):
        rest = url[len("http://") :]
        return [url, f"https://{rest}", f"//{rest}"]
    return [url]


def _rewrite_resource_urls(html: str, mapping: dict[str, str]) -> str:
    """把 HTML 中出现的资源 URL 改写为本地相对路径。

    URL 在 HTML 属性里可能是原样形态，也可能是 `&amp;` 转义形态，两种都替换；
    简单字符串替换可能误伤 JS 字符串中的同 URL 文本，对离线还原无实际影响。
    """
    for url, local in mapping.items():
        if not url:
            continue
        for variant in _url_scheme_variants(url):
            html = html.replace(variant, local)
            escaped = _html_escape(variant, quote=True)
            if escaped != variant:
                html = html.replace(escaped, local)
    return html


class _ResourceSink:
    """资源落盘收集器：CDP 快照与 HTTP 回补共用同一套上限与命名规则。

    文件名单调哈希命名（URL sha1 前 12 位 + 扩展名），两条路径写到同一目录也
    不会互相覆盖；总量/文件数上限在收集器内统一判定，避免某条路径绕过预算
    把捕获包撑爆。
    """

    def __init__(self, target_dir: Path) -> None:
        self.target_dir = target_dir
        self.saved: dict[str, str] = {}
        #: 资源在 HTML 里的原始书写形态 → 本地路径（改写引用时按文本替换）
        self.aliases: dict[str, str] = {}
        self.total_bytes = 0
        #: 触发上限时的说明；非 None 即表示后续资源不再收录
        self.note: str | None = None

    def admit(self) -> bool:
        """是否还能继续收录；触顶后统一返回 False（说明只在首次触顶时写入）。"""
        if self.note is not None:
            return False
        if len(self.saved) >= _RESOURCE_MAX_FILES:
            self.note = f"资源数超过 {_RESOURCE_MAX_FILES}，其余跳过"
            return False
        return True

    def alias(self, url: str, raw: str) -> None:
        """登记资源在 HTML 里的原始书写形态（内容可能已由 CDP 路径落盘）。

        枚举阶段必须无条件登记：CDP 已拿到正文的资源不会再走回补，其相对写法
        只能在这里补上，否则离线副本改不动引用。
        """
        local = self.saved.get(url)
        if local and raw and raw != url:
            self.aliases.setdefault(raw, local)

    def store(
        self,
        url: str,
        data: bytes,
        mime: str,
        kind: str = "",
        aliases: Iterable[str] = (),
    ) -> bool:
        """写入单个资源并建映射；空内容/超单文件/超总量返回 False。

        ``aliases`` 是同一资源在 HTML 文本里的其它书写形态（相对路径、
        协议相对等）。改写 HTML 时按文本替换，只有绝对 URL 是匹配不上的。
        """
        if not data or len(data) > _RESOURCE_MAX_BYTES:
            return False
        if self.total_bytes + len(data) > _RESOURCE_MAX_TOTAL_BYTES:
            if self.note is None:
                self.note = f"资源总量超 {_RESOURCE_MAX_TOTAL_BYTES // (1024 * 1024)}MiB，已截断"
            return False
        name = (
            f"{hashlib.sha1(url.encode('utf-8')).hexdigest()[:12]}"
            f".{_resource_ext(mime, kind)}"
        )
        self.target_dir.mkdir(parents=True, exist_ok=True)
        (self.target_dir / name).write_bytes(data)
        local = f"resources/{name}"
        self.saved[url] = local
        for alias in aliases:
            if alias and alias != url:
                self.aliases.setdefault(alias, local)
        self.total_bytes += len(data)
        return True


def _iter_cdp_frame_resources(frame_tree: Any) -> list[tuple[str, list]]:
    """递归收集 CDP frameTree 中各 frame 的 (frameId, resources) 对。

    资源树以主 frame 为根、``childFrames`` 挂子 frame；iframe 门户（登录表单
    全在 iframe 里）的 CSS/JS 都在子 frame 的 ``resources`` 里，只取根节点
    会让离线副本缺资源。畸形节点（非 dict / 缺 id）容错跳过。
    """
    out: list[tuple[str, list]] = []
    if not isinstance(frame_tree, dict):
        return out
    frame_id = str((frame_tree.get("frame") or {}).get("id") or "")
    entries = frame_tree.get("resources") or []
    if isinstance(entries, list):
        out.append((frame_id, entries))
    for child in frame_tree.get("childFrames") or []:
        out.extend(_iter_cdp_frame_resources(child))
    return out


async def _cdp_resource_snapshot(page: Any, sink: _ResourceSink) -> None:
    """经 CDP 抓取全部 frame（含 iframe）已加载的 Script/Stylesheet 资源并写入 sink。

    逐项容错：缓存已逐出/取回失败的单个资源跳过，不中断整体快照。CDP 内容与
    页面实际执行的版本一致，优先级高于 HTTP 回补，故先跑这条路径。
    """
    cdp = await page.context.new_cdp_session(page)
    try:
        # getResourceContent 要求本会话启用 Page 域（Playwright 的 CDP 会话
        # 不会自动启用，缺省时报 "Agent is not enabled"）
        await cdp.send("Page.enable")
        tree = await cdp.send("Page.getResourceTree")
        for frame_id, entries in _iter_cdp_frame_resources(tree.get("frameTree", {})):
            for res in entries:
                if not sink.admit():
                    return
                if not isinstance(res, dict):
                    continue
                rtype = (res.get("type") or "").lower()
                if rtype not in ("stylesheet", "script"):
                    continue
                url = res.get("url") or ""
                if not url.startswith(("http://", "https://")) or url in sink.saved:
                    continue
                try:
                    got = await cdp.send(
                        "Page.getResourceContent", {"frameId": frame_id, "url": url}
                    )
                except Exception:  # noqa: BLE001 — 缓存逐出等，逐项跳过
                    continue
                if got.get("base64Encoded"):
                    data = base64.b64decode(got.get("content") or "")
                else:
                    data = (got.get("content") or "").encode("utf-8")
                sink.store(url, data, res.get("mimeType") or "", rtype)
    finally:
        await cdp.detach()


#: 页面内枚举已加载的 script/stylesheet URL（引擎无关，不依赖 CDP）
#:
#: 两条来源互补：DOM 属性覆盖静态引用（含尚未进入缓存的），Performance 资源
#: 条目覆盖导航后动态插入/预加载的请求。返回 [绝对URL, 属性的原始书写形态, 种类]
#: 三元组：原始形态用于把 HTML 里的引用改成 resources/ 相对路径——HTML 文本里常见
#: 相对写法（`static/css/style.css`、`/css/a.css`），只有绝对 URL 是替换不掉的。
_PAGE_RESOURCE_PROBE_JS = r"""
() => {
  const out = [];
  const seen = new Set();
  const push = (url, raw, kind) => {
    if (!url || url.startsWith('data:') || seen.has(url)) return;
    seen.add(url);
    out.push([url, raw || '', kind]);
  };
  for (const el of document.querySelectorAll('script[src]')) {
    push(el.src, el.getAttribute('src'), 'script');
  }
  for (const el of document.querySelectorAll('link[href]')) {
    const rel = (el.getAttribute('rel') || '').toLowerCase();
    const as = (el.getAttribute('as') || '').toLowerCase();
    if (rel.includes('stylesheet') || (rel.includes('preload') && as === 'style')) {
      push(el.href, el.getAttribute('href'), 'stylesheet');
    }
  }
  try {
    for (const entry of performance.getEntriesByType('resource')) {
      const type = (entry.initiatorType || '').toLowerCase();
      if (type === 'script') push(entry.name, '', 'script');
      else if (type === 'link' || type === 'css' || type === 'style') push(entry.name, '', 'stylesheet');
    }
  } catch (_) {}
  return out;
}
"""


async def _http_resource_snapshot(page: Any, sink: _ResourceSink) -> str | None:
    """引擎无关兜底：逐 frame 枚举已加载的 script/stylesheet，按 URL 回补正文。

    CDP 不可用（firefox / webkit）时这是唯一能拿到 CSS/JS 正文的路径；Chromium
    下作为补充，捞 CDP 内存缓存已逐出或导航后才插入的资源。按 ``page.frames``
    逐 frame 执行枚举脚本并按 URL 去重汇总，iframe 门户的资源才能进离线副本；
    单个 frame（跨域/销毁中）枚举失败跳过，全部失败才报枚举不可用。
    ``context.request`` 走浏览器网络栈（带 context 的 cookie / UA），逐项容错，
    返回说明文本或 None。
    """
    # 按 URL 去重汇总各 frame 的枚举结果（首见优先，保留原始书写形态）
    entries_all: list[Any] = []
    seen: set[str] = set()
    first_error = ""
    frames = list(getattr(page, "frames", None) or [])
    if not frames:
        # 测试替身兼容：无 frames 属性时按单页处理
        frames = [page]
    for frame in frames:
        try:
            entries = await frame.evaluate(_PAGE_RESOURCE_PROBE_JS)
        except Exception as exc:  # noqa: BLE001 — 跨域/销毁中的 frame 单独跳过
            if not first_error:
                first_error = str(exc)
            continue
        for entry in entries or []:
            if not isinstance(entry, (list, tuple)) or not entry:
                continue
            url = str(entry[0] or "")
            if not url or url in seen:
                continue
            seen.add(url)
            entries_all.append(entry)
    if not entries_all and first_error:
        # 所有 frame 枚举都失败，本兜底整体不可用
        return f"页面资源枚举失败: {first_error}"
    fetched = 0
    failed = 0
    first_fetch_error = ""
    for entry in entries_all:
        if not sink.admit():
            break
        url = str(entry[0] or "")
        raw = str(entry[1] or "") if len(entry) > 1 else ""
        kind = str(entry[2] or "") if len(entry) > 2 else ""
        if not url.startswith(("http://", "https://")):
            continue
        if url in sink.saved:
            # 正文已有（CDP 路径或更早的条目）：只补登记 HTML 里的原始写法
            sink.alias(url, raw)
            continue
        try:
            resp = await page.context.request.get(url, timeout=_RESOURCE_FETCH_TIMEOUT_MS)
            if not resp.ok:
                failed += 1
                if not first_fetch_error:
                    first_fetch_error = f"HTTP {resp.status}"
                continue
            data = await resp.body()
            mime = resp.headers.get("content-type", "")
        except Exception as exc:  # noqa: BLE001 — 单个资源取回失败不中断整体
            failed += 1
            if not first_fetch_error:
                first_fetch_error = str(exc)[:120]
            continue
        if sink.store(url, data, mime, kind, aliases=[raw]):
            fetched += 1
    if failed:
        return f"{fetched} 个资源经 HTTP 回补成功，{failed} 个失败（首个原因: {first_fetch_error}）"
    return None


async def _capture_page_resources(
    page: Any, target_dir: Path, *, bs: dict
) -> tuple[dict[str, str], dict[str, str], str | None]:
    """抓取页面 CSS/JS 资源并落盘到 target_dir。

    返回 ``(saved, aliases, note)``：``saved`` 为 绝对URL → resources/<name>
    映射，``aliases`` 为资源在 HTML 里的原始书写形态 → 同一路径，两者合并后交给
    ``_rewrite_resource_urls`` 才能把 HTML 引用全部改到本地（相对路径写法只能靠
    aliases 命中）。``note`` 为降级/截断说明或 None。

    两条抓取路径：CDP（Chromium 系，内容与执行时一致）优先，页面枚举 + HTTP 回补
    （引擎无关）兜底补缺。非 Chromium 渠道 CDP 在协议层就不存在，此处跳过并按渠道
    给出可操作说明，而不是把 Playwright 的英文异常直接甩给用户。
    """
    sink = _ResourceSink(target_dir)
    notes: list[str] = []
    channel = str((bs or {}).get("browser_channel") or "playwright").strip().lower()
    if _channel_supports_cdp(bs):
        try:
            await _cdp_resource_snapshot(page, sink)
        except Exception as exc:  # noqa: BLE001 — 回退 HTTP 回补
            notes.append(f"CDP 资源快照不可用: {exc}")
    else:
        notes.append(
            f"当前浏览器渠道 {channel} 不支持 CDP：MHTML 完整布局快照不可用，"
            "CSS/JS 已改用页面枚举 + HTTP 回补抓取；需要完整快照请把浏览器渠道"
            "切换为 Chromium / Chrome / Edge 后重新捕获"
        )
    try:
        fallback_note = await _http_resource_snapshot(page, sink)
    except Exception as exc:  # noqa: BLE001 — 兜底整体失败不影响已落盘的 CDP 产物
        fallback_note = f"资源回补失败: {exc}"
    if fallback_note:
        notes.append(fallback_note)
    if sink.note:
        notes.append(sink.note)
    return sink.saved, sink.aliases, "；".join(notes) if notes else None


# ── 取消注册表（跨线程安全）──


class CancelRegistry:
    """cancel_id → threading.Event 的线程安全映射。

    支持"取消先于注册到达"的场景：若 cancel 到达时对应事件尚未注册，
    记录为 pending，注册时立即置位。
    """

    def __init__(self) -> None:
        self._lock = threading.Lock()
        self._events: dict[str, threading.Event] = {}
        self._pending: set[str] = set()

    # pending 集合上限：限制从未被注册的 cancel_id 永久堆积（历史遗留 F10）
    _MAX_PENDING = 256

    def register(self, cancel_id: str) -> threading.Event:
        """注册 cancel_id，返回对应的 Event（若此前已触发取消则立即置位）。"""
        ev = threading.Event()
        with self._lock:
            if cancel_id in self._pending:
                self._pending.discard(cancel_id)
                ev.set()
            self._events[cancel_id] = ev
        return ev

    def trigger(self, cancel_id: str) -> None:
        """触发取消：置位对应 Event，或记录 pending。"""
        with self._lock:
            ev = self._events.get(cancel_id)
            if ev is not None:
                ev.set()
            else:
                # 记录 pending 以支持“取消先于注册到达”；若某些 cancel_id 从未被注册
                # （请求已完成），达到上限时清空防止永久堆积（历史遗留 F10）
                if len(self._pending) >= self._MAX_PENDING:
                    self._pending.clear()
                self._pending.add(cancel_id)

    def unregister(self, cancel_id: str) -> None:
        """清理 cancel_id 注册项。"""
        with self._lock:
            self._events.pop(cancel_id, None)
            self._pending.discard(cancel_id)


# ── Worker 核心 ──


# ── 常驻与空闲回收策略 ──

#: 浏览器空闲自动释放秒数：worker.keep_alive 未启用时，浏览器类命令完成后若
#: 持续无新命令，超过该时长即全量关闭浏览器释放内存（Worker 进程本身仍由
#: Rust 侧 worker.idle_timeout_seconds 管理）。连续任务不受影响：每次新命令
#: 都会取消并重置计时。
BROWSER_IDLE_RELEASE_SECS = 30

# 普通任务截图为 WebSocket 异步回读预留时间；启动时仍会清理异常退出残留。
TASK_SCREENSHOT_RETENTION_SECS = 30

# 调试步骤补拍截图的超时（毫秒）。截图只是给调试面板看的预览，超时即放弃：
# 卡住的页面不得让 debug_step / debug_run_all 的命令响应无限期等待。
DEBUG_FRAME_TIMEOUT_MS = 5000

#: Rust spawn 时按 cfg.worker.keep_alive 注入（改配置对下一个 Worker 生命周期
#: 生效）。True 时浏览器跨会话常驻：登录成功整页保留登录状态（门户页 JS 心跳
#: 不中断），非成功终态仅会话级释放，且不装浏览器空闲回收计时器。
_WORKER_KEEP_ALIVE = os.environ.get("CAMPUS_AUTH_WORKER_KEEP_ALIVE", "").strip().lower() in (
    "1",
    "true",
    "yes",
)


class WorkerCore:
    """管理 Playwright 浏览器实例生命周期，并分发浏览器动作命令。"""

    # 仅 Chromium 系支持的启动参数
    _CHROMIUM_ONLY_FLAGS = {
        "--no-sandbox",
        "--disable-dev-shm-usage",
        "--disable-gpu",
        "--memory-pressure-off",
        "--disable-web-security",
    }

    # 安全敏感参数黑名单（用户自定义 browser_args 中不允许出现）
    _BLOCKED_BROWSER_ARGS = {
        "--remote-debugging-port",
        "--remote-debugging-address",
        "--user-data-dir",
        "--load-extension",
        "--disable-extensions-except",
        "--enable-automation",
        "--remote-allow-origins",
        "--proxy-server",
        "--proxy-bypass-list",
    }

    def __init__(self) -> None:
        self._playwright: Any = None
        self._browser: Any = None
        self._context: Any = None
        self._page: Any = None
        self._last_browser_settings: dict | None = None
        self._debug_sessions: dict[str, DebugSession] = {}
        self.emit: Callable[[str, dict], None] = lambda event_type, data: None
        self.shutdown_event: threading.Event | None = None
        # 当前任务期间捕获的页面弹窗文案（_run_task 期间重置，随 StructuredResult 上报）
        self._task_dialogs: list[str] = []
        # 当前会话类型（login / debug）：注入 step_progress 事件，供前端区分
        # 登录会话与调试会话的步骤进度（登录步骤不应污染调试面板）
        self._session_type: str = "login"
        # 运行时能力（任务 10）：由 worker_main._serve 注入（OCR 预加载探测结果），
        # 随 worker_health_check 响应上报；未注入时为空 dict（Rust 侧回退文件探测）
        self.capabilities: dict[str, bool] = {}
        # 浏览器空闲自动释放计时（keep_alive 关闭时武装，见 _arm_browser_idle_release）
        self._browser_idle_task: asyncio.Task | None = None
        # 延迟删除普通任务截图，确保 Rust WebSocket 有时间完成异步回读。
        self._screenshot_cleanup_tasks: set[asyncio.Task] = set()
        # 防止同一个 Page 重复绑定 dialog 处理器。
        self._wired_page_ids: set[int] = set()

    # ── 浏览器启动参数构建 ──

    def _build_launch_args(self, bs: dict, channel: str = "playwright") -> list[str]:
        """构建浏览器启动参数；非 Chromium 引擎过滤 Chromium-only 参数。"""
        is_chromium = self._is_chromium_channel(bs, channel)

        args: list[str] = []
        if is_chromium:
            args.extend(
                [
                    "--no-sandbox",
                    "--disable-dev-shm-usage",
                    "--disable-gpu",
                    "--memory-pressure-off",
                    # 默认反检测：去掉 Blink 的自动化标记（无副作用，其余推荐参数见前端按钮）
                    "--disable-blink-features=AutomationControlled",
                ]
            )
            if bs.get("disable_web_security", False):
                args.append("--disable-web-security")
            if bs.get("low_resource_mode", False):
                args.append("--blink-settings=imagesEnabled=false")

        custom_args = str(bs.get("browser_args", "") or "").strip()
        if custom_args:
            for flag in custom_args.splitlines():
                flag = flag.strip()
                if not flag or flag.startswith("#"):
                    continue
                if not is_chromium and flag in self._CHROMIUM_ONLY_FLAGS:
                    continue
                flag_name = flag.split("=", 1)[0]
                if flag_name in self._BLOCKED_BROWSER_ARGS:
                    logger.warning(f"已过滤安全敏感浏览器参数: {flag_name}")
                    continue
                if flag not in args:
                    args.append(flag)
        return args

    def _build_context_options(self, bs: dict) -> dict[str, Any]:
        """构建浏览器上下文选项。"""
        ctx_opts: dict[str, Any] = {
            "viewport": {
                "width": int(bs.get("viewport_width", 1280)),
                "height": int(bs.get("viewport_height", 720)),
            },
            "locale": bs.get("locale", "zh-CN"),
            "timezone_id": bs.get("timezone_id", "Asia/Shanghai"),
            "has_touch": False,
            "color_scheme": "light",
            "ignore_https_errors": bs.get("ignore_https_errors", True),
        }
        ua = (bs.get("user_agent") or "").strip()
        if ua:
            ctx_opts["user_agent"] = ua
        extra_headers = self._get_extra_http_headers(bs)
        if extra_headers:
            ctx_opts["extra_http_headers"] = extra_headers
        if bs.get("bind_proxy"):
            ctx_opts["proxy"] = {"server": bs["bind_proxy"]}
        return ctx_opts

    def _get_extra_http_headers(self, bs: dict) -> dict[str, str]:
        """解析自定义 HTTP 请求头（extra_headers_json）。"""
        raw = str(bs.get("extra_headers_json", "") or "").strip()
        if not raw:
            return {}
        try:
            headers = json.loads(raw)
            if isinstance(headers, dict):
                result: dict[str, str] = {}
                blocked = {
                    "cookie", "authorization", "proxy-authorization",
                    "proxy-authenticate", "host", "content-length",
                    "transfer-encoding", "connection",
                }
                for k, v in headers.items():
                    if k is None:
                        continue
                    k_str, v_str = str(k), str(v)
                    if len(k_str) > 256 or len(v_str) > 4096:
                        logger.warning(f"请求头过长，已跳过: {k_str[:32]}")
                        continue
                    if "\r" in k_str or "\n" in k_str or "\r" in v_str or "\n" in v_str:
                        logger.warning(f"请求头含换行符，已跳过: {k_str[:32]}")
                        continue
                    if k_str.strip().lower() in blocked:
                        logger.warning(f"敏感请求头已拒绝: {k_str[:32]}")
                        continue
                    result[k_str] = v_str
                return result
            logger.warning("自定义请求头格式无效: 应为 JSON 对象，已忽略")
        except Exception as exc:  # noqa: BLE001
            logger.warning(f"解析自定义请求头失败: {exc}")
        return {}

    def _resolve_launcher(
        self,
        playwright: Any,
        channel: str,
        custom_path: str,
        bs: dict | None = None,
    ) -> tuple[Any, str | None]:
        """根据 channel 解析对应的 launcher 对象。"""
        if channel == "custom":
            if not custom_path or not Path(custom_path).is_file():
                raise FileNotFoundError(f"自定义浏览器可执行文件不存在: {custom_path}")
            settings = bs if bs is not None else (self._last_browser_settings or {})
            engine = settings.get("custom_browser_engine", "auto")
            engine = engine if engine in ("firefox", "webkit") else "chromium"
            return getattr(playwright, engine), custom_path
        if channel == "firefox":
            return playwright.firefox, None
        if channel == "webkit":
            return playwright.webkit, None
        # playwright / chromium / msedge / chrome 走 Chromium launcher。
        return playwright.chromium, None

    @staticmethod
    def _is_chromium_channel(bs: dict, channel: str) -> bool:
        """是否为 Chromium 系（含 custom 路径配 Chromium 引擎；firefox/webkit 排除）。

        与资源快照的 CDP 可用性判定同源（``_channel_supports_cdp``），避免启动
        参数过滤与捕获路径出现两套口径。
        """
        return _channel_supports_cdp({**(bs or {}), "browser_channel": channel})

    async def _launch_browser(
        self,
        playwright: Any,
        channel: str,
        custom_path: str,
        headless: bool,
        launch_args: list[str],
        bs: dict | None = None,
    ) -> Any:
        """启动非持久化浏览器。"""
        launcher, resolved_path = self._resolve_launcher(playwright, channel, custom_path, bs)
        kwargs: dict[str, Any] = {"headless": headless, "args": launch_args}
        # 去掉 Playwright 默认的 --enable-automation（消除 automation 痕迹；
        # 用户侧仍被黑名单拦截，仅后端内置可配；仅 Chromium 支持该参数）
        if self._is_chromium_channel(bs or {}, channel):
            kwargs["ignore_default_args"] = ["--enable-automation"]
        if resolved_path:
            kwargs["executable_path"] = resolved_path
        elif channel in ("msedge", "chrome"):
            kwargs["channel"] = channel
        return await launcher.launch(**kwargs)

    async def _launch_persistent_context(
        self,
        playwright: Any,
        channel: str,
        custom_path: str,
        headless: bool,
        launch_args: list[str],
        user_data_dir: str,
        ctx_opts: dict[str, Any],
        bs: dict | None = None,
    ) -> Any:
        """启动持久化上下文浏览器（保留 cookies）。"""
        launcher, resolved_path = self._resolve_launcher(playwright, channel, custom_path, bs)
        kwargs: dict[str, Any] = {"headless": headless, "args": launch_args, **ctx_opts}
        if self._is_chromium_channel(bs or {}, channel):
            kwargs["ignore_default_args"] = ["--enable-automation"]
        if resolved_path:
            kwargs["executable_path"] = resolved_path
        elif channel in ("msedge", "chrome"):
            kwargs["channel"] = channel
        return await launcher.launch_persistent_context(user_data_dir, **kwargs)

    async def _apply_stealth_and_routes(self, bs: dict) -> None:
        """应用反检测脚本和路由拦截。"""
        if self._context is None:
            return
        if bs.get("low_resource_mode", False):
            await self._context.route("**/*", self._handle_low_resource_request)
        if bs.get("stealth_mode", False):
            # 显式 null 时 .get 默认值不生效，需 or 兜底再 strip
            custom = (bs.get("stealth_custom_script") or "").strip()
            script = custom or _STEALTH_INIT_SCRIPT
            await self._context.add_init_script(script)

    async def _start_browser(self, config: dict) -> None:
        """启动浏览器（按 browser_channel 选择引擎）。"""
        from playwright.async_api import async_playwright

        bs = config.get("browser_settings", {})
        self._last_browser_settings = bs
        headless = bs.get("headless", True)
        pure_mode = bs.get("pure_mode", False)
        channel = str(bs.get("browser_channel") or "playwright").strip().lower()
        custom_path = str(bs.get("browser_custom_path") or "").strip()

        if self._playwright is None:
            self._playwright = await async_playwright().start()

        persistent = bs.get("persistent_context", False)
        try:
            if persistent:
                if channel == "custom":
                    raw_engine = (bs.get("custom_browser_engine") or "auto").strip().lower()
                    engine = raw_engine if raw_engine in ("firefox", "webkit") else "chromium"
                    path_key = hashlib.sha256(
                        str(Path(custom_path).resolve()).encode("utf-8")
                    ).hexdigest()[:12]
                    data_key = f"custom-{engine}-{path_key}"
                else:
                    data_key = channel
                user_data_dir = _browser_data_dir() / data_key
                user_data_dir.mkdir(parents=True, exist_ok=True)
                launch_args = [] if pure_mode else self._build_launch_args(bs, channel)
                ctx_opts = self._build_context_options(bs)
                self._context = await self._launch_persistent_context(
                    self._playwright, channel, custom_path, headless,
                    launch_args, str(user_data_dir), ctx_opts, bs,
                )
                self._wire_context()
                self._browser = None
                if not pure_mode:
                    await self._apply_stealth_and_routes(bs)
            elif pure_mode:
                self._browser = await self._launch_browser(
                    self._playwright, channel, custom_path, headless, [], bs
                )
                # pure mode 只禁用额外启动参数、stealth 与资源路由；
                # locale/timezone/UA/header/proxy 等 BrowserContext 契约仍应一致生效。
                ctx_opts = self._build_context_options(bs)
                self._context = await self._browser.new_context(**ctx_opts)
                self._wire_context()
            else:
                launch_args = self._build_launch_args(bs, channel)
                self._browser = await self._launch_browser(
                    self._playwright, channel, custom_path, headless, launch_args, bs
                )
                ctx_opts = self._build_context_options(bs)
                self._context = await self._browser.new_context(**ctx_opts)
                self._wire_context()
                await self._apply_stealth_and_routes(bs)

            self._page = await self._new_page()
            logger.info(
                "浏览器已启动: channel=%s, headless=%s, persistent=%s",
                channel,
                headless,
                persistent,
            )
        except Exception:  # noqa: BLE001 — 启动环节异常源众多（driver 崩溃/参数非法等），统一回滚后原样上抛
            logger.error("浏览器启动失败，回滚资源", exc_info=True)
            await self.close_browser()
            raise

    def _wire_page(self, page: Any) -> None:
        """为上下文创建的每个页面注册统一弹窗处理器。"""
        page_id = id(page)
        if page_id in self._wired_page_ids:
            return
        on = getattr(page, "on", None)
        if callable(on):
            on("dialog", lambda d: asyncio.ensure_future(self._handle_page_dialog(d)))
        self._wired_page_ids.add(page_id)

    def _on_context_page(self, page: Any) -> None:
        """接管门户打开的新标签页/弹窗，供后续步骤继续执行。"""
        self._wire_page(page)
        self._page = page

    def _wire_context(self) -> None:
        """监听 BrowserContext 的新增页面事件。"""
        if self._context is not None:
            on = getattr(self._context, "on", None)
            if callable(on):
                on("page", self._on_context_page)

    async def _new_page(self) -> Any:
        """创建新页面并注册防残留 dialog 处理器（B5 修正）。

        页面上的 alert/confirm 若不处理会阻塞后续导航与页面加载；注册 accept
        处理器使残留对话框自动点“确定”继续，避免卡死后续任务。
        用 accept 而非 dismiss：登录成功等业务弹窗预期向下确认，dismiss 会
        取消流程导致登录判定失败（历史遗留：误拦截登录成功弹窗）。
        顺带把弹窗文案通过 ``dialog`` 事件推给前端，使被吞掉的“登录成功！”
        等提示能在前端日志/通知中显示出来。
        """
        page = await self._context.new_page()
        self._wire_page(page)
        return page

    async def _prepare_session_page(self) -> Any:
        """创建会话隔离的顶层 Page，同时保留 BrowserContext 级 Cookie。

        会话隔离的关键入口，任务/调试会话开始前必须先经此建页：
        - 单活跃页语义：先关闭全部旧页/恢复页，阻断上一会话的
          sessionStorage 与后台脚本渗入新会话；
        - 非持久化上下文在首个文档加载时清空本地/会话存储；持久化上下文只清
          sessionStorage，保留 localStorage 登录态；
        - Cookie 挂在 BrowserContext 上，不受换页影响，登录态得以跨会话保留。

        无参数；成功返回已就绪的新 Page 并将其设为当前 ``self._page``，
        BrowserContext 未初始化时抛出 ``WorkerError``。
        """
        if self._context is None:
            raise WorkerError(Outcome.UNKNOWN_ERROR, "Browser context is not initialized")

        # Worker 采用单活跃页语义：关闭所有旧页/恢复页，
        # 防止其 sessionStorage 与后台脚本渗入新会话。
        try:
            old_pages = list(self._context.pages)
        except Exception as exc:  # noqa: BLE001
            logger.debug("列举旧页面失败，降级为当前页: %s", exc)
            old_pages = [self._page] if self._page is not None else []
        for old_page in old_pages:
            await self._safe_close(old_page, "old session page")
        # 旧 Page 全部关闭后再清空身份集合，避免 Python 复用对象 id 时漏绑新页事件。
        self._wired_page_ids.clear()
        self._page = None

        page = await self._new_page()
        try:
            persistent = bool((self._last_browser_settings or {}).get("persistent_context", False))
            isolation_script = (
                _PERSISTENT_SESSION_STORAGE_ISOLATION_SCRIPT
                if persistent
                else _TASK_STORAGE_ISOLATION_SCRIPT
            )
            await page.add_init_script(isolation_script)
        except Exception:  # noqa: BLE001 — init 脚本注入失败时回收刚建的页面，异常原样上抛由上层归类
            await self._safe_close(page, "isolated page")
            raise
        self._page = page
        return page

    async def _handle_page_dialog(self, dialog) -> None:
        """处理页面原生弹窗：自动确认并把弹窗文案推给前端。"""
        try:
            message = getattr(dialog, "message", None)
            if message:
                msg = str(message)
                self.emit("dialog", {"message": msg, "action": "accept"})
                # 收集进当前任务的弹窗列表（限长防泄漏），随 StructuredResult 上报，
                # 使“账号或密码错误”等页面提示能进入 Rust 侧登录日志
                if len(self._task_dialogs) < 20:
                    self._task_dialogs.append(msg)
            await dialog.accept()
        except Exception as exc:  # noqa: BLE001
            # 弹窗可能在操作间隙已消失，忽略即可，不影响主流程
            logger.debug(f"处理页面弹窗异常（忽略）: {exc}")

    async def ensure_browser(self, config: dict) -> None:
        """确保浏览器就绪（复用已存在的实例，仅在未就绪或配置变更时重建）。"""
        bs = config.get("browser_settings", {})
        # 任何浏览器命令开始都取消待触发的空闲自动释放
        self._cancel_browser_idle_release()
        # 会话级释放后的热恢复：浏览器进程存活且配置未变，仅重建轻量 context
        # 与页面（全新 cookie 罐，会话隔离语义不变），省去 1-3s 冷启动
        if (
            self._browser is not None
            and self._context is None
            and self._last_browser_settings == bs
            and self._browser.is_connected()
        ):
            try:
                # 旧 context 已随会话级释放关闭，其页面对象 id 可能被 Python
                # 复用：重建 context 前先清空防重复绑定集合，否则新页会被
                # 误判为"已绑定"而漏装 dialog 处理器（对齐 _close_session）
                self._wired_page_ids.clear()
                self._context = await self._browser.new_context(
                    **self._build_context_options(bs)
                )
                self._wire_context()
                if not bs.get("pure_mode", False):
                    await self._apply_stealth_and_routes(bs)
                self._page = await self._new_page()
                logger.info("复用浏览器进程热恢复会话上下文")
                return
            except Exception:  # noqa: BLE001 — 热恢复涉及建 context/装路由/建页多环节，任一失败统一回退完整重建
                logger.warning("浏览器进程热恢复失败，回退完整重建", exc_info=True)
                await self.close_browser()
        has_browser = self._browser is not None or self._context is not None
        if has_browser and await self._health_check() and self._last_browser_settings == bs:
            return
        logger.info("浏览器未就绪或配置变更，重建浏览器")
        await self.close_browser()
        await self._start_browser(config)

    async def _health_check(self) -> bool:
        """检查 Browser 与 BrowserContext 是否都仍可用。"""
        if self._context is None:
            return False
        try:
            # 非 persistent 模式下 Browser 进程在线不代表 context 仍然存活；
            # context 可能已被关闭，而 browser.is_connected() 依旧返回 True。
            if self._browser is not None and not self._browser.is_connected():
                return False
            # pages 只是本地对象快照，关闭后的 context 也可能还能读取；cookies()
            # 会走一次真实协议调用，可可靠暴露 "Target page/context closed"。
            await self._context.cookies()
            return True
        except Exception:  # noqa: BLE001 — 探活调用任何异常都代表 context 已不可用
            return False

    @staticmethod
    async def _safe_close(resource: Any, name: str) -> None:
        """安全关闭单个资源。"""
        if resource is None:
            return
        try:
            await resource.close()
        except Exception as exc:  # noqa: BLE001
            msg = str(exc).lower()
            if "target closed" in msg or "connection closed" in msg:
                logger.debug(f"关闭 {name} 时连接已断开（正常）")
            else:
                logger.error(f"关闭 {name} 异常: {exc}")

    async def close_browser(self) -> None:
        """关闭浏览器并释放所有资源（公开接口，供外部清理调用）。"""
        if self._page is not None:
            await self._safe_close(self._page, "页面")
            self._page = None
        if self._context is not None:
            await self._safe_close(self._context, "上下文")
            self._context = None
            self._wired_page_ids.clear()
        if self._browser is not None:
            await self._safe_close(self._browser, "浏览器")
            self._browser = None
        if self._playwright is not None:
            try:
                await self._playwright.stop()
            except Exception:  # noqa: BLE001
                logger.debug("停止 Playwright 时连接已断开（正常）")
            self._playwright = None
        # 覆盖 debug_stop 未被正确调用（EOF / shutdown 路径）的泄漏场景；
        # 截图与 cancel 注册统一走幂等 teardown，不能只清 map。
        self._teardown_all_debug_sessions()
        self._last_browser_settings = None
        logger.info("浏览器及资源已关闭")

    async def _close_session(self) -> None:
        """会话级释放：关闭页面与非持久化 context，保留浏览器进程供热复用。

        persistent_context 模式的 context 即浏览器本体（_browser 为 None），
        关闭 context 等于终止进程，故仅清页面，context 由既有 ensure_browser
        快速路径继续复用。_last_browser_settings 必须保留，热恢复分支依赖它
        判定"配置未变"。
        """
        if self._page is not None:
            await self._safe_close(self._page, "页面")
            self._page = None
        if self._context is not None and self._browser is not None:
            await self._safe_close(self._context, "上下文")
            self._context = None
        # 关页后清空防重复绑定集合：Python 复用对象 id 时，新页会被误判为
        # "已绑定"而漏装 dialog 处理器（与 close_browser / _prepare_session_page 同款）
        self._wired_page_ids.clear()
        # 与 close_browser 同款兜底：截图与 cancel 注册统一释放。
        self._teardown_all_debug_sessions()
        logger.info("会话级资源已释放（浏览器进程保留）")

    def _cancel_browser_idle_release(self) -> None:
        """取消待触发的浏览器空闲自动释放（任何新浏览器命令都会重置计时）。"""
        if self._browser_idle_task is not None:
            self._browser_idle_task.cancel()
            self._browser_idle_task = None

    def _arm_browser_idle_release(self) -> None:
        """武装浏览器空闲自动释放计时（worker.keep_alive 启用时不回收）。"""
        if _WORKER_KEEP_ALIVE:
            return
        self._cancel_browser_idle_release()
        self._browser_idle_task = asyncio.create_task(self._browser_idle_release())

    async def _browser_idle_release(self) -> None:
        """空闲到期后全量关闭浏览器释放内存（Worker 进程仍由 Rust 空闲超时管理）。"""
        try:
            await asyncio.sleep(BROWSER_IDLE_RELEASE_SECS)
        except asyncio.CancelledError:
            return
        self._browser_idle_task = None
        # 调试会话兜底保护：活跃调试期间绝不回收
        if self._debug_sessions:
            return
        if self._browser is None and self._context is None:
            return
        logger.info(
            "浏览器空闲 %ds，自动释放（worker.keep_alive 未启用）",
            BROWSER_IDLE_RELEASE_SECS,
        )
        try:
            await asyncio.wait_for(self.close_browser(), timeout=_WAIT_TIMEOUT_SECS)
        except asyncio.TimeoutError:
            logger.warning("浏览器空闲自动释放超时（8s），跳过")
        except Exception as exc:  # noqa: BLE001
            logger.debug(f"浏览器空闲自动释放异常（忽略）: {exc}")

    async def force_interrupt_pending(self) -> None:
        """强制中断可能挂起的 Playwright 操作：关闭当前页面以打断 CDP await。

        命令级超时自愈时调用：页面关闭会使挂起的 ``page.evaluate``/``goto`` 等
        以“目标已关闭”异常结束，从而让被取消的任务真正退出，避免残留任务占住
        浏览器资源。不关闭整个浏览器，避免影响后续轻量请求。
        """
        if self._page is not None:
            page = self._page
            await self._safe_close(page, "页面")
            self._page = None
            # 对齐步骤层 _on_page_lost 语义：依赖该页的调试会话一并结束，
            # 否则僵尸会话持续占用"单会话"槽位，登录/任务被 B3 守卫持续拒绝
            for sid, session in list(self._debug_sessions.items()):
                if getattr(session, "page", None) is page:
                    self._debug_sessions.pop(sid, None)
                    self._teardown_debug_session(session)
                    logger.warning("调试会话 %s 因命令级超时强制中断页面而结束", sid)

    async def _handle_low_resource_request(self, route: Any) -> None:
        """低资源模式请求处理：拦截图片/字体/媒体。"""
        try:
            request = route.request
            if request.resource_type in {"image", "font", "media"}:
                await route.abort()
                return
            await route.continue_()
        except Exception as exc:  # noqa: BLE001
            logger.debug(f"路由异常已忽略: {exc}")

    # ── 上下文构建 ──

    def _make_context(
        self,
        page: Any,
        variables: dict[str, str],
        bs: dict,
        cancel_event: threading.Event | None,
        screenshot_dir: Path | None,
        task_config: TaskConfig,
    ) -> StepContext:
        """构造步骤执行上下文。"""
        session_type = self._session_type

        def _emit(event_type: str, data: dict) -> None:
            """给浏览器事件注入 session_type，供前端区分登录/调试会话。"""
            if event_type in {"step_progress", "screenshot", "dialog"} and isinstance(data, dict):
                data = dict(data)
                data["session_type"] = session_type
            self.emit(event_type, data)

        def _adopt_page(new_page: Any) -> None:
            self._wire_page(new_page)
            self._page = new_page

        return StepContext(
            page=page,
            variables=variables,
            cancel_event=cancel_event,
            screenshot_dir=screenshot_dir,
            default_timeout=_to_ms(bs, "timeout", 10000),
            navigation_timeout=_nav_timeout(bs),
            reveal_hidden=task_config.reveal_hidden,
            step_delay=task_config.step_delay,
            emit=_emit,
            on_page=_adopt_page,
        )

    async def _navigate(self, page: Any, url: str, nav_timeout: int) -> None:
        """导航到 URL，按异常消息细分错误分类（P7）。"""
        try:
            await page.goto(url, wait_until="domcontentloaded", timeout=nav_timeout)
        except Exception as exc:  # noqa: BLE001
            raise _classify_navigation_error(exc, url)

    @staticmethod
    async def _wait_after_navigation(task_config: TaskConfig, context: StepContext) -> None:
        """按任务配置在初始导航后额外等待，并保持取消可响应。"""
        if task_config.navigation_wait > 0:
            await _sleep_cancellable(task_config.navigation_wait, context)

    async def _run_task(
        self,
        task_config: TaskConfig,
        bs: dict,
        variables: dict[str, str],
        cancel_event: threading.Event | None,
        screenshot_dir: Path | None,
        navigate_url: str = "",
        nav_fallback_url: str = "",
    ) -> StructuredResult:
        """执行单个浏览器任务：确保浏览器 → 导航 → 运行步骤。

        ``navigate_url`` 为显式强制首导航（登录用它去触发地址，覆盖任务 url）；
        ``nav_fallback_url`` 仅在任务自身 url 解析为空时生效（回落 Profile 有效登录
        地址），用于避免任务没配起始地址时退化成空白页。
        """
        start = time.perf_counter()
        self._session_type = "login"
        context: StepContext | None = None
        try:
            # 有码任务在拉起浏览器前同步预热一次：主线程加载 ddddocr/numpy C 扩展
            # 入缓存，后续后台线程的分类识别只命中缓存，避免 Windows loader lock 卡 100s
            if any(s.step_type == "ocr" for s in (task_config.steps or [])):
                try:
                    from worker_main import _preload_ocr_deps  # noqa: WPS433

                    _preload_ocr_deps(force=True)
                except Exception:  # noqa: BLE001 — 预热 best-effort，失败不影响任务
                    pass
            await self.ensure_browser({"browser_settings": bs})
            await self._prepare_session_page()

            context = self._make_context(
                self._page, variables, bs, cancel_event, screenshot_dir, task_config
            )
            # 首导航三级取值：显式 navigate_url（登录强制去触发地址，口径不变）>
            # 任务自身 url > Profile 有效登录地址（回落）。判空必须在 resolve 之后：
            # 原实现先判 target 再 resolve，`{{LOGIN_URL}}` 解析成空串后会直接
            # page.goto("") 报无效 URL（直连渠道 + 认证地址留空即命中）。
            preferred = (navigate_url or "").strip()
            if preferred:
                target = resolve(preferred, variables).strip()
            else:
                target = _resolve_start_url(task_config.url, variables, nav_fallback_url)
            if target:
                nav_timeout = _nav_timeout(bs)
                # 全新 Page 让浏览器/上下文保持热态（免冷启动），同时强制存储隔离。
                await self._navigate(self._page, target, nav_timeout)
                await self._wait_after_navigation(task_config, context)

            result = await run_steps(self._page, task_config.steps, context)
            # success_condition 成功判定：声明变量名时，从 store_as 结果取变量真值判定，
            # 覆盖默认的"步骤全部成功即成功"兜底（对齐原项目 v4.2.3 _check_success）。
            # 必须先于 cookies 清理：真值判定未命中时最终 outcome 是失败，
            # 判定前清理会让该场景带着可疑登录态进入下一次重试
            var_name = (task_config.success_condition or "").strip()
            if var_name and result.outcome == Outcome.SUCCESS.value:
                if var_name not in context.results:
                    result = _build_result(
                        Outcome.UNKNOWN_ERROR,
                        f"成功条件变量未设置: {var_name}（请检查 eval 步骤的 store_as）",
                        context,
                        start,
                    )
                else:
                    value = context.results[var_name]
                    if not _is_truthy(value):
                        result = _build_result(
                            Outcome.UNKNOWN_ERROR,
                            f"成功条件未命中: {var_name}（真值判定不通过）",
                            context,
                            start,
                        )
                    else:
                        # 变量值可能含凭据（如 eval 提取的 token），只记真值判定结果，不打印 value
                        logger.info("[success_condition] 命中成功: 变量 %s 真值判定通过", var_name)
                        result.message = f"成功条件命中: {var_name}（真值判定通过）"
            # B5 取舍：任务失败后在共享 context 上清除 cookies，避免上次任务的残留会话
            # （登录态等）污染下一个任务。不重建整个页面/浏览器——那会显著增加下一次
            # 任务的重启开销；在现有 context 复用结构下，清除 cookies 已覆盖绝大多数
            # 跨任务污染场景（登录态隔离）。重试同任务由 Rust 侧重新调用，_run_task
            # 顶部经 _prepare_session_page 关闭全部旧页并新建会话页（无 reload 复用），
            # 不受此处影响。
            # 按**最终** outcome 清理：success_condition 未命中也是失败，同样需要隔离
            if result.outcome not in (Outcome.SUCCESS.value, Outcome.CANCELLED.value):
                if self._context is not None:
                    try:
                        await self._context.clear_cookies()
                        logger.info("[_run_task] 任务失败，已清除 context cookies")
                    except Exception as exc:  # noqa: BLE001
                        logger.debug(f"[_run_task] 清除 cookies 失败（忽略）: {exc}")
            return result
        except WorkerError as exc:
            if exc.outcome != Outcome.CANCELLED and self._context is not None:
                try:
                    await self._context.clear_cookies()
                    logger.info("[_run_task] 任务异常，已清除 context cookies")
                except Exception as clear_exc:  # noqa: BLE001
                    logger.debug(f"[_run_task] 清除 cookies 失败（忽略）: {clear_exc}")
            raise
        except Exception:
            if self._context is not None:
                try:
                    await self._context.clear_cookies()
                    logger.info("[_run_task] 任务异常，已清除 context cookies")
                except Exception as clear_exc:  # noqa: BLE001
                    logger.debug(f"[_run_task] 清除 cookies 失败（忽略）: {clear_exc}")
            raise
        finally:
            # WebSocket 会在收到事件后异步回读文件并内联图片；延迟清理避免事件
            # 已广播但 Rust 尚未读盘时文件先被删除。异常退出残留由启动清理兜底。
            if context is not None:
                self._defer_task_screenshot_cleanup(context)

    # ── 命令处理器 ──

    @asynccontextmanager
    async def _cancel_session(self, params: dict) -> AsyncIterator[tuple]:
        """一次性命令的公共 setup：注册 cancel_id、解析 TaskConfig，退出时清理。

        仅供 execute_login_attempt / execute_browser_task 这类一次性命令使用。
        debug_start 不适用：其 cancel_id 需保留至 debug_stop 才清理。

        yields: (cancel_event, bs, task)
        """
        bs = params.get("browser_settings", {}) or {}
        task_raw = params.get("task_config", {}) or {}
        cancel_id = params.get("cancel_id", "")
        cancel_event = cancel_registry.register(cancel_id) if cancel_id else None
        try:
            task = TaskConfig.from_dict(task_raw)
            yield cancel_event, bs, task
        finally:
            if cancel_id:
                cancel_registry.unregister(cancel_id)

    async def handle_browser_health_check(self, params: dict) -> dict:
        """健康检查：确认 Playwright 与浏览器可用。"""
        bs = params.get("browser_settings") or {}
        channel = str(bs.get("browser_channel") or "playwright").strip().lower()
        custom_path = str(bs.get("browser_custom_path") or "").strip()
        try:
            # _ensure_browser 现为纯文件系统/registry 探测（不再冷启 sync_playwright
            # driver），但目录遍历与 registry 读取仍是同步 IO：丢到线程池执行，
            # 避免慢盘上阻塞事件循环，与 OCR classification 的同步 CPU 推理处理一致。
            healthy = await asyncio.to_thread(_ensure_browser, channel, custom_path)
        except Exception as exc:  # noqa: BLE001 — 健康检查失败本身即结果（healthy=False），不能向 IPC 抛异常
            logger.warning(f"健康检查异常: {exc}")
            healthy = False
        result: dict = {"healthy": healthy, "channel": channel}
        if not healthy:
            # 上报缺失组件：Rust 侧据此给出「缺哪个目录」的可操作错误，
            # 而不是只剩通用「健康检查失败」（ENV-1 失败闭环）
            engine = _managed_engine(channel)
            if engine:
                result["missing"] = _missing_components(engine)
                # 托管引擎可由 uv playwright install 自动补齐
                result["reinstallable"] = True
            else:
                # 系统通道（msedge/chrome）与 custom 没有可补的 Playwright 二进制：
                # engine 为 None 时若上报空数组，Rust 侧只剩「未上报缺失组件明细」
                # 这一无指向性文案（启动超时/健康检查失败），排障方向完全偏离
                # 真实原因「配置指定的浏览器未安装」。此处改为给出可操作指向。
                result["missing"] = [_unavailable_channel_hint(channel, custom_path)]
                # 无二进制可下，Rust 侧不应再宣称「将尝试自动重装」
                result["reinstallable"] = False
        return result

    async def handle_worker_health_check(self, params: dict) -> dict:
        """轻量健康检查：确认 Worker IPC/事件循环可用，不探测 Chromium。

        任务 10：响应向后兼容地扩展 ``version`` 与 ``capabilities``——
        Rust 侧（BridgeSupervisor.send_health_check）会缓存 capabilities，
        供 /api/ocr/status 在 Worker 存活时优先展示运行时 OCR 能力。
        """
        return {
            "healthy": True,
            "version": WORKER_VERSION,
            "capabilities": dict(self.capabilities),
        }

    def _ensure_no_debug_session(self, action: str) -> None:
        """B3 纵深防御（Python 半）：调试会话存续时拒绝新建浏览器类任务。

        调试会话持有 Worker 浏览器上下文，登录/浏览器任务此时执行会重建浏览器，
        把调试会话的 page/context 连根拔掉。Outcome 无 BUSY 变体（新增会破坏
        与 Rust 的 serde 契约），复用最贴近的 UNKNOWN_ERROR（终态失败、不重试），
        消息中明确说明原因。根治方案（Rust 侧会话槽位覆盖调试会话整个存活期，
        而非仅 debug_start 命令期间）另行立项。
        """
        if self._debug_sessions:
            raise WorkerError(
                Outcome.UNKNOWN_ERROR, f"调试会话进行中，无法执行{action}，请先停止调试"
            )

    @staticmethod
    async def _inspect_redirect_test_page(
        page: Any,
        trigger_url: str,
        response_status: int | None,
    ) -> dict[str, Any]:
        """读取主页面与 iframe 的有限可见信号，不回传原文或最终地址。"""
        text_parts: list[str] = []
        password_inputs = 0
        account_inputs = 0
        forms = 0
        remaining = _REDIRECT_TEST_TEXT_LIMIT
        for frame in list(page.frames):
            if remaining <= 0:
                break
            try:
                body_text = await frame.locator("body").inner_text(timeout=1500)
                if body_text:
                    clipped = body_text[:remaining]
                    text_parts.append(clipped)
                    remaining -= len(clipped)
            except Exception:  # noqa: BLE001 — 跨域/销毁中的 frame 可能无法读取，其余 frame 仍可判断
                pass
            try:
                password_inputs += await frame.locator("input[type='password']").count()
                account_inputs += await frame.locator(
                    "input[type='text'], input[type='email'], input[name*='user' i], "
                    "input[name*='account' i], input[name*='phone' i]"
                ).count()
                forms += await frame.locator("form").count()
            except Exception:  # noqa: BLE001 — 单个 frame 销毁不应让整次检测失败
                pass
        try:
            title = await page.title()
        except Exception:  # noqa: BLE001
            title = ""
        return _classify_redirect_test(
            trigger_url=trigger_url,
            final_url=str(getattr(page, "url", "") or ""),
            response_status=response_status,
            visible_text="\n".join([title, *text_parts]),
            password_inputs=password_inputs,
            account_inputs=account_inputs,
            forms=forms,
        )

    async def handle_test_redirect(self, params: dict) -> dict:
        """用独立、可见的临时浏览器验证触发地址能否到达校园网认证页。

        检测不得复用 ``self._context``：正常登录可能正在保留 Cookie、localStorage
        或门户心跳页面；复用会产生“已登录 Cookie 掩盖门户”的误判，也可能破坏
        keep_alive 会话。这里始终另启非持久化浏览器，完成后只回收自己的资源。
        """
        self._ensure_no_debug_session("重定向检测")
        trigger_url = str(params.get("trigger_url") or "").strip()
        if not trigger_url.startswith(("http://", "https://")):
            raise WorkerError(Outcome.UNKNOWN_ERROR, "重定向检测仅支持 http/https 地址")

        from playwright.async_api import async_playwright

        bs = dict(params.get("browser_settings") or {})
        channel = str(bs.get("browser_channel") or "playwright").strip().lower()
        custom_path = str(bs.get("browser_custom_path") or "").strip()
        pure_mode = bool(bs.get("pure_mode", False))
        cancel_id = str(params.get("cancel_id") or "")
        cancel_event = cancel_registry.register(cancel_id) if cancel_id else None
        created_playwright = False
        browser: Any = None
        context: Any = None
        response_status: int | None = None
        self._cancel_browser_idle_release()
        try:
            if self._playwright is None:
                self._playwright = await async_playwright().start()
                created_playwright = True
            launch_args = [] if pure_mode else self._build_launch_args(bs, channel)
            # 用户明确要求看到真实跳转过程：无论全局 headless 设置如何，检测恒为可见窗口。
            browser = await self._launch_browser(
                self._playwright,
                channel,
                custom_path,
                False,
                launch_args,
                bs,
            )
            context = await browser.new_context(**self._build_context_options(bs))
            if not pure_mode and bs.get("stealth_mode", False):
                custom = str(bs.get("stealth_custom_script") or "").strip()
                await context.add_init_script(custom or _STEALTH_INIT_SCRIPT)

            pages: list[Any] = []
            context.on("page", lambda new_page: pages.append(new_page))
            page = await context.new_page()
            if page not in pages:
                pages.append(page)
            if cancel_event is not None and cancel_event.is_set():
                raise StepCancelled("重定向检测已取消")
            try:
                response = await page.goto(
                    trigger_url,
                    wait_until="domcontentloaded",
                    timeout=min(_nav_timeout(bs), 15000),
                )
                if response is not None:
                    response_status = response.status
            except Exception as exc:  # noqa: BLE001 — 导航失败属于“无法跟随”，不是 Worker 故障
                logger.info("重定向检测导航失败: %s", type(exc).__name__)
                return {"status": "not_detected"}

            deadline = time.monotonic() + _REDIRECT_TEST_SETTLE_SECS
            while time.monotonic() < deadline:
                if cancel_event is not None and cancel_event.is_set():
                    raise StepCancelled("重定向检测已取消")
                await asyncio.sleep(0.25)

            active_page = page
            for candidate in reversed(pages):
                try:
                    if not candidate.is_closed():
                        active_page = candidate
                        break
                except Exception:  # noqa: BLE001
                    continue
            return await self._inspect_redirect_test_page(
                active_page,
                trigger_url,
                response_status,
            )
        finally:
            if cancel_id:
                cancel_registry.unregister(cancel_id)
            if context is not None:
                try:
                    await asyncio.wait_for(context.close(), timeout=_WAIT_TIMEOUT_SECS)
                except Exception:  # noqa: BLE001 — 临时窗口关闭失败不覆盖检测结论
                    logger.warning("关闭重定向检测上下文失败", exc_info=True)
            if browser is not None:
                try:
                    await asyncio.wait_for(browser.close(), timeout=_WAIT_TIMEOUT_SECS)
                except Exception:  # noqa: BLE001
                    logger.warning("关闭重定向检测浏览器失败", exc_info=True)
            if created_playwright and self._browser is None and self._context is None:
                try:
                    await self._playwright.stop()
                except Exception:  # noqa: BLE001
                    logger.debug("停止临时 Playwright 连接失败", exc_info=True)
                self._playwright = None
            elif self._browser is not None or self._context is not None:
                self._arm_browser_idle_release()

    async def handle_execute_login_attempt(self, params: dict) -> dict:
        """执行完整登录流程。"""
        self._ensure_no_debug_session("登录任务")
        try:
            async with self._cancel_session(params) as (cancel_event, bs, task):
                # Rust 会把“登录网址留空”解析为有效触发地址；触发器非空时优先，
                # 兼容旧版同时保存 auth_url + trigger_url 的显式重定向方案。
                navigate_url = _profile_login_url(params)
                # 任务变量可自定义普通模板值，但系统保留变量必须始终反映当前 Profile。
                # 统一经 _system_variables 注入：键缺失时跳过（避免空串覆盖任务自定义
                # 变量）；{{LOGIN_URL}} 优先有效 trigger_url、回落 auth_url，与首导航一致。
                variables = dict(task.variables or {})
                variables.update(self._system_variables(params))
                self._task_dialogs = []
                result = await self._run_task(
                    task, bs, variables, cancel_event, _debug_screenshot_dir(),
                    navigate_url=navigate_url,
                )
                result.data = {"dialogs": list(self._task_dialogs)}
                return result.to_dict()
        finally:
            # 登录任务与普通浏览器任务保持一致，结束后都武装空闲回收。
            self._arm_browser_idle_release()

    async def handle_execute_browser_task(self, params: dict) -> dict:
        """执行浏览器任务（不含账号密码语义）。"""
        # B3 防御（Python 半）：同 handle_execute_login_attempt，调试会话存续期
        # 内拒绝浏览器任务，避免上下文互踩。
        self._ensure_no_debug_session("浏览器任务")
        try:
            async with self._cancel_session(params) as (cancel_event, bs, task):
                variables = dict(task.variables or {})
                variables.update(self._system_variables(params))
                self._task_dialogs = []
                result = await self._run_task(
                    task, bs, variables, cancel_event, _debug_screenshot_dir(),
                    nav_fallback_url=_profile_login_url(params),
                )
                result.data = {"dialogs": list(self._task_dialogs)}
                return result.to_dict()
        finally:
            # 任务结束（含失败）即武装空闲回收：keep_alive 关闭时超时后全量释放浏览器
            self._arm_browser_idle_release()

    @staticmethod
    def _system_variables(params: dict) -> dict:
        """提取命令参数中的系统保留变量（{{USERNAME}} 等），仅覆盖调用方显式提供的键。

        Rust 侧登录编排总是传全量四项；任务执行/调试路径经同一注入保证任务模板
        与文档契约一致。键缺失时不注入，避免空串覆盖任务自定义 variables。
        重定向登录下 {{LOGIN_URL}} 优先取有效 trigger_url（回落 auth_url），与登录首导航一致。
        """
        mapping = {
            "USERNAME": "username",
            "PASSWORD": "password",
            "ISP": "isp",
            "LOGIN_URL": "auth_url",
        }
        out = {sys_key: str(params[src_key]) for sys_key, src_key in mapping.items() if src_key in params}
        trigger = params.get("trigger_url", "") or ""
        if isinstance(trigger, str) and trigger.strip():
            out["LOGIN_URL"] = trigger
        return out

    async def handle_debug_start(self, params: dict) -> dict:
        """启动调试会话，保留浏览器上下文供后续 debug_step 复用。

        与 Rust 单会话语义一致：同一时刻仅允许一个活跃调试会话，
        需先 debug_stop 才能再次启动（避免多个会话共享 self._page 互相覆盖）。
        """
        bs = params.get("browser_settings", {}) or {}
        task_raw = params.get("task_config", {}) or {}
        cancel_id = params.get("cancel_id", "")
        if self._debug_sessions:
            raise WorkerError(Outcome.UNKNOWN_ERROR, "已存在活跃调试会话，请先停止再启动")
        session_id = uuid.uuid4().hex
        self._session_type = "debug"
        cancel_event = cancel_registry.register(cancel_id) if cancel_id else None
        session_established = False
        try:
            task = TaskConfig.from_dict(task_raw)

            # G3：启动窗口（环境准备 + 首导航）同样响应取消——步骤层有取消检查，
            # 但 start 阶段点"取消"此前被完全忽略，浏览器会照常拉起并导航
            def _ensure_not_cancelled(stage: str) -> None:
                if cancel_event is not None and cancel_event.is_set():
                    raise StepCancelled(f"调试已取消（{stage}阶段）")

            _ensure_not_cancelled("环境准备前")
            await self.ensure_browser({"browser_settings": bs})
            _ensure_not_cancelled("会话准备前")
            # 调试会话同样是顶层存储隔离边界。
            await self._prepare_session_page()
            variables = dict(task.variables or {})
            variables.update(self._system_variables(params))
            context = self._make_context(
                self._page, variables, bs, cancel_event, _debug_screenshot_dir(), task
            )
            # 首导航：任务自身 url 优先（调试以“这个任务”为准，不做强制覆盖），
            # 未配置时回落 Profile 有效登录地址。原实现只判 `if task.url:`（判的是
            # 字面量 `{{LOGIN_URL}}`）就去 goto 解析后的空串，认证地址留空时启动即失败。
            target = _resolve_start_url(task.url, variables, _profile_login_url(params))
            if target:
                _ensure_not_cancelled("导航前")
                await self._navigate(self._page, target, _nav_timeout(bs))
                await self._wait_after_navigation(task, context)
                _ensure_not_cancelled("导航后")
            else:
                # 任务 url 与 Profile 认证地址均为空（如直连渠道且认证地址留空）时
                # 跳过首导航：goto("") 会以无效 URL 中断启动，会话根本建不起来；
                # 任务自带的 goto 步骤仍可手动单步执行
                logger.warning("调试任务无起始地址（任务 url 与 Profile 认证地址均为空），跳过首导航")
            self._debug_sessions[session_id] = DebugSession(
                session_id=session_id,
                page=self._page,
                task_config=task,
                context=context,
                cancel_id=cancel_id,
                task_id=task.task_id,
                steps_info=_build_steps_info(task),
            )
            session_established = True
            # 初始截图（整页）。失败不中断启动：首个步骤结束后还有步骤级补拍兜底
            if not await self._capture_debug_frame(
                self._debug_sessions[session_id], full_page=True
            ):
                logger.warning("调试会话初始截图失败，预览将等待首个步骤截图")
            return self._debug_response(self._debug_sessions[session_id])
        finally:
            # debug_start 的 cancel_id 只覆盖启动命令；会话建成后的每个 step/run_all
            # 都注册自己的请求 cancel_id，避免 Rust 取消新命令却命中旧令牌。
            cancelled_after_establish = bool(
                session_established
                and cancel_event is not None
                and cancel_event.is_set()
            )
            if cancelled_after_establish:
                # 取消可能恰好发生在会话写入字典与响应返回之间；此时不能留下一个
                # Rust 已判定取消、Worker 却仍视为活跃的幽灵调试会话。
                session = self._debug_sessions.pop(session_id, None)
                if session is not None:
                    self._teardown_debug_session(session)
                session_established = False
                await self._close_session()
            if cancel_id:
                cancel_registry.unregister(cancel_id)
            if session_established:
                self._debug_sessions[session_id].context.cancel_event = None
                self._debug_sessions[session_id].cancel_id = ""
            if cancelled_after_establish:
                raise StepCancelled("调试已取消（启动完成阶段）")

    def _debug_session_for(self, session_id: str) -> "DebugSession":
        """解析调试会话：显式 session_id 优先；为空时回退到唯一活跃会话。

        Rust 侧从不显式传 session_id（单会话语义），空串时若恰有一个活跃
        会话则回退到它；存在多个会话时要求显式指定，避免歧义（历史遗留 P1）。
        """
        if session_id:
            session = self._debug_sessions.get(session_id)
            if session is None:
                raise WorkerError(Outcome.UNKNOWN_ERROR, "调试会话不存在，请先启动调试")
            return session
        if len(self._debug_sessions) == 1:
            return next(iter(self._debug_sessions.values()))
        if self._debug_sessions:
            raise WorkerError(Outcome.UNKNOWN_ERROR, "存在多个调试会话，请指定 session_id")
        raise WorkerError(Outcome.UNKNOWN_ERROR, "调试会话不存在，请先启动调试")

    @staticmethod
    def _debug_response(session: "DebugSession") -> dict:
        """序列化调试会话为前端可渲染的完整结构（对齐原版 debug_to_response）。

        返回完整 steps + results，前端据此渲染逐步信息，而非仅返回孤立的
        结构化结果（修复：调试面板步骤"全空"）。
        """
        total = len(session.steps_info)
        return {
            "running": session.current_step < total,
            "task_id": session.task_id,
            "current_step": session.current_step,
            "total_steps": total,
            "steps": session.steps_info,
            "results": list(session.results),
            "screenshot_url": None,
        }

    async def _capture_debug_frame(
        self,
        session: "DebugSession",
        *,
        step_index: int | None = None,
        full_page: bool = False,
    ) -> bool:
        """补拍一帧调试页面截图并推送事件（best-effort，返回是否成功）。

        调试预览此前只有 `debug_start` 的初始截图与显式 `screenshot` 步骤会推送，
        普通步骤（input / click / ocr / sleep …）执行完什么都不推，面板的"实时截图"
        永远停在启动画面——表现为"点下一步浏览器不刷新"。每次步骤结束后补拍一帧，
        预览才随单步前进。

        `step_index` 为 0 基步骤序号（初始截图为 None），前端据此在预览标题上显示
        这一帧是"哪一步之后"的画面。

        步骤补拍默认取视口截图（`full_page=False`）：1280x720 视口体积小、无需整页
        拼接，既不拖慢单步响应，也能避开长页 full_page 超过 Rust 侧 8MiB 内联上限
        被静默丢弃的情况；会话初始截图仍用整页，便于一眼看清门户全貌。

        截图失败（页面已关闭 / 超时 / 磁盘错误）只记日志并返回 False，绝不影响
        步骤结果与命令响应。
        """
        page = getattr(session.context, "page", None)
        if page is None:
            page = session.page
        try:
            shot_dir = _debug_screenshot_dir()
            shot_dir.mkdir(parents=True, exist_ok=True)
            stamp = str(int(time.time() * 1000))
            # 文件名带步骤序号：同一毫秒内的两次截图不会互相覆盖，且能一眼看出归属
            label = "init" if step_index is None else str(step_index)
            local_path = str(shot_dir / f"debug_{session.session_id}_{label}_{stamp}.png")
            await page.screenshot(
                path=local_path, full_page=full_page, timeout=DEBUG_FRAME_TIMEOUT_MS
            )
            # 追踪路径以便会话结束时统一清理（截图可能含表单明文凭据）
            session.context.screenshots.append(local_path)
            payload: dict[str, Any] = {"path": local_path}
            if step_index is not None:
                payload["step_index"] = step_index
            self.emit("screenshot", payload)
            return True
        except Exception as exc:  # noqa: BLE001 — 预览截图失败不影响步骤结果
            logger.debug("调试截图失败（忽略）: %s", exc)
            return False

    @staticmethod
    def _record_debug_result(
        session: "DebugSession", idx: int, success: bool, message: str
    ) -> None:
        """记录单个步骤的调试结果，供前端结果列表展示。"""
        session.results.append(
            {
                "step_index": idx,
                "success": bool(success),
                "message": message or ("" if success else "执行失败"),
                "running": False,
            }
        )

    @asynccontextmanager
    async def _debug_command_cancel(
        self, params: dict, session: "DebugSession"
    ) -> AsyncIterator[None]:
        """将当前调试命令的 cancel_id 临时绑定到复用的会话上下文。"""
        cancel_id = params.get("cancel_id", "")
        cancel_event = cancel_registry.register(cancel_id) if cancel_id else None
        previous = session.context.cancel_event
        session.context.cancel_event = cancel_event
        try:
            yield
        finally:
            session.context.cancel_event = previous
            if cancel_id:
                cancel_registry.unregister(cancel_id)

    async def handle_debug_step(self, params: dict) -> dict:
        """执行调试会话中的单个步骤。

        优先级：
        - 提供 `step`（完整 StepConfig）→ 执行该步；
        - 提供 `step_index`（整数）→ 执行该索引处的步骤；
        - 两者皆无 → 自动执行“下一步”（由会话内游标维护），便于前端逐步调试。

        执行后记录步骤结果并返回完整会话数据（steps + results），供前端逐步渲染。
        """
        session_id = params.get("session_id", "")
        session = self._debug_session_for(session_id)

        steps = session.task_config.steps
        step_raw = params.get("step")
        step_index = params.get("step_index")

        if step_raw is not None:
            step = StepConfig.from_dict(step_raw)
            auto_advance = False
            idx = None
        elif isinstance(step_index, int):
            if step_index < 0 or step_index >= len(steps):
                raise WorkerError(Outcome.UNKNOWN_ERROR, f"调试步骤索引越界: {step_index}")
            step = steps[step_index]
            auto_advance = False
            idx = step_index
        else:
            # 自动执行下一步
            idx = session.current_step
            if idx >= len(steps):
                return self._debug_response(session)
            step = steps[idx]
            auto_advance = True

        success = True
        message = ""
        async with self._debug_command_cancel(params, session):
            try:
                await run_step_async(
                    session.page, step, session.context,
                    step_index=idx, total_steps=len(steps),
                )
            except Exception as exc:  # noqa: BLE001 — 取消/分类失败/未预期异常统一归一（前两者不记堆栈）
                if not isinstance(exc, WorkerError):
                    logger.exception("调试步骤执行未预期异常")
                _outcome, message = _normalize_step_failure(exc)
                success = False
        if idx is not None:
            self._record_debug_result(session, idx, success, message)
        if auto_advance and idx is not None:
            session.current_step = idx + 1
        # 步骤结束后补拍一帧：面板"实时截图"随单步前进（含显式 step 负载的场景，
        # 该路径没有 idx，用会话游标作为归属序号）
        await self._capture_debug_frame(
            session, step_index=idx if idx is not None else session.current_step
        )
        return self._debug_response(session)

    async def handle_debug_run_all(self, params: dict) -> dict:
        """依次执行调试会话中尚未运行的全部步骤（从当前游标到末尾）。

        与正式执行保持一致：步骤间应用 ``step_delay``；可选步骤失败记录后继续，
        必需步骤失败、取消或未预期异常则停止。返回完整会话数据。
        """
        session_id = params.get("session_id", "")
        session = self._debug_session_for(session_id)

        steps = session.task_config.steps
        start = session.current_step
        stop_idx = len(steps)
        if start >= len(steps):
            return self._debug_response(session)
        async with self._debug_command_cancel(params, session):
            for idx in range(start, len(steps)):
                step = steps[idx]
                session.current_step = idx
                success = True
                message = ""
                fatal = False
                try:
                    if idx > start and session.context.step_delay > 0:
                        await _sleep_cancellable(session.context.step_delay, session.context)
                    await run_step_async(
                        session.page,
                        step,
                        session.context,
                        step_index=idx,
                        total_steps=len(steps),
                    )
                except Exception as exc:  # noqa: BLE001 — 取消/分类失败/未预期异常统一归一（前两者不记堆栈）
                    if not isinstance(exc, WorkerError):
                        logger.exception("调试批量执行未预期异常")
                    _outcome, message = _normalize_step_failure(exc)
                    success = False
                    # 取消与未预期异常终止批量执行；分类失败按 required 决定是否继续
                    fatal = (
                        step.required
                        if isinstance(exc, WorkerError) and not isinstance(exc, StepCancelled)
                        else True
                    )
                self._record_debug_result(session, idx, success, message)
                # 每步都补拍一帧：批量执行时前端预览同步推进到当前步骤画面
                await self._capture_debug_frame(session, step_index=idx)
                if fatal:
                    stop_idx = idx + 1
                    break
        session.current_step = stop_idx
        return self._debug_response(session)

    def _defer_task_screenshot_cleanup(self, context: StepContext) -> None:
        """延迟清理普通任务截图，为 WebSocket 读盘与编码预留窗口。"""
        paths = list(context.screenshots)
        context.screenshots.clear()
        if not paths:
            return

        async def _cleanup_later() -> None:
            await asyncio.sleep(TASK_SCREENSHOT_RETENTION_SECS)
            for path in paths:
                try:
                    Path(path).unlink(missing_ok=True)
                except Exception as exc:  # noqa: BLE001
                    logger.debug("延迟清理任务截图失败 %s: %s", path, exc)

        task = asyncio.create_task(_cleanup_later())
        self._screenshot_cleanup_tasks.add(task)
        task.add_done_callback(self._screenshot_cleanup_tasks.discard)

    @staticmethod
    def _cleanup_debug_screenshots(session: "DebugSession") -> None:
        """删除调试会话期间产生的截图文件。

        调试截图可能包含表单中的明文凭据，会话结束后及时清除，
        避免长期驻留磁盘（历史遗留 F5）。
        """
        paths = list(getattr(session.context, "screenshots", []) or [])
        for p in paths:
            try:
                Path(p).unlink(missing_ok=True)
            except Exception as exc:  # noqa: BLE001
                logger.debug(f"清理调试截图失败 {p}: {exc}")
        # screenshots 是 StepContext 的 list[str] 字段，list.clear() 不会抛出
        session.context.screenshots.clear()

    def _teardown_debug_session(self, session: "DebugSession") -> None:
        """幂等释放单个调试会话的磁盘截图与取消注册。"""
        self._cleanup_debug_screenshots(session)
        cancel_id = getattr(session, "cancel_id", "")
        if cancel_id:
            cancel_registry.unregister(cancel_id)
            # 同一 session 被异常路径重复收尾时不重复触碰注册表。
            session.cancel_id = ""

    def _teardown_all_debug_sessions(self) -> None:
        """从槽位移除并释放全部调试会话。"""
        sessions = list(self._debug_sessions.values())
        self._debug_sessions.clear()
        for session in sessions:
            self._teardown_debug_session(session)

    async def handle_debug_status(self, params: dict) -> dict:
        """查询当前调试会话详情（无副作用）。供前端刷新后恢复步骤数据。"""
        if not self._debug_sessions:
            raise WorkerError(Outcome.UNKNOWN_ERROR, "调试会话不存在，请先启动调试")
        session = self._debug_session_for(params.get("session_id", ""))
        return self._debug_response(session)

    async def handle_debug_stop(self, params: dict) -> dict:
        """停止调试会话并释放会话资源（keep_alive 时浏览器进程常驻热复用）。"""
        session_id = params.get("session_id", "")
        session = self._debug_session_for(session_id)
        self._debug_sessions.pop(session.session_id, None)
        self._teardown_debug_session(session)
        await self._close_session()
        # 调试结束即武装空闲回收：keep_alive 关闭时浏览器进程超时后全量释放
        self._arm_browser_idle_release()
        return {}

    async def handle_close_browser(self, params: dict) -> dict:
        """登录会话终态后的浏览器资源回收（Worker 进程保留）。

        三档行为，由 Rust 侧按配置与登录结果决定：
        - ``preserve_state=true``（keep_alive 且登录成功）：页面与登录状态原样
          保留——门户页 JS 心跳/在线状态不中断，不清 cookie、不导航、不关页；
        - Worker 启动环境 ``CAMPUS_AUTH_WORKER_KEEP_ALIVE=1``（keep_alive 但非
          成功终态）：会话级释放（关页面与非持久化 context），浏览器进程留给
          下次登录热启动；
        - 默认：全量关闭浏览器（历史行为）。

        极端情况下 close 可能挂起（如 driver 未及时退出），保留内部超时兜底，
        避免一条挂起命令阻塞 Worker 命令队列。

        内部兜底必须先于命令级自愈触发：Rust 下发 ``rust_timeout_ms``（如
        close_browser 的 8s 预算）时，命令级超时为 0.9×预算（7.2s），若内部
        兜底仍用固定 8s，慢关闭会被命令级自愈拦腰打断成半关闭（页关了、
        context/进程没收）。故有预算时内部兜底取 ``min(8s, 0.9×预算 - 0.5s)``，
        先以 TimeoutError 收尾返回（浏览器清理在本函数的超时里继续尽力完成），
        命令级自愈不再有机会触发。
        """
        self._cancel_browser_idle_release()
        if params.get("preserve_state"):
            logger.info("keep_alive 常驻：登录成功，保留页面与登录状态")
            return {}
        internal_timeout = _WAIT_TIMEOUT_SECS
        budget_ms = params.get("rust_timeout_ms")
        if isinstance(budget_ms, (int, float)) and budget_ms > 0:
            internal_timeout = max(
                0.5, min(_WAIT_TIMEOUT_SECS, float(budget_ms) / 1000 * 0.9 - 0.5)
            )
        try:
            if _WORKER_KEEP_ALIVE:
                await asyncio.wait_for(self._close_session(), timeout=internal_timeout)
            else:
                await asyncio.wait_for(self.close_browser(), timeout=internal_timeout)
        except asyncio.TimeoutError:
            logger.warning("close_browser 超时（%.1fs），跳过等待继续", internal_timeout)
        return {}

    async def _capture_navigate(self, url: str, bs: dict, cancel_event: Any) -> None:
        """页面捕获前置：确保浏览器就绪、建隔离会话页并导航到目标 URL。

        门户页常在加载后异步拉验证码/配置脚本，导航后等待一轮 networkidle 让
        DOM 与已加载资源尽量齐全；长轮询页面等满超时即按当前状态继续（不致命）。
        取消事件是裸 threading.Event（非 StepContext），导航边界处直接检查置位。
        """
        await self.ensure_browser({"browser_settings": bs})
        # 捕获用于分析“未登录门户”，不得复用常驻登录会话的 Cookie。
        if self._context is not None:
            await self._context.clear_cookies()
        await self._prepare_session_page()
        await self._navigate(self._page, url, _nav_timeout(bs))
        try:
            await self._page.wait_for_load_state("networkidle", timeout=8000)
        except Exception:  # noqa: BLE001
            logger.debug("networkidle 等待超时，按当前页面状态继续捕获")
        if cancel_event is not None and cancel_event.is_set():
            raise StepCancelled("页面捕获已取消")

    @staticmethod
    async def _capture_mhtml(page: Any, target: Path) -> bool:
        """经 CDP 抓取完整布局 MHTML 快照并写入 target，成功返回 True。

        MHTML 单文件自包含样式/图片，供"保存页面文件"离线还原；Chromium 按
        设计不含 JS，脚本由 resources/ 补齐。捕获失败或快照为空时返回 False，
        不影响其余产物；detach 失败只记录日志，不能覆盖已经成功写盘的快照。
        """
        cdp = None
        captured = False
        try:
            cdp = await page.context.new_cdp_session(page)
            mhtml = await cdp.send("Page.captureSnapshot", {"format": "mhtml"})
            cdp_data = mhtml.get("data", "")
            if cdp_data:
                payload = cdp_data.encode("utf-8") if isinstance(cdp_data, str) else bytes(cdp_data)
                target.write_bytes(payload)
                captured = True
        except Exception as exc:  # noqa: BLE001 — MHTML 失败不影响其余产物
            logger.debug("MHTML 快照失败（跳过）: %s", exc)
        finally:
            if cdp is not None:
                try:
                    await cdp.detach()
                except Exception as exc:  # noqa: BLE001 — detach 失败不能覆盖已成功的快照
                    logger.debug("MHTML CDP 会话释放失败（忽略）: %s", exc)
        return captured

    @staticmethod
    def _write_capture_meta(
        cap_dir: Path,
        url: str,
        final_url: str,
        title: str,
        resources: dict[str, str],
        structure_summary: dict[str, int],
        *,
        mhtml_ok: bool,
        offline_ok: bool,
        note: str | None,
    ) -> dict[str, Any]:
        """构建并落盘 captures/latest/meta.json，返回 meta 供响应组装。

        资源目录仅在确有产物时写入路径；note 为空不写字段，保持 meta 结构
        与消费方（Rust 读盘侧）的既有契约一致。``mhtml_path`` / 离线副本路径
        同为可选字段：非 Chromium 渠道没有 MHTML，无资源时也没有离线副本。
        """
        meta: dict[str, Any] = {
            "captured_at": time.strftime("%Y-%m-%dT%H:%M:%S"),
            "request_url": url,
            "final_url": final_url,
            "title": title,
            "html_path": str(cap_dir / "page.html"),
            "screenshot_path": str(cap_dir / "screenshot.png"),
            "resources_dir": str(cap_dir / "resources") if resources else None,
            "resources_count": len(resources),
            "structure_path": str(cap_dir / "page_structure.json"),
            "structure_summary": structure_summary,
        }
        if mhtml_ok:
            meta["mhtml_path"] = str(cap_dir / "page.mhtml")
        if offline_ok:
            meta["offline_html_path"] = str(cap_dir / "page.offline.html")
        if note:
            meta["note"] = note
        (cap_dir / "meta.json").write_text(
            json.dumps(meta, ensure_ascii=False, indent=2), encoding="utf-8"
        )
        return meta

    async def handle_page_capture(self, params: dict) -> dict:
        """导航到目标页面并落盘 HTML / CSS-JS 资源 / 全页截图（供 AI 任务生成）。

        与 ``feedback_capture`` 的差异：本命令自带导航（无活跃页面也可用），
        产物固定写到 ``captures/latest/``（每次覆盖），内容只经 Rust 侧读盘消费，
        不走 IPC 回传——NDJSON 单行上限 1 MiB，门户页 HTML/JS 普遍超限。
        会话语义与 ``execute_login_attempt`` 一致：占用浏览器会话槽位（Rust 侧
        FIFO 排队），导航会替换当前页；存在活跃调试会话时由 Rust 互斥矩阵快速失败。
        """
        url = str(params.get("url") or "").strip()
        if not url:
            raise WorkerError(Outcome.UNKNOWN_ERROR, "page_capture 缺少 url")
        if not url.startswith(("http://", "https://")):
            raise WorkerError(Outcome.UNKNOWN_ERROR, "仅支持 http/https 页面捕获")
        if self._debug_sessions:
            raise WorkerError(Outcome.UNKNOWN_ERROR, "存在活跃调试会话，请先停止调试再捕获")
        bs = params.get("browser_settings", {}) or {}
        cancel_id = params.get("cancel_id", "")
        cancel_event = cancel_registry.register(cancel_id) if cancel_id else None

        def _ensure_not_cancelled(stage: str) -> None:
            """阶段边界取消检查（对齐 debug_start 的分阶段模式）。"""
            if cancel_event is not None and cancel_event.is_set():
                raise StepCancelled(f"页面捕获已取消（{stage}）")

        try:
            await self._capture_navigate(url, bs, cancel_event)
            # 落盘目录与 _debug_screenshot_dir 同语义：锚定运行时 worker 工程目录，
            # 不依赖进程 CWD；固定 captures/latest 每次覆盖，避免产物无界堆积
            _ensure_not_cancelled("HTML 获取前")
            cap_dir = _capture_dir()
            shutil.rmtree(cap_dir, ignore_errors=True)
            cap_dir.mkdir(parents=True, exist_ok=True)
            html = await self._page.content()
            (cap_dir / "page.html").write_text(html, encoding="utf-8")
            _ensure_not_cancelled("截图前")
            png_bytes = await self._page.screenshot(full_page=True)
            screenshot_note: str | None = None
            if len(png_bytes) > _CAPTURE_MAX_SCREENSHOT_BYTES:
                png_bytes = await self._page.screenshot(full_page=False)
                screenshot_note = "全页截图过大，已改为当前视口截图"
            (cap_dir / "screenshot.png").write_bytes(png_bytes)
            _ensure_not_cancelled("结构提取前")
            try:
                structure = await _capture_page_structure(self._page)
            except Exception as exc:  # noqa: BLE001 — 原始 HTML 仍可作为生成兜底
                structure = {"version": 1, "frames": [], "error": str(exc)[:300]}
            (cap_dir / "page_structure.json").write_text(
                json.dumps(structure, ensure_ascii=False, indent=2), encoding="utf-8"
            )
            frame_items = structure.get("frames", [])
            structure_summary = {
                "frames": len(frame_items),
                "forms": sum(len(item.get("forms", [])) for item in frame_items),
                "controls": sum(len(item.get("controls", [])) for item in frame_items),
                "captcha_candidates": sum(
                    len(item.get("captcha_candidates", [])) for item in frame_items
                ),
            }
            mhtml_ok = False
            # MHTML 走 CDP，非 Chromium 渠道下调用必然失败：直接跳过，降级说明由
            # 资源快照那条统一给出（避免日志里出现无意义的英文 CDP 异常）
            if _channel_supports_cdp(bs):
                mhtml_ok = await self._capture_mhtml(self._page, cap_dir / "page.mhtml")
            _ensure_not_cancelled("资源快照前")
            resources: dict[str, str] = {}
            aliases: dict[str, str] = {}
            note: str | None = None
            try:
                resources, aliases, note = await _capture_page_resources(
                    self._page, cap_dir / "resources", bs=bs
                )
            except Exception as exc:  # noqa: BLE001 — 资源快照失败不阻断 HTML/截图
                note = f"资源快照失败: {exc}"
            _ensure_not_cancelled("离线副本生成前")
            # 离线副本：把 HTML 中的资源引用改写成 resources/ 相对路径，解压后直接
            # 双击即可还原（无 MHTML 的渠道下这是唯一可离线查看的形态）。原始
            # page.html 保持不变——它是 Rust 侧喂给 LLM 的材质，URL 语义不该被污染。
            offline_ok = False
            if resources:
                try:
                    offline_html = _rewrite_resource_urls(html, {**resources, **aliases})
                    (cap_dir / "page.offline.html").write_text(offline_html, encoding="utf-8")
                    offline_ok = True
                except Exception as exc:  # noqa: BLE001 — 离线副本失败不影响其余产物
                    extra = f"离线副本生成失败: {exc}"
                    note = f"{note}；{extra}" if note else extra
            if screenshot_note:
                note = f"{note}；{screenshot_note}" if note else screenshot_note
            try:
                title = await self._page.title()
            except Exception:  # noqa: BLE001 — 页面标题读取失败不致命
                title = ""
            meta = self._write_capture_meta(
                cap_dir,
                url,
                self._page.url,
                title,
                resources,
                structure_summary,
                mhtml_ok=mhtml_ok,
                offline_ok=offline_ok,
                note=note,
            )
            logger.info(
                "[capture] 页面捕获完成: %s (html=%d chars, resources=%d)",
                meta["final_url"],
                len(html),
                len(resources),
            )
            return {
                "final_url": meta["final_url"],
                "title": title,
                "html_chars": len(html),
                "png_bytes": len(png_bytes),
                "resources_count": len(resources),
                "structure_summary": structure_summary,
                "note": note,
            }
        finally:
            # finally 而非 except Exception：命令级超时的 task.cancel() 产生
            # CancelledError（BaseException），except Exception 拦不住会跳过注销
            self._arm_browser_idle_release()
            if cancel_id:
                cancel_registry.unregister(cancel_id)

    async def handle_feedback_capture(self, params: dict) -> dict:
        """捕获当前调试页面的完整 MHTML、截图与 CSS/JS 资源（供导出问题报告）。

        `page.content()` 仅含 HTML，外链 CSS/图片离线无法还原完整布局。
        能取到 CDP MHTML 时优先使用，否则回退 HTML；大内容统一落盘避免 IPC 1MiB 超限。
        Chromium 的 MHTML 序列化按设计不保存 JS（CSS 也只嵌内存缓存命中的部分），
        故额外经 Page.getResourceTree/getResourceContent 把已加载的脚本与样式表
        落盘到 resources/，并生成引用改写后的 page.html 供源码级离线还原。
        非 Chromium 渠道没有 CDP：MHTML 直接跳过，CSS/JS 由页面枚举 + HTTP 回补
        抓取（见 ``_capture_page_resources``），产物同样可用于离线还原。

        与 page_capture 对齐：注册 cancel_id 并在关键阶段检查取消；入口取消
        待触发的浏览器空闲回收；CDP detach 放入 finally，抓取失败也不泄漏会话。
        """
        if self._page is None:
            raise WorkerError(Outcome.UNKNOWN_ERROR, "无活跃页面，无法捕获")
        cancel_id = str(params.get("cancel_id") or "")
        cancel_event = cancel_registry.register(cancel_id) if cancel_id else None

        def _ensure_not_cancelled(stage: str) -> None:
            if cancel_event is not None and cancel_event.is_set():
                raise StepCancelled(f"页面捕获已取消（{stage}）")

        # 与 test_redirect / page_capture 一致：捕获类命令先取消空闲自动释放
        self._cancel_browser_idle_release()
        try:
            _ensure_not_cancelled("开始前")
            # 落盘根目录先定（资源快照需要直接写入子目录），避免 IPC 1MiB 超限
            stamp = str(int(time.time() * 1000))
            fb_dir = _feedback_capture_dir(stamp)
            # 会话用的浏览器设置决定 CDP 是否可用（MHTML 与资源快照都只存在于 Chromium）
            bs = self._last_browser_settings or {}
            # 尝试 MHTML（完整离线快照，含样式与图片），失败回退 HTML；
            # 非 Chromium 渠道不尝试：CDP 在协议层不存在，只会白记一条英文异常
            mhtml_bytes: bytes | None = None
            if _channel_supports_cdp(bs):
                cdp = None
                try:
                    cdp = await self._page.context.new_cdp_session(self._page)
                    mhtml = await cdp.send("Page.captureSnapshot", {"format": "mhtml"})
                    cdp_data = mhtml.get("data", "")
                    if cdp_data:
                        mhtml_bytes = (
                            cdp_data.encode("utf-8")
                            if isinstance(cdp_data, str)
                            else bytes(cdp_data)
                        )
                except Exception as exc:  # noqa: BLE001 — CDP 不可用时回退 content()
                    logger.debug("CDP MHTML 快照失败，回退 HTML: %s", exc)
                    mhtml_bytes = None
                finally:
                    # detach 必须无条件执行：抓取失败时也要释放 CDP 会话，
                    # 否则泄漏的会话会占住与页面的绑定直到页面对象销毁
                    if cdp is not None:
                        try:
                            await cdp.detach()
                        except Exception as exc:  # noqa: BLE001
                            logger.debug("MHTML CDP 会话释放失败（忽略）: %s", exc)
            _ensure_not_cancelled("MHTML 后")
            # CSS/JS 资源快照：MHTML 不含 JS，这里补齐脚本与样式表（全部 frame）
            resources: dict[str, str] = {}  # url -> resources/<name>
            aliases: dict[str, str] = {}  # HTML 里的原始书写形态 -> resources/<name>
            resource_note: str | None = None
            try:
                resources, aliases, resource_note = await _capture_page_resources(
                    self._page, fb_dir / "resources", bs=bs
                )
            except Exception as exc:  # noqa: BLE001 — 资源快照失败不影响其余产物
                resource_note = f"资源快照失败: {exc}"
            _ensure_not_cancelled("资源快照后")
            # 有资源时 HTML 也必须导出（MHTML 内无 JS，resources 需要引用方）；
            # MHTML 不可用时同样回退 HTML
            html: str | None = None
            if mhtml_bytes is None or resources:
                try:
                    html = _rewrite_resource_urls(
                        await self._page.content(), {**resources, **aliases}
                    )
                except Exception as exc:  # noqa: BLE001
                    raise WorkerError(Outcome.UNKNOWN_ERROR, f"获取页面内容失败: {exc}") from exc
            _ensure_not_cancelled("截图前")
            try:
                png_bytes = await self._page.screenshot(full_page=True)
            except Exception as exc:  # noqa: BLE001
                raise WorkerError(Outcome.UNKNOWN_ERROR, f"截图失败: {exc}") from exc
            _ensure_not_cancelled("落盘前")
            try:
                fb_dir.mkdir(parents=True, exist_ok=True)
                png_path = fb_dir / "screenshot.png"
                png_path.write_bytes(png_bytes)
                result: dict = {"png_path": str(png_path)}
                if mhtml_bytes is not None:
                    mhtml_path = fb_dir / "page.mhtml"
                    mhtml_path.write_bytes(mhtml_bytes)
                    result["mhtml_path"] = str(mhtml_path)
                if html is not None:
                    html_path = fb_dir / "page.html"
                    html_path.write_bytes(html.encode("utf-8"))
                    result["html_path"] = str(html_path)
                if resources:
                    result["resources_dir"] = str(fb_dir / "resources")
                    result["resources_count"] = len(resources)
                if resource_note:
                    result["resources_note"] = resource_note
                return result
            except Exception as exc:  # noqa: BLE001
                raise WorkerError(Outcome.UNKNOWN_ERROR, f"落盘失败: {exc}") from exc
        finally:
            # finally 而非 except Exception：命令级超时的 task.cancel() 产生
            # CancelledError（BaseException），except Exception 拦不住会跳过注销
            if cancel_id:
                cancel_registry.unregister(cancel_id)

    async def handle_ocr_recognize(self, params: dict) -> dict:
        """识别 base64 图片中的文本（ddddocr），模型加载与推理共享总超时预算。

        注册 cancel_id：Rust 侧 /api/ocr/uninstall 据此取消在途识别（Windows 上
        onnxruntime DLL 被占用会导致 uv remove 失败），模型加载与推理阶段均响应取消。
        """
        cancel_id = params.get("cancel_id", "")
        cancel_event = cancel_registry.register(cancel_id) if cancel_id else None
        try:
            return await self._ocr_recognize_impl(params, cancel_event)
        finally:
            # finally 而非 except Exception：命令级超时的 CancelledError 也要注销
            if cancel_id:
                cancel_registry.unregister(cancel_id)

    async def _ocr_recognize_impl(self, params: dict, cancel_event: Any) -> dict:
        image_base64 = params.get("image_base64", "")
        if not image_base64:
            raise WorkerError(Outcome.UNKNOWN_ERROR, "ocr_recognize 缺少 image_base64")

        def _ensure_not_cancelled(stage: str) -> None:
            if cancel_event is not None and cancel_event.is_set():
                raise StepCancelled(f"OCR 识别已取消（{stage}）")

        deadline = time.monotonic() + OCR_TIMEOUT_SECS

        def remaining_timeout() -> float:
            return max(0.0, deadline - time.monotonic())

        _ensure_not_cancelled("模型加载前")
        try:
            # 模型构造（DdddOcr()）同步加载 onnx 模型，可能首次加载较慢。
            # 与后续 classification 共用 OCR_TIMEOUT_SECS 总预算，避免两阶段各吃满一次超时。
            remaining = remaining_timeout()
            if remaining <= 0:
                raise asyncio.TimeoutError
            ocr = await asyncio.wait_for(
                asyncio.to_thread(_get_ocr, bool(params.get("old", False))),
                timeout=remaining,
            )
        except asyncio.TimeoutError:
            raise WorkerError(
                Outcome.UNKNOWN_ERROR,
                f"OCR 处理超时（>{OCR_TIMEOUT_SECS}s，模型加载阶段）。"
                "若持续超时请检查 OCR 依赖是否完整",
            ) from None
        except WorkerError:
            raise
        except Exception as exc:  # noqa: BLE001
            raise WorkerError(
                Outcome.UNKNOWN_ERROR,
                f"ddddocr 未安装或加载失败: {exc}。请在设置页安装 OCR 依赖后重试",
            ) from exc
        _ensure_not_cancelled("推理前")
        try:
            img_bytes = base64.b64decode(image_base64)
        except Exception as exc:  # noqa: BLE001
            raise WorkerError(Outcome.UNKNOWN_ERROR, f"图片解码失败: {exc}") from exc
        # 截图/上传图片可能是 RGBA，先规整为 ddddocr 友好的 RGB，提升识别准确率
        img_bytes = _preprocess_ocr_image(img_bytes)
        # classification 是同步 CPU 推理，丢到线程池避免阻塞事件循环；
        # 只使用模型加载后的剩余预算，保证单次 OCR 的墙钟上限稳定。
        try:
            remaining = remaining_timeout()
            if remaining <= 0:
                raise asyncio.TimeoutError
            text = await asyncio.wait_for(
                asyncio.to_thread(ocr.classification_with_timeout, img_bytes, remaining),
                timeout=remaining,
            )
        except asyncio.TimeoutError:
            raise WorkerError(
                Outcome.UNKNOWN_ERROR, f"OCR 处理超时（>{OCR_TIMEOUT_SECS}s）"
            ) from None
        except Exception as exc:  # noqa: BLE001 — ddddocr/PIL 图片不可识别等，转为一句话
            raise WorkerError(
                Outcome.UNKNOWN_ERROR,
                f"无法识别该图片: {exc}。请使用清晰的标准图片（png/jpg），"
                "避免 webp/avif/截图边缘裁剪等 PIL 不支持的格式",
            ) from exc
        logger.info("[ocr] 识别完成，结果长度=%d", len(text or ""))
        return {"text": text}

    async def handle_shutdown(self, params: dict) -> dict:
        """关闭 Worker：置位 shutdown_event，主循环随后退出。"""
        if self.shutdown_event is not None:
            self.shutdown_event.set()
        return {}


# 模块级取消注册表与 Worker 实例
cancel_registry = CancelRegistry()
worker_core = WorkerCore()


# 命令注册表：method 名 → 处理器
COMMANDS: dict[str, Callable] = {
    "worker_health_check": worker_core.handle_worker_health_check,
    "browser_health_check": worker_core.handle_browser_health_check,
    "test_redirect": worker_core.handle_test_redirect,
    "execute_login_attempt": worker_core.handle_execute_login_attempt,
    "execute_browser_task": worker_core.handle_execute_browser_task,
    "close_browser": worker_core.handle_close_browser,
    "debug_start": worker_core.handle_debug_start,
    "debug_step": worker_core.handle_debug_step,
    "debug_run_all": worker_core.handle_debug_run_all,
    "debug_stop": worker_core.handle_debug_stop,
    "debug_status": worker_core.handle_debug_status,
    "feedback_capture": worker_core.handle_feedback_capture,
    "page_capture": worker_core.handle_page_capture,
    "ocr_recognize": worker_core.handle_ocr_recognize,
    "shutdown": worker_core.handle_shutdown,
}


# 默认反检测初始化脚本（stealth_mode 启用且无自定义脚本时使用）
_STEALTH_INIT_SCRIPT = """
() => {
  try {
    Object.defineProperty(navigator, 'webdriver', { get: () => undefined });
    Object.defineProperty(navigator, 'plugins', { get: () => [1, 2, 3, 4, 5] });
    Object.defineProperty(navigator, 'languages', { get: () => ['zh-CN', 'zh', 'en'] });
  } catch (e) {}
}
"""
