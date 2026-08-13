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
import threading
import time
import uuid
from contextlib import asynccontextmanager
from dataclasses import dataclass
from pathlib import Path
from typing import Any, AsyncIterator, Callable

from models import (
    Outcome,
    StepConfig,
    StructuredResult,
    TaskConfig,
)
from step_handlers import StepCancelled, StepContext, WorkerError, _check_cancel, run_step_async
from variable_resolver import resolve

logger = logging.getLogger(__name__)


def _to_ms(bs: dict, key: str, default_ms: int) -> int:
    """从 browser_settings 读取超时并归一化为毫秒。

    Rust 侧 ``BrowserSettings`` 中 ``timeout`` / ``navigation_timeout`` 单位是秒，
    而 Playwright API 需要毫秒。本函数把 <1000 的值视为秒并 ×1000，
    >=1000 的值视为毫秒直接返回（兼容旧配置或已转换的调用方）。
    """
    val = bs.get(key, default_ms)
    try:
        ival = int(val)
    except (TypeError, ValueError):
        return default_ms
    return ival * 1000 if ival < 1000 else ival

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
    from playwright.async_api import TimeoutError as PlaywrightTimeoutError  # noqa: F811
    duration_ms = int((time.perf_counter() - start) * 1000)
    return StructuredResult(
        outcome=outcome.value,
        message=message,
        duration_ms=duration_ms,
        screenshots=list(context.screenshots),
    )


async def run_steps(page: Any, steps: list[StepConfig], context: StepContext) -> StructuredResult:
    """按序执行步骤列表。"""
    start = time.perf_counter()
    if not steps:
        return _build_result(Outcome.UNKNOWN_ERROR, "任务未包含任何步骤，无法执行", context, start)
    try:
        for idx, step in enumerate(steps):
            _check_cancel(context)
            if idx > 0 and context.step_delay > 0:
                await asyncio.sleep(context.step_delay)
            try:
                await run_step_async(page, step, context)
            except WorkerError as exc:
                if step.required:
                    raise
                logger.warning(f"步骤 {step.id} 失败但非必须，继续执行: {exc.message}")
        return _build_result(Outcome.SUCCESS, "执行成功", context, start)
    except StepCancelled:
        return _build_result(Outcome.CANCELLED, "执行已取消", context, start)
    except WorkerError as exc:
        return _build_result(Outcome(exc.outcome), exc.message, context, start)
    except Exception as exc:
        logger.exception(f"步骤执行未预期异常")
        return _build_result(Outcome.UNKNOWN_ERROR, f"执行异常: {exc}", context, start)


# ── 浏览器环境探测（原 playwright_bootstrap.py）──


def _ensure_browser(channel: str = "playwright") -> bool:
    """确保目标浏览器可用。系统浏览器直接视为就绪。"""
    if channel in ("msedge", "chrome", "custom"):
        return True
    try:
        import playwright  # noqa: F401
    except Exception:
        return False
    if channel == "firefox":
        import shutil
        if shutil.which("firefox") is not None:
            return True
    try:
        from playwright.sync_api import sync_playwright
        with sync_playwright() as p:
            executable = p.chromium.executable_path
            if executable and Path(executable).exists():
                return True
    except Exception:
        pass
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


# ── 调试会话 ──


@dataclass
class DebugSession:
    """调试会话状态。"""

    session_id: str
    page: Any
    task_config: TaskConfig
    context: StepContext
    cancel_id: str = ""
    cancel_event: threading.Event | None = None
    # 自动步进游标：前端“下一步”无显式索引时，按顺序执行尚未运行的步骤
    current_step: int = 0


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

    # ── 浏览器启动参数构建 ──

    def _build_launch_args(self, bs: dict, channel: str = "playwright") -> list[str]:
        """构建浏览器启动参数。"""
        args: list[str] = []
        if channel != "firefox":
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
                if channel == "firefox" and flag in self._CHROMIUM_ONLY_FLAGS:
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
            engine = {"firefox": "firefox", "webkit": "webkit"}.get(engine, "chromium")
            return getattr(playwright, engine), custom_path
        if channel == "firefox":
            return playwright.firefox, None
        # playwright / msedge / chrome 等系统浏览器均走 chromium
        return playwright.chromium, None

    async def _launch_browser(self, playwright, channel, custom_path, headless, launch_args):
        """启动非持久化浏览器。"""
        launcher, resolved_path = self._resolve_launcher(playwright, channel, custom_path)
        kwargs: dict[str, Any] = {"headless": headless, "args": launch_args}
        if resolved_path:
            kwargs["executable_path"] = resolved_path
        elif channel not in ("firefox", "playwright"):
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
        elif channel not in ("firefox", "playwright"):
            kwargs["channel"] = channel
        return await launcher.launch_persistent_context(user_data_dir, **kwargs)

    async def _apply_stealth_and_routes(self, bs: dict) -> None:
        """应用反检测脚本和路由拦截。"""
        if self._context is None:
            return
        if bs.get("low_resource_mode", False):
            await self._context.route("**/*", self._handle_low_resource_request)
        if bs.get("stealth_mode", False):
            custom = bs.get("stealth_custom_script", "").strip()
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
                from pathlib import Path as _P

                user_data_dir = _P("browser_data") / channel
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
                ctx_opts = {
                    "viewport": {
                        "width": int(bs.get("viewport_width", 1280)),
                        "height": int(bs.get("viewport_height", 720)),
                    }
                }
                self._context = await self._browser.new_context(**ctx_opts)
            else:
                launch_args = self._build_launch_args(bs, channel)
                self._browser = await self._launch_browser(
                    self._playwright, channel, custom_path, headless, launch_args
                )
                ctx_opts = self._build_context_options(bs)
                self._context = await self._browser.new_context(**ctx_opts)
                await self._apply_stealth_and_routes(bs)

            self._page = await self._context.new_page()
        except Exception:
            logger.warning("浏览器启动失败，回滚资源", exc_info=True)
            await self._close_browser()
            raise

    async def ensure_browser(self, config: dict) -> None:
        """确保浏览器就绪（复用已存在的实例，仅在未就绪或配置变更时重建）。"""
        bs = config.get("browser_settings", {})
        has_browser = self._browser is not None or self._context is not None
        if has_browser and await self._health_check() and self._last_browser_settings == bs:
            return
        await self._close_browser()
        await self._start_browser(config)

    async def _health_check(self) -> bool:
        """检查浏览器健康状态。"""
        if self._browser is None:
            if self._context is None:
                return False
            try:
                _ = self._context.pages
                return True
            except Exception:
                return False
        try:
            return self._browser.is_connected()
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

    async def _close_browser(self) -> None:
        """关闭浏览器并释放所有资源。"""
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
        self._last_browser_settings = None

    async def close_browser(self) -> None:
        """关闭浏览器并释放所有资源（公开接口，供外部清理调用）。"""
        await self._close_browser()

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
            emit=self.emit,
        )

    async def _navigate(self, page: Any, url: str, nav_timeout: int) -> None:
        """导航到 URL，超时映射为导航错误。"""
        from playwright.async_api import TimeoutError as PlaywrightTimeoutError

        try:
            await page.goto(url, wait_until="domcontentloaded", timeout=nav_timeout)
        except PlaywrightTimeoutError as exc:
            raise WorkerError(Outcome.NAVIGATION_TIMEOUT, f"导航超时: {url}") from exc
        except Exception as exc:  # noqa: BLE001
            raise WorkerError(Outcome.NETWORK_ERROR, f"导航失败: {exc}") from exc

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
        await self.ensure_browser({"browser_settings": bs})
        if self._page is None or self._page.is_closed():
            if self._context is None:
                raise WorkerError(Outcome.UNKNOWN_ERROR, "浏览器页面初始化失败")
            self._page = await self._context.new_page()

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
                logger.info(f"重试复用页面：reload url={target}")
                try:
                    await self._page.reload(
                        wait_until="domcontentloaded", timeout=nav_timeout
                    )
                except Exception as exc:  # noqa: BLE001
                    logger.warning(f"reload 失败，回退到 goto: {exc}")
                    await self._navigate(self._page, target, nav_timeout)
            else:
                await self._navigate(self._page, target, nav_timeout)

        context = self._make_context(
            self._page, variables, bs, cancel_event, screenshot_dir, task_config
        )
        result = await run_steps(self._page, task_config.steps, context)
        # success_condition 成功判定：声明变量名时，从 store_as 结果取变量真值判定，
        # 覆盖默认的"步骤全部成功即成功"兜底（对齐原项目 v4.2.3 _check_success）。
        var_name = (task_config.success_condition or "").strip()
        if var_name and result.outcome == Outcome.SUCCESS.value:
            if var_name not in context.results:
                return _build_result(
                    Outcome.UNKNOWN_ERROR,
                    f"成功条件变量未设置: {var_name}（请检查 eval 步骤的 store_as）",
                    context,
                    time.perf_counter(),
                )
            value = context.results[var_name]
            if not _is_truthy(value):
                return _build_result(
                    Outcome.UNKNOWN_ERROR,
                    f"成功条件未命中: {var_name}={value}",
                    context,
                    time.perf_counter(),
                )
            logger.info("[success_condition] 命中成功: %s=%s", var_name, value)
            result.message = f"成功条件命中: {var_name}={value}"
        return result

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

    async def handle_browser_health_check(self, params: dict, core: "WorkerCore") -> dict:
        """健康检查：确认 Playwright 与浏览器可用。"""
        channel = (params.get("browser_settings") or {}).get("browser_channel", "playwright")
        try:
            healthy = _ensure_browser(channel)
        except Exception as exc:
            logger.warning(f"健康检查异常: {exc}")
            healthy = False
        return {"healthy": healthy}

    async def handle_execute_login_attempt(self, params: dict, core: "WorkerCore") -> dict:
        """执行完整登录流程。"""
        async with self._cancel_session(params) as (cancel_event, bs, task):
            auth_url = params.get("auth_url", "")
            variables = {
                "USERNAME": params.get("username", ""),
                "PASSWORD": params.get("password", ""),
                "ISP": params.get("isp", ""),
                "LOGIN_URL": auth_url,
            }
            variables.update(task.variables or {})
            result = await self._run_task(
                task, bs, variables, cancel_event, Path("debug"), navigate_url=auth_url
            )
            return result.to_dict()

    async def handle_execute_browser_task(self, params: dict, core: "WorkerCore") -> dict:
        """执行浏览器任务（不含账号密码语义）。"""
        async with self._cancel_session(params) as (cancel_event, bs, task):
            variables = dict(task.variables or {})
            result = await self._run_task(
                task, bs, variables, cancel_event, Path("debug")
            )
            return result.to_dict()

    async def handle_debug_start(self, params: dict, core: "WorkerCore") -> dict:
        """启动调试会话，保留浏览器上下文供后续 debug_step 复用。"""
        bs = params.get("browser_settings", {}) or {}
        task_raw = params.get("task_config", {}) or {}
        cancel_id = params.get("cancel_id", "")
        session_id = uuid.uuid4().hex
        cancel_event = cancel_registry.register(cancel_id) if cancel_id else None
        try:
            task = TaskConfig.from_dict(task_raw)
            await self.ensure_browser({"browser_settings": bs})
            if self._page is None or self._page.is_closed():
                if self._context is None:
                    raise WorkerError(Outcome.UNKNOWN_ERROR, "浏览器页面初始化失败")
                self._page = await self._context.new_page()
            variables = dict(task.variables or {})
            if task.url:
                await self._navigate(
                    self._page, resolve(task.url, variables),
                    _to_ms(bs, "navigation_timeout", 15000),
                )
            context = self._make_context(
                self._page, variables, bs, cancel_event, Path("debug"), task
            )
            self._debug_sessions[session_id] = DebugSession(
                session_id=session_id,
                page=self._page,
                task_config=task,
                context=context,
                cancel_id=cancel_id,
                cancel_event=cancel_event,
            )
            # 初始截图
            try:
                stamp = str(int(time.time() * 1000))
                filename = f"debug_{session_id}_{stamp}.png"
                local_path = str(Path("debug") / filename)
                Path("debug").mkdir(parents=True, exist_ok=True)
                await self._page.screenshot(path=local_path, full_page=True)
                # 追踪初始截图路径，以便会话结束时统一清理（历史遗留 F5）
                try:
                    context.screenshots.append(local_path)
                except Exception:  # noqa: BLE001
                    pass
                self.emit("screenshot", {"path": local_path})
            except Exception as exc:  # noqa: BLE001
                logger.debug(f"调试初始截图失败: {exc}")
            return {"session_id": session_id}
        except Exception:
            if cancel_id:
                cancel_registry.unregister(cancel_id)
            raise

    async def handle_debug_step(self, params: dict, core: "WorkerCore") -> dict:
        """执行调试会话中的单个步骤。

        优先级：
        - 提供 `step`（完整 StepConfig）→ 执行该步；
        - 提供 `step_index`（整数）→ 执行该索引处的步骤；
        - 两者皆无 → 自动执行“下一步”（由会话内游标维护），便于前端逐步调试。
        """
        session_id = params.get("session_id", "")
        session = self._debug_sessions.get(session_id)
        if not session_id or session is None:
            raise WorkerError(Outcome.UNKNOWN_ERROR, "调试会话不存在，请先启动调试")

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
                return StructuredResult(
                    outcome=Outcome.SUCCESS.value, message="所有步骤已执行完毕"
                ).to_dict()
            step = steps[idx]
            auto_advance = True

        try:
            await run_step_async(session.page, step, session.context)
        except StepCancelled:
            return StructuredResult(
                outcome=Outcome.CANCELLED.value, message="调试步骤已取消"
            ).to_dict()
        except WorkerError as exc:
            return StructuredResult(
                outcome=exc.outcome, message=exc.message
            ).to_dict()
        if auto_advance and idx is not None:
            session.current_step = idx + 1
        return StructuredResult(outcome=Outcome.SUCCESS.value, message="步骤执行成功").to_dict()

    async def handle_debug_run_all(self, params: dict, core: "WorkerCore") -> dict:
        """依次执行调试会话中尚未运行的全部步骤（从当前游标到末尾）。"""
        session_id = params.get("session_id", "")
        session = self._debug_sessions.get(session_id)
        if not session_id or session is None:
            raise WorkerError(Outcome.UNKNOWN_ERROR, "调试会话不存在，请先启动调试")

        steps = session.task_config.steps
        start = session.current_step
        if start >= len(steps):
            return StructuredResult(
                outcome=Outcome.SUCCESS.value, message="所有步骤已执行完毕"
            ).to_dict()
        try:
            for idx in range(start, len(steps)):
                step = steps[idx]
                session.current_step = idx
                await run_step_async(session.page, step, session.context)
            session.current_step = len(steps)
        except StepCancelled:
            return StructuredResult(
                outcome=Outcome.CANCELLED.value, message="调试步骤已取消"
            ).to_dict()
        except WorkerError as exc:
            return StructuredResult(
                outcome=exc.outcome, message=exc.message
            ).to_dict()
        return StructuredResult(
            outcome=Outcome.SUCCESS.value, message="全部步骤执行成功"
        ).to_dict()

    @staticmethod
    def _cleanup_debug_screenshots(session: "DebugSession") -> None:
        """删除调试会话期间产生的截图文件。

        调试截图可能包含表单中的明文凭证，会话结束后及时清除，
        避免长期驻留磁盘（历史遗留 F5）。
        """
        paths = list(getattr(session.context, "screenshots", []) or [])
        for p in paths:
            try:
                Path(p).unlink(missing_ok=True)
            except Exception as exc:  # noqa: BLE001
                logger.debug(f"清理调试截图失败 {p}: {exc}")
        try:
            session.context.screenshots.clear()
        except Exception:  # noqa: BLE001
            pass

    async def handle_debug_stop(self, params: dict, core: "WorkerCore") -> dict:
        """停止调试会话并关闭浏览器。"""
        session_id = params.get("session_id", "")
        session = self._debug_sessions.pop(session_id, None)
        if session is not None:
            # 先清理本会话的截图（可能含明文凭证），再注销取消项
            self._cleanup_debug_screenshots(session)
            if session.cancel_id:
                cancel_registry.unregister(session.cancel_id)
        await self._close_browser()
        return {}

    async def handle_ocr_recognize(self, params: dict, core: "WorkerCore") -> dict:
        """识别 base64 图片中的文本（ddddocr）。"""
        image_base64 = params.get("image_base64", "")
        if not image_base64:
            raise WorkerError(Outcome.UNKNOWN_ERROR, "ocr_recognize 缺少 image_base64")
        try:
            import ddddocr  # type: ignore
        except Exception as exc:  # noqa: BLE001
            raise WorkerError(Outcome.UNKNOWN_ERROR, f"ddddocr 未安装: {exc}") from exc
        try:
            img_bytes = base64.b64decode(image_base64)
        except Exception as exc:  # noqa: BLE001
            raise WorkerError(Outcome.UNKNOWN_ERROR, f"图片解码失败: {exc}") from exc
        ocr = ddddocr.DdddOcr(old=bool(params.get("old", False)), show_ad=False)
        text = ocr.classification(img_bytes)
        return {"text": text}

    async def handle_shutdown(self, params: dict, core: "WorkerCore") -> dict:
        """关闭 Worker：置位 shutdown_event，主循环随后退出。"""
        if self.shutdown_event is not None:
            self.shutdown_event.set()
        return {}


# 模块级取消注册表与 Worker 实例
cancel_registry = CancelRegistry()
worker_core = WorkerCore()


# 命令注册表：method 名 → 处理器
COMMANDS: dict[str, Callable] = {
    "browser_health_check": worker_core.handle_browser_health_check,
    "execute_login_attempt": worker_core.handle_execute_login_attempt,
    "execute_browser_task": worker_core.handle_execute_browser_task,
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
