"""Playwright Worker 核心：浏览器生命周期管理 + 命令处理。

本模块是 Python 侧浏览器自动化执行平面。Rust 主进程通过 NDJSON IPC
调用此处注册的处理器（见 ``COMMANDS``），每个处理器对应一种命令：
browser_health_check / execute_login_attempt / execute_browser_task /
debug_start / debug_step / debug_stop / ocr_recognize / shutdown。

Worker 仅作为"浏览器动作执行器"，单次动作返回 StructuredResult；
重试、状态机、取消调度由 Rust 侧负责。取消通过 ``cancel_id`` 映射到
``threading.Event``，处理器在步骤边界检查该事件。
"""

from __future__ import annotations

import asyncio
import base64
import logging
import os
import threading
import time
import uuid
from contextlib import asynccontextmanager
from pathlib import Path
from typing import Any, AsyncIterator, Callable

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
    run_step_async,
)
from ocr_runtime import OCR_TIMEOUT_SECS, _get_ocr, _preprocess_ocr_image
from debug_session import DebugSession, _build_steps_info
from variable_resolver import resolve

logger = logging.getLogger(__name__)

# Worker 脚本所在目录：debug 截图等相对目录一律锚定到此，
# 避免依赖 Rust spawn 继承的 CWD（未设 current_dir，可能是任意目录）
_WORKER_DIR = Path(__file__).resolve().parent

# Worker 版本（任务 10）：与 pyproject.toml 的 project.version 保持同步（手动维护），
# 随 worker_health_check 响应上报给 Rust 侧。
WORKER_VERSION = "5.0.0a1"


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


def _debug_screenshot_dir() -> Path:
    """调试截图目录（锚定到 worker 脚本目录，不依赖进程 CWD）。"""
    return _WORKER_DIR / "debug"


# 模块加载时刻：启动清理时用于判定“上次会话残留”（mtime 早于该时刻的文件）
_MODULE_LOAD_TIME = time.time()


def _purge_stale_debug_screenshots() -> None:
    """Worker 启动时清理上次会话残留的截图文件（A7）。

    Worker 进程被强杀时，任务级（_run_task）与调试级（_cleanup_debug_screenshots）
    清理均不会执行，debug/ 目录会残留可能含明文凭据的截图。启动时
    best-effort 删除修改时间早于本进程启动（模块加载时刻）的 ``*.png``；
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
            if entry.suffix.lower() != ".png" or not entry.is_file():
                continue
            if entry.stat().st_mtime >= _MODULE_LOAD_TIME:
                continue
            entry.unlink(missing_ok=True)
            logger.info(f"已清理上次会话残留截图: {entry.name}")
        except Exception as exc:  # noqa: BLE001
            logger.debug(f"清理残留截图失败 {entry}: {exc}")


def _to_ms(bs: dict, key: str, default_ms: int) -> int:
    """从 browser_settings 读取超时并归一化为毫秒。

    Rust 侧 ``BrowserSettings`` 中 ``timeout`` / ``navigation_timeout`` 为
    u32 秒，而 Playwright API 需要毫秒，统一 ×1000；缺省值已是毫秒，原样返回。
    """
    val = bs.get(key)
    if val is None:
        return default_ms
    try:
        ival = int(val)
    except (TypeError, ValueError):
        return default_ms
    return ival * 1000

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


async def _sleep_cancellable(seconds: float, context: StepContext) -> None:
    """分片 sleep，每片检查取消事件，避免长时间延迟期间无法响应取消。"""
    slice_s = 0.2
    deadline = time.monotonic() + max(0.0, seconds)
    while True:
        _check_cancel(context)
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            return
        await asyncio.sleep(min(slice_s, remaining))


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
    except StepCancelled:
        return _build_result(Outcome.CANCELLED, "执行已取消", context, start)
    except WorkerError as exc:
        return _build_result(Outcome(exc.outcome), exc.message, context, start)
    except Exception as exc:
        logger.exception("步骤执行未预期异常")
        return _build_result(Outcome.UNKNOWN_ERROR, f"执行异常: {exc}", context, start)


# ── 浏览器环境探测（原 playwright_bootstrap.py）──


def _ensure_browser(channel: str = "playwright") -> bool:
    """确保目标浏览器可用；Playwright 管理的引擎按实际 executable 检测。"""
    if channel in ("msedge", "chrome", "custom"):
        return True
    try:
        from playwright.sync_api import sync_playwright

        with sync_playwright() as p:
            if channel == "firefox":
                browser_type = p.firefox
            elif channel == "webkit":
                browser_type = p.webkit
            else:
                browser_type = p.chromium
            executable = browser_type.executable_path
            return bool(executable and Path(executable).exists())
    except Exception:
        return False


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

    # ── 浏览器启动参数构建 ──

    def _build_launch_args(self, bs: dict, channel: str = "playwright") -> list[str]:
        """构建浏览器启动参数；非 Chromium 引擎过滤 Chromium-only 参数。"""
        custom_engine = (bs.get("custom_browser_engine") or "auto").strip().lower()
        is_chromium = channel not in ("firefox", "webkit")
        if channel == "custom":
            is_chromium = custom_engine not in ("firefox", "webkit")

        args: list[str] = []
        if is_chromium:
            args.extend(
                [
                    "--no-sandbox",
                    "--disable-dev-shm-usage",
                    "--disable-gpu",
                    "--memory-pressure-off",
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
        import json

        raw = str(bs.get("extra_headers_json", "") or "").strip()
        if not raw:
            return {}
        try:
            headers = json.loads(raw)
            if isinstance(headers, dict):
                result: dict[str, str] = {}
                for k, v in headers.items():
                    if k is None:
                        continue
                    k_str, v_str = str(k), str(v)
                    if len(k_str) > 256 or len(v_str) > 4096:
                        logger.warning(f"请求头过长，已跳过: {k_str[:32]}")
                        continue
                    if "\r" in k_str or "\n" in k_str:
                        logger.warning(f"请求头 key 含换行符，已跳过: {k_str[:32]}")
                        continue
                    result[k_str] = v_str
                return result
            logger.warning("自定义请求头格式无效: 应为 JSON 对象，已忽略")
        except Exception as exc:  # noqa: BLE001
            logger.warning(f"解析自定义请求头失败: {exc}")
        return {}

    def _resolve_launcher(self, playwright, channel: str, custom_path: str):
        """根据 channel 解析对应的 launcher 对象。"""
        if channel == "custom" and custom_path:
            if not Path(custom_path).exists():
                raise FileNotFoundError(f"自定义浏览器路径不存在: {custom_path}")
            engine = (self._last_browser_settings or {}).get("custom_browser_engine", "auto")
            engine = engine if engine in ("firefox", "webkit") else "chromium"
            return getattr(playwright, engine), custom_path
        if channel == "firefox":
            return playwright.firefox, None
        if channel == "webkit":
            return playwright.webkit, None
        # playwright / chromium / msedge / chrome 走 Chromium launcher。
        return playwright.chromium, None

    async def _launch_browser(self, playwright, channel, custom_path, headless, launch_args):
        """启动非持久化浏览器。"""
        launcher, resolved_path = self._resolve_launcher(playwright, channel, custom_path)
        kwargs: dict[str, Any] = {"headless": headless, "args": launch_args}
        if resolved_path:
            kwargs["executable_path"] = resolved_path
        elif channel in ("msedge", "chrome"):
            kwargs["channel"] = channel
        return await launcher.launch(**kwargs)

    async def _launch_persistent_context(
        self, playwright, channel, custom_path, headless, launch_args, user_data_dir, ctx_opts
    ):
        """启动持久化上下文浏览器（保留 cookies）。"""
        launcher, resolved_path = self._resolve_launcher(playwright, channel, custom_path)
        kwargs: dict[str, Any] = {"headless": headless, "args": launch_args, **ctx_opts}
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
        channel = bs.get("browser_channel", "playwright")
        custom_path = bs.get("browser_custom_path", "")

        if self._playwright is None:
            self._playwright = await async_playwright().start()

        persistent = bs.get("persistent_context", False)
        try:
            if persistent:
                user_data_dir = _browser_data_dir() / channel
                user_data_dir.mkdir(parents=True, exist_ok=True)
                launch_args = [] if pure_mode else self._build_launch_args(bs, channel)
                ctx_opts = self._build_context_options(bs)
                self._context = await self._launch_persistent_context(
                    self._playwright, channel, custom_path, headless,
                    launch_args, str(user_data_dir), ctx_opts,
                )
                self._browser = None
                if not pure_mode:
                    await self._apply_stealth_and_routes(bs)
            elif pure_mode:
                self._browser = await self._launch_browser(
                    self._playwright, channel, custom_path, headless, []
                )
                # pure mode 只禁用额外启动参数、stealth 与资源路由；
                # locale/timezone/UA/header/proxy 等 BrowserContext 契约仍应一致生效。
                ctx_opts = self._build_context_options(bs)
                self._context = await self._browser.new_context(**ctx_opts)
            else:
                launch_args = self._build_launch_args(bs, channel)
                self._browser = await self._launch_browser(
                    self._playwright, channel, custom_path, headless, launch_args
                )
                ctx_opts = self._build_context_options(bs)
                self._context = await self._browser.new_context(**ctx_opts)
                await self._apply_stealth_and_routes(bs)

            self._page = await self._new_page()
        except Exception:
            logger.warning("浏览器启动失败，回滚资源", exc_info=True)
            await self.close_browser()
            raise

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
        page.on("dialog", lambda d: asyncio.ensure_future(self._handle_page_dialog(d)))
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
        has_browser = self._browser is not None or self._context is not None
        if has_browser and await self._health_check() and self._last_browser_settings == bs:
            return
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
        except Exception:
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
        if self._browser is not None:
            await self._safe_close(self._browser, "浏览器")
            self._browser = None
        if self._playwright is not None:
            try:
                await self._playwright.stop()
            except Exception:  # noqa: BLE001
                logger.debug("停止 Playwright 时连接已断开（正常）")
            self._playwright = None
        # 关闭浏览器时顺带清理全部调试会话截图（可能含明文凭据），
        # 覆盖 debug_stop 未被正确调用（EOF / shutdown 路径）的泄漏场景
        for session in list(self._debug_sessions.values()):
            self._cleanup_debug_screenshots(session)
        self._debug_sessions.clear()
        self._last_browser_settings = None

    async def force_interrupt_pending(self) -> None:
        """强制中断可能挂起的 Playwright 操作：关闭当前页面以打断 CDP await。

        命令级超时自愈时调用：页面关闭会使挂起的 ``page.evaluate``/``goto`` 等
        以“目标已关闭”异常结束，从而让被取消的任务真正退出，避免残留任务占住
        浏览器资源。不关闭整个浏览器，避免影响后续轻量请求。
        """
        if self._page is not None:
            await self._safe_close(self._page, "页面")
            self._page = None

    async def _handle_low_resource_request(self, route) -> None:
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
            """给 step_progress 事件注入 session_type，供前端区分登录/调试会话。"""
            if event_type == "step_progress" and isinstance(data, dict):
                data = dict(data)
                data["session_type"] = session_type
            self.emit(event_type, data)

        return StepContext(
            page=page,
            variables=variables,
            browser_settings=bs,
            cancel_event=cancel_event,
            screenshot_dir=screenshot_dir,
            default_timeout=_to_ms(bs, "timeout", 10000),
            navigation_timeout=_to_ms(bs, "navigation_timeout", 15000),
            reveal_hidden=task_config.reveal_hidden,
            step_delay=task_config.step_delay,
            emit=_emit,
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
    ) -> StructuredResult:
        """执行单个浏览器任务：确保浏览器 → 导航 → 运行步骤。"""
        start = time.perf_counter()
        self._session_type = "login"
        await self.ensure_browser({"browser_settings": bs})
        if self._page is None or self._page.is_closed():
            if self._context is None:
                raise WorkerError(Outcome.UNKNOWN_ERROR, "浏览器页面初始化失败")
            self._page = await self._new_page()

        context = self._make_context(
            self._page, variables, bs, cancel_event, screenshot_dir, task_config
        )
        target = navigate_url or task_config.url
        if target:
            target = resolve(target, variables)
            nav_timeout = _to_ms(bs, "navigation_timeout", 15000)
            # 重试复用：若页面已存在且 URL 已是目标，直接 reload（更轻、避免重新加载耗时）；
            # 否则正常 goto 导航。设计目标：重试时不重新走完整导航流程，刷新页面即可重试登录动作。
            try:
                current_url = self._page.url
            except Exception:
                current_url = ""
            if current_url and current_url.rstrip("/") == target.rstrip("/"):
                # 目标地址可能包含账号、令牌或其他查询参数，日志只记录动作本身。
                logger.info("重试复用页面：reload")
                try:
                    await self._page.reload(
                        wait_until="domcontentloaded", timeout=nav_timeout
                    )
                except Exception as exc:  # noqa: BLE001
                    logger.warning(f"reload 失败，回退到 goto: {exc}")
                    await self._navigate(self._page, target, nav_timeout)
            else:
                await self._navigate(self._page, target, nav_timeout)
            await self._wait_after_navigation(task_config, context)

        try:
            result = await run_steps(self._page, task_config.steps, context)
            # B5 取舍：任务失败后在共享 context 上清除 cookies，避免上次任务的残留会话
            # （登录态等）污染下一个任务。不重建整个页面/浏览器——那会显著增加下一次
            # 任务的重启开销；在现有 context 复用结构下，清除 cookies 已覆盖绝大多数
            # 跨任务污染场景（登录态隔离）。重试同任务由 Rust 侧重新调用，页面按
            # "_run_task 顶部 reload 复用"逻辑刷新，不受此处影响。
            if result.outcome not in (Outcome.SUCCESS.value, Outcome.CANCELLED.value):
                if self._context is not None:
                    try:
                        await self._context.clear_cookies()
                        logger.info("[_run_task] 任务失败，已清除 context cookies")
                    except Exception as exc:  # noqa: BLE001
                        logger.debug(f"[_run_task] 清除 cookies 失败（忽略）: {exc}")
            # success_condition 成功判定：声明变量名时，从 store_as 结果取变量真值判定，
            # 覆盖默认的"步骤全部成功即成功"兜底（对齐原项目 v4.2.3 _check_success）。
            var_name = (task_config.success_condition or "").strip()
            if var_name and result.outcome == Outcome.SUCCESS.value:
                if var_name not in context.results:
                    return _build_result(
                        Outcome.UNKNOWN_ERROR,
                        f"成功条件变量未设置: {var_name}（请检查 eval 步骤的 store_as）",
                        context,
                        start,
                    )
                value = context.results[var_name]
                if not _is_truthy(value):
                    return _build_result(
                        Outcome.UNKNOWN_ERROR,
                        f"成功条件未命中: {var_name}={value}",
                        context,
                        start,
                    )
                logger.info("[success_condition] 命中成功: %s=%s", var_name, value)
                result.message = f"成功条件命中: {var_name}={value}"
            return result
        finally:
            # A7：登录/浏览器任务截图可能含表单明文凭据，任务结束（成功/失败/
            # 取消/异常等所有退出路径）后 best-effort 删除磁盘文件。截图事件
            # 已在 handle_screenshot 中即时推送（仅携带路径字符串，前端不回读
            # 文件），删除不影响展示链路；debug 会话不走 _run_task，其截图由
            # _cleanup_debug_screenshots（debug_stop / close_browser）清理。
            self._cleanup_task_screenshots(context)

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
        channel = (params.get("browser_settings") or {}).get("browser_channel", "playwright")
        try:
            # _ensure_browser 内部用 sync_playwright，在 asyncio 事件循环内直接调用会抛
            # "Sync API inside the asyncio loop" 被吞掉而误判 healthy=false（Worker 启动超时）。
            # 丢到线程池执行，与 OCR classification 的同步 CPU 推理处理一致。
            healthy = await asyncio.to_thread(_ensure_browser, channel)
        except Exception as exc:
            logger.warning(f"健康检查异常: {exc}")
            healthy = False
        return {"healthy": healthy}

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

    async def handle_execute_login_attempt(self, params: dict) -> dict:
        """执行完整登录流程。"""
        # B3 防御（Python 半）：调试会话持有 Worker 浏览器上下文期间拒绝登录
        # 任务，避免登录重建浏览器把调试会话的 page/context 连根拔掉。
        # Outcome 无 BUSY 变体（新增会破坏与 Rust 的 serde 契约），复用最贴近的
        # UNKNOWN_ERROR（终态失败、不重试），消息中明确说明原因。
        # 根治方案（Rust 侧会话槽位覆盖调试会话整个存活期，而非仅 debug_start
        # 命令期间）另行立项，此处仅作纵深防御。
        if self._debug_sessions:
            raise WorkerError(
                Outcome.UNKNOWN_ERROR, "调试会话进行中，无法执行登录任务，请先停止调试"
            )
        async with self._cancel_session(params) as (cancel_event, bs, task):
            auth_url = params.get("auth_url", "")
            # 任务变量可自定义普通模板值，但系统保留变量必须始终反映当前 Profile。
            variables = dict(task.variables or {})
            variables.update(
                {
                    "USERNAME": params.get("username", ""),
                    "PASSWORD": params.get("password", ""),
                    "ISP": params.get("isp", ""),
                    "LOGIN_URL": auth_url,
                }
            )
            self._task_dialogs = []
            result = await self._run_task(
                task, bs, variables, cancel_event, _debug_screenshot_dir(),
                navigate_url=auth_url,
            )
            result.data = {"dialogs": list(self._task_dialogs)}
            return result.to_dict()

    async def handle_execute_browser_task(self, params: dict) -> dict:
        """执行浏览器任务（不含账号密码语义）。"""
        # B3 防御（Python 半）：同 handle_execute_login_attempt，调试会话存续期
        # 内拒绝浏览器任务，避免上下文互踩。
        if self._debug_sessions:
            raise WorkerError(
                Outcome.UNKNOWN_ERROR, "调试会话进行中，无法执行浏览器任务，请先停止调试"
            )
        async with self._cancel_session(params) as (cancel_event, bs, task):
            variables = dict(task.variables or {})
            self._task_dialogs = []
            result = await self._run_task(
                task, bs, variables, cancel_event, _debug_screenshot_dir()
            )
            result.data = {"dialogs": list(self._task_dialogs)}
            return result.to_dict()

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
        try:
            task = TaskConfig.from_dict(task_raw)
            await self.ensure_browser({"browser_settings": bs})
            if self._page is None or self._page.is_closed():
                if self._context is None:
                    raise WorkerError(Outcome.UNKNOWN_ERROR, "浏览器页面初始化失败")
                self._page = await self._new_page()
            variables = dict(task.variables or {})
            context = self._make_context(
                self._page, variables, bs, cancel_event, _debug_screenshot_dir(), task
            )
            if task.url:
                await self._navigate(
                    self._page, resolve(task.url, variables),
                    _to_ms(bs, "navigation_timeout", 15000),
                )
                await self._wait_after_navigation(task, context)
            self._debug_sessions[session_id] = DebugSession(
                session_id=session_id,
                page=self._page,
                task_config=task,
                context=context,
                cancel_id=cancel_id,
                cancel_event=cancel_event,
                task_id=task.task_id,
                steps_info=_build_steps_info(task),
            )
            # 初始截图
            try:
                stamp = str(int(time.time() * 1000))
                filename = f"debug_{session_id}_{stamp}.png"
                shot_dir = _debug_screenshot_dir()
                local_path = str(shot_dir / filename)
                shot_dir.mkdir(parents=True, exist_ok=True)
                await self._page.screenshot(path=local_path, full_page=True)
                # 追踪初始截图路径，以便会话结束时统一清理（历史遗留 F5）
                try:
                    context.screenshots.append(local_path)
                except Exception:  # noqa: BLE001
                    pass
                self.emit("screenshot", {"path": local_path})
            except Exception as exc:  # noqa: BLE001
                logger.debug(f"调试初始截图失败: {exc}")
            return self._debug_response(self._debug_sessions[session_id])
        except Exception:
            if cancel_id:
                cancel_registry.unregister(cancel_id)
            raise

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
        try:
            await run_step_async(session.page, step, session.context, step_index=idx, total_steps=len(steps))
        except StepCancelled:
            success, message = False, "步骤已取消"
        except WorkerError as exc:
            success, message = False, exc.message
        except Exception as exc:  # noqa: BLE001
            logger.exception("调试步骤执行未预期异常")
            success, message = False, f"执行异常: {exc}"
        if idx is not None:
            self._record_debug_result(session, idx, success, message)
        if auto_advance and idx is not None:
            session.current_step = idx + 1
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
            except StepCancelled:
                success, message, fatal = False, "步骤已取消", True
            except WorkerError as exc:
                success, message = False, exc.message
                fatal = step.required
            except Exception as exc:  # noqa: BLE001
                logger.exception("调试批量执行未预期异常")
                success, message, fatal = False, f"执行异常: {exc}", True
            self._record_debug_result(session, idx, success, message)
            if fatal:
                stop_idx = idx + 1
                break
        session.current_step = stop_idx
        return self._debug_response(session)

    @staticmethod
    def _cleanup_task_screenshots(context: StepContext) -> None:
        """删除登录/浏览器任务期间产生的截图文件（A7）。

        任务截图可能包含表单中的明文凭据，任务结束后及时清除，避免长期
        驻留磁盘。仅删除 ``context.screenshots`` 中记录的文件（每个文件
        best-effort，失败仅记日志不抛出），不递归清理整个 debug/ 目录，
        避免误删其他并发会话的文件。StructuredResult 中的 screenshots
        路径列表在 _build_result 时已快照，清理不影响 IPC 响应内容。
        """
        for p in list(context.screenshots):
            try:
                Path(p).unlink(missing_ok=True)
            except Exception as exc:  # noqa: BLE001
                logger.debug(f"清理任务截图失败 {p}: {exc}")
        # screenshots 是 StepContext 的 list[str] 字段，list.clear() 不会抛出
        context.screenshots.clear()

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

    async def handle_debug_stop(self, params: dict) -> dict:
        """停止调试会话并关闭浏览器。"""
        session_id = params.get("session_id", "")
        session = self._debug_session_for(session_id)
        self._debug_sessions.pop(session.session_id, None)
        # 先清理本会话的截图（可能含明文凭据），再注销取消项
        self._cleanup_debug_screenshots(session)
        if session.cancel_id:
            cancel_registry.unregister(session.cancel_id)
        await self.close_browser()
        return {}

    async def handle_close_browser(self, params: dict) -> dict:
        """关闭浏览器但保留 Worker 进程。

        登录会话到达终态（成功/失败/取消）后由 Rust 侧调用，对齐原版
        BrowserContextManager 的会话级生命周期：会话内重试复用同一浏览器，
        会话结束即关闭。Worker 进程保留，下次登录由 ensure_browser 重建浏览器。

        ``close_browser`` 内含 playwright.stop()，极端情况下可能挂起（如 driver
        进程未及时退出），此处加内部超时兜底，避免一条挂起命令阻塞 Worker 命令队列。
        """
        try:
            await asyncio.wait_for(self.close_browser(), timeout=8.0)
        except asyncio.TimeoutError:
            logger.warning("close_browser 超时（8s），跳过等待继续")
        return {}

    async def handle_ocr_recognize(self, params: dict) -> dict:
        """识别 base64 图片中的文本（ddddocr），模型加载与推理共享总超时预算。"""
        image_base64 = params.get("image_base64", "")
        if not image_base64:
            raise WorkerError(Outcome.UNKNOWN_ERROR, "ocr_recognize 缺少 image_base64")

        deadline = time.monotonic() + OCR_TIMEOUT_SECS

        def remaining_timeout() -> float:
            return max(0.0, deadline - time.monotonic())

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
        except Exception as exc:  # noqa: BLE001
            raise WorkerError(
                Outcome.UNKNOWN_ERROR,
                f"ddddocr 未安装或加载失败: {exc}。请在设置页安装 OCR 依赖后重试",
            ) from exc
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
                asyncio.to_thread(ocr.classification, img_bytes),
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
    "execute_login_attempt": worker_core.handle_execute_login_attempt,
    "execute_browser_task": worker_core.handle_execute_browser_task,
    "close_browser": worker_core.handle_close_browser,
    "debug_start": worker_core.handle_debug_start,
    "debug_step": worker_core.handle_debug_step,
    "debug_run_all": worker_core.handle_debug_run_all,
    "debug_stop": worker_core.handle_debug_stop,
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