"""浏览器生命周期：start/close/keep_alive/空闲回收/生命周期锁与页面弹窗接线（WorkerCore 拆分）。

自 playwright_worker.py 迁出（Mixin 化）：方法签名与行为逐字段保留，经
WorkerCore 多继承挂回（见 playwright_worker.py 的组装）。职责边界：

- ``ensure_browser`` / ``close_browser`` / ``_close_session`` / 空闲回收全部
  收敛到 ``_browser_lifecycle_lock``（WorkerCore.__init__ 声明），命令内的
  步骤执行在锁外，不串行化命令本身；
- 页面接线（``_wire_page`` / ``_wire_context``）与弹窗自动确认（``_handle_page_dialog``）
  ——事件与弹窗的统一出口，任务/调试/会话页共用；
- ``handle_close_browser``：登录会话终态后的三档资源回收命令。

补丁语义约定：``_WORKER_KEEP_ALIVE`` / ``BROWSER_IDLE_RELEASE_SECS`` 被测试
monkeypatch 时统一打在本模块（属主）上。
"""

from __future__ import annotations

import asyncio
import hashlib
import logging
import os
from pathlib import Path
from typing import Any

from models import Outcome
from step_handlers import WorkerError
import worker_paths

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

# 浏览器关闭/会话释放的兜底等待上限（秒）：close 可能挂起（driver 未及时退出等），
# 统一超时跳过，避免单条挂起命令阻塞 Worker 命令队列
_WAIT_TIMEOUT_SECS = 8.0

# 单条弹窗文案上限：门户 alert 可能携带超长 HTML，整段进入 IPC 会撑爆
# Rust 侧 1MiB NDJSON 行上限（整行丢弃 → 成功信封丢失 → 登录误判失败），
# 与 Rust 侧 IPC_LOG_PREVIEW_LEN 同一截断纪律
_DIALOG_MESSAGE_MAX_CHARS = 500

# 浏览器空闲自动释放秒数：worker.keep_alive 未启用时，浏览器类命令完成后若
# 持续无新命令，超过该时长即全量关闭浏览器释放内存（Worker 进程本身仍由
# Rust 侧 worker.idle_timeout_seconds 管理）。连续任务不受影响：每次新命令
# 都会取消并重置计时。
BROWSER_IDLE_RELEASE_SECS = 30

# Rust spawn 时按 cfg.worker.keep_alive 注入（改配置对下一个 Worker 生命周期
# 生效）。True 时浏览器跨会话常驻：登录成功整页保留登录状态（门户页 JS 心跳
# 不中断），非成功终态仅会话级释放，且不装浏览器空闲回收计时器。
_WORKER_KEEP_ALIVE = os.environ.get("CAMPUS_AUTH_WORKER_KEEP_ALIVE", "").strip().lower() in (
    "1",
    "true",
    "yes",
)


class BrowserLifecycleMixin:
    """浏览器实例生命周期与页面接线（WorkerCore 的生命周期职责）。"""

    async def _start_browser(self, config: dict) -> None:
        """启动浏览器（按 browser_channel 选择引擎）。"""
        from playwright.async_api import async_playwright

        bs = config.get("browser_settings", {})
        self._last_browser_settings = bs
        headless = bs.get("headless", True)
        pure_mode = bs.get("pure_mode", False)
        channel = str(bs.get("browser_channel") or "playwright").strip().lower()
        custom_path = str(bs.get("browser_custom_path") or "").strip()
        launch_args = [] if pure_mode else self._build_launch_args(bs, channel)
        # 正式登录也需绕过系统代理；否则检测直连成功后，登录仍可能被失效代理阻断。
        # 纯净模式只关闭可选定制，不应关闭直连保障；显式绑定代理时保留用户选择。
        if self._bypass_system_proxy(bs) and self._is_chromium_channel(bs, channel):
            if "--no-proxy-server" not in launch_args:
                launch_args.append("--no-proxy-server")

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
                user_data_dir = worker_paths._browser_data_dir() / data_key
                user_data_dir.mkdir(parents=True, exist_ok=True)
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
                    self._playwright, channel, custom_path, headless, launch_args, bs
                )
                # pure mode 只禁用额外启动参数、stealth 与资源路由；
                # locale/timezone/UA/header/proxy 等 BrowserContext 契约仍应一致生效。
                ctx_opts = self._build_context_options(bs)
                self._context = await self._browser.new_context(**ctx_opts)
                self._wire_context()
            else:
                self._browser = await self._launch_browser(
                    self._playwright, channel, custom_path, headless, launch_args, bs
                )
                ctx_opts = self._build_context_options(bs)
                self._context = await self._browser.new_context(**ctx_opts)
                self._wire_context()
                await self._apply_stealth_and_routes(bs)

            self._page = await self._new_page()
            logger.info(
                "浏览器已启动: channel=%s, headless=%s, persistent=%s, system_proxy_bypassed=%s",
                channel,
                headless,
                persistent,
                "--no-proxy-server" in launch_args,
            )
        except Exception:  # noqa: BLE001 — 启动环节异常源众多（driver 崩溃/参数非法等），统一回滚后原样上抛
            logger.error("浏览器启动失败，回滚资源", exc_info=True)
            # 仅由 ensure_browser（已持生命周期锁）调用，走锁内关闭避免死锁
            await self._close_browser_locked()
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
                msg = str(message)[:_DIALOG_MESSAGE_MAX_CHARS]
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
        # 任何浏览器命令开始都取消待触发的空闲自动释放。计时协程在关闭段仍持有
        # 任务引用（见 _browser_idle_release），此处 cancel 在睡眠期/等锁期/关闭期
        # 均能送达；关闭可能卡在不可中断的关闭调用上，锁保证其完全退出后命令侧
        # 再检查状态（P2-3）。
        self._cancel_browser_idle_release()
        # 生命周期段整体持锁：空闲回收的关闭与命令侧的就绪判定/重建互斥，
        # 命令看到的浏览器状态不会在判定与使用之间被并发关闭撕开
        async with self._browser_lifecycle_lock:
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
                    await self._close_browser_locked()
            has_browser = self._browser is not None or self._context is not None
            if has_browser and await self._health_check() and self._last_browser_settings == bs:
                return
            logger.info("浏览器未就绪或配置变更，重建浏览器")
            await self._close_browser_locked()
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
        async with self._browser_lifecycle_lock:
            await self._close_browser_locked()

    async def _close_browser_locked(self) -> None:
        """close_browser 的锁内实现（调用方必须已持有生命周期锁）。

        ensure_browser / _start_browser 在临界区内也需要完整关闭，经此入口
        避免对非重入的 asyncio.Lock 二次加锁死锁。
        """
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
        # 注意：本方法只管自动化浏览器（命令回收 / 空闲回收 / 重建回滚共用），
        # 不触碰持久配置会话——配置会话与自动化实例完全独立，仅随 Worker 退出
        # 收尾（见 close_profile_session，worker_main._serve 退出路径调用）。
        self._last_browser_settings = None
        logger.info("浏览器及资源已关闭")

    async def _close_session(self) -> None:
        """会话级释放：关闭页面与非持久化 context，保留浏览器进程供热复用。

        与 close_browser 同持生命周期锁：空闲回收的关闭可能并发进行
        （close_browser 命令到达时回收计时已到期），页面/context 字段不得
        交叉置空。调用方均在命令路径（命令间串行）上，锁不引入新交错。

        persistent_context 模式的 context 即浏览器本体（_browser 为 None），
        关闭 context 等于终止进程，故仅清页面，context 由既有 ensure_browser
        快速路径继续复用。_last_browser_settings 必须保留，热恢复分支依赖它
        判定"配置未变"。
        """
        async with self._browser_lifecycle_lock:
            await self._close_session_locked()

    async def _close_session_locked(self) -> None:
        """_close_session 的锁内实现（调用方必须已持有生命周期锁）。"""
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
        """空闲到期后全量关闭浏览器释放内存（Worker 进程仍由 Rust 空闲超时管理）。

        关闭全程持有任务引用并持生命周期锁：命令侧 ensure_browser 入口的
        cancel（_cancel_browser_idle_release）在睡眠期/等锁期/关闭期均能送达；
        拿到锁后复查状态——等待期间命令可能已重建浏览器，绝不能关掉新实例
        （P2-3）。
        """
        try:
            await asyncio.sleep(BROWSER_IDLE_RELEASE_SECS)
        except asyncio.CancelledError:
            return
        try:
            async with self._browser_lifecycle_lock:
                # 调试会话兜底保护：活跃调试期间绝不回收
                if self._debug_sessions:
                    return
                if self._browser is None and self._context is None:
                    return
                logger.info(
                    "浏览器空闲 %ds，自动释放（worker.keep_alive 未启用）",
                    BROWSER_IDLE_RELEASE_SECS,
                )
                # 超时兜底：挂死的关闭不得永久占住生命周期锁堵死后续命令；
                # wait_for 取消内层关闭后锁释放，半关闭状态由下一轮
                # ensure_browser 的健康检查/重建收敛
                await asyncio.wait_for(
                    self._close_browser_locked(), timeout=_WAIT_TIMEOUT_SECS
                )
        except asyncio.CancelledError:
            # 命令到达后取消本任务：浏览器生命周期已由命令路径接管。关闭若已
            # 开始，残留半关闭状态由命令侧 ensure_browser 的健康检查/重建收敛
            return
        except asyncio.TimeoutError:
            logger.warning("浏览器空闲自动释放超时（8s），跳过")
        except Exception as exc:  # noqa: BLE001
            logger.debug(f"浏览器空闲自动释放异常（忽略）: {exc}")
        finally:
            # 仅清理仍指向本任务的引用：期间 _arm_browser_idle_release 可能已
            # 装上新计时器，不得误清（close_browser 不再提前置 None，保证
            # 关闭段内 cancel 仍可送达）
            if self._browser_idle_task is asyncio.current_task():
                self._browser_idle_task = None

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


__all__ = [
    "BROWSER_IDLE_RELEASE_SECS",
    "BrowserLifecycleMixin",
    "_DIALOG_MESSAGE_MAX_CHARS",
    "_PERSISTENT_SESSION_STORAGE_ISOLATION_SCRIPT",
    "_TASK_STORAGE_ISOLATION_SCRIPT",
    "_WAIT_TIMEOUT_SECS",
    "_WORKER_KEEP_ALIVE",
]
