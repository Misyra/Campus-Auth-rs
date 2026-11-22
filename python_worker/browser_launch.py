"""浏览器上下文准备：启动参数构建、launcher 解析与 stealth/直连/资源路由（WorkerCore 拆分）。

自 playwright_worker.py 迁出（Mixin 化）：方法签名与行为逐字段保留，经
WorkerCore 多继承挂回（见 playwright_worker.py 的组装）。本模块同时承载两份
跨模块契约常量：

- ``_to_ms`` / ``_nav_timeout``：Rust ``BrowserSettings``（秒）→ Playwright（毫秒）
  的超时换算，非正值统一回退默认——worker_main._command_timeout 经门面再导出
  共用同一份；
- ``_channel_supports_cdp``：「渠道是否 Chromium 系」的唯一判定口径，启动参数
  过滤与捕获路径（capture_kit / capture_commands）共用，避免两处漂移。

补丁语义约定：本模块名下符号被测试 monkeypatch 时统一打在 browser_launch 上。
"""

from __future__ import annotations

import json
import logging
from pathlib import Path
from typing import Any

logger = logging.getLogger(__name__)

# 仅 Chromium 系支持的启动参数（原 WorkerCore 类属性，随方法一同迁出）
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
    "--proxy-pac-url",
    "--proxy-auto-detect",
    "--no-proxy-server",
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


def _to_ms(bs: dict, key: str, default_ms: int) -> int:
    """从 browser_settings 读取超时并归一化为毫秒。

    Rust 侧 ``BrowserSettings`` 中 ``timeout`` / ``navigation_timeout`` 为
    u32 秒，而 Playwright API 需要毫秒，统一 ×1000；缺省值已是毫秒，原样返回。
    key 缺失、非法值与**非正值（≤0）**统一回退 ``default_ms``：0 会让
    Playwright 每个操作瞬间超时失败（如 close_browser 的 timeout=0 直接把
    所有步骤打成 1ms 超时），此处一处保护所有消费点。
    """
    if not isinstance(bs, dict):
        # 畸形 browser_settings（非 dict）不得让超时推导抛 AttributeError
        # 杀死 Worker（_command_timeout 在 _dispatch_guarded 的 try 之外），
        # 统一回退默认值
        return default_ms
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


class BrowserLaunchMixin:
    """浏览器启动参数与 launcher 解析（WorkerCore 的上下文准备职责）。"""

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
                if not is_chromium and flag in _CHROMIUM_ONLY_FLAGS:
                    continue
                flag_name = flag.split("=", 1)[0]
                if flag_name in _BLOCKED_BROWSER_ARGS:
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
        policy = bs.get("network_policy")
        if isinstance(policy, dict):
            if policy.get("mode") == "proxy" and policy.get("proxy"):
                ctx_opts["proxy"] = policy["proxy"]
        elif bs.get("bind_proxy"):
            ctx_opts["proxy"] = {"server": bs["bind_proxy"]}
        return ctx_opts

    @staticmethod
    def _bypass_system_proxy(bs: dict) -> bool:
        """统一出口优先；缺省时兼容旧版未绑定代理即直连的行为。"""
        policy = bs.get("network_policy")
        if isinstance(policy, dict):
            return policy.get("mode") == "direct"
        return not bs.get("bind_proxy")

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

    def _network_launch_options(self, bs: dict, channel: str) -> dict[str, Any]:
        """Firefox 使用偏好设置切换直连与系统代理，避免持久配置残留。"""
        is_firefox = channel == "firefox" or (
            channel == "custom" and bs.get("custom_browser_engine") == "firefox"
        )
        if not is_firefox:
            return {}
        policy = bs.get("network_policy")
        if self._bypass_system_proxy(bs):
            return {"firefox_user_prefs": {"network.proxy.type": 0}}
        if isinstance(policy, dict) and policy.get("mode") == "system":
            return {"firefox_user_prefs": {"network.proxy.type": 5}}
        return {}

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
        kwargs.update(self._network_launch_options(bs or {}, channel))
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
        kwargs.update(self._network_launch_options(bs or {}, channel))
        if resolved_path:
            kwargs["executable_path"] = resolved_path
        elif channel in ("msedge", "chrome"):
            kwargs["channel"] = channel
        return await launcher.launch_persistent_context(user_data_dir, **kwargs)

    async def _apply_stealth_and_routes(self, bs: dict, context: Any = None) -> None:
        """应用反检测脚本和路由拦截（context 缺省时作用于自动化会话上下文）。"""
        if context is None:
            context = self._context
        if context is None:
            return
        if bs.get("low_resource_mode", False):
            await context.route("**/*", self._handle_low_resource_request)
        if bs.get("stealth_mode", False):
            # 显式 null 时 .get 默认值不生效，需 or 兜底再 strip
            custom = (bs.get("stealth_custom_script") or "").strip()
            script = custom or _STEALTH_INIT_SCRIPT
            await context.add_init_script(script)

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


__all__ = [
    "BrowserLaunchMixin",
    "_STEALTH_INIT_SCRIPT",
    "_channel_supports_cdp",
    "_nav_timeout",
    "_to_ms",
]
