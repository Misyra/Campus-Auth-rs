"""持久配置会话与自动登录命令：profile_session_* / profile_auto_login 编排（WorkerCore 拆分）。

自 playwright_worker.py 迁出（Mixin 化）：方法签名与行为逐字段保留，经
WorkerCore 多继承挂回（见 playwright_worker.py 的组装）。纯逻辑层早已拆出
（config_session.py 的状态/录制器、auto_login.py 的校验/回放映射），本模块
只承载驱动真实页面 I/O 的命令编排。

补丁语义约定：沉降窗口常量（``AUTO_LOGIN_LOAD_STATE_TIMEOUT_MS`` /
``AUTO_LOGIN_BACKFILL_SETTLE_MS``）属 auto_login（单一事实源，同时驱动 JS
模板构造），本模块经**模块属性访问**读取；``run_step_async`` /
``_sleep_cancellable`` 属 step_handlers，同样经模块属性访问调用。
"""

from __future__ import annotations

import asyncio
import logging
import time
import uuid
from typing import Any

import auto_login
from models import Outcome, StructuredResult
from step_handlers import StepCancelled, StepContext, WorkerError
import step_handlers
from auto_login import (
    auto_login_success_message,
    build_replay_steps,
    build_replay_variables,
    validate_auto_login_actions,
)
from browser_launch import _nav_timeout, _to_ms
from browser_lifecycle import _WAIT_TIMEOUT_SECS
from cancel_registry import cancel_registry
from config_session import (
    RECORDER_BINDING_NAME,
    ProfileSessionState,
    RecorderSink,
    build_recorder_init_script,
)

logger = logging.getLogger(__name__)


class ProfileCommandsMixin:
    """持久配置会话与自动登录命令（WorkerCore 的 profile 职责）。"""

    # ── 持久配置会话（profile_session_*）与持久自动登录（profile_auto_login）──
    #
    # 「记住密码」模式：门户登录页自带「记住密码」（凭据存 cookies/localStorage），
    # 持久化 user_data_dir 即可跨进程保留；下次打开门户页时门户自己的 JS 会把账号
    # 密码默认填入（纯页面行为，无头可用）。因此自动化只需录制登录点击：本节命令
    # 开一次 headed 持久会话供用户手动登录（勾选门户记住密码），录下点击/选择序列；
    # 之后的自动登录（profile_auto_login）= 无头持久上下文打开门户页（门户 JS
    # 自动回填）+ 回放录制动作 + 网络验证兜底（Rust 侧编排）。

    async def _start_profile_playwright(self) -> Any:
        """为配置会话启动独占的 Playwright 连接。

        不复用 ``self._playwright``：自动化路径的 close_browser / 空闲回收会
        ``stop()`` 共享连接，配置会话存续期（用户手动登录，可达数分钟）不能被
        无关登录/任务殃及——独立连接换来与自动化会话的完全隔离。
        """
        from playwright.async_api import async_playwright

        return await async_playwright().start()

    def _profile_session_is_alive(self, session: ProfileSessionState) -> bool:
        """判定配置会话浏览器是否仍存活（供 start 互斥检查与幽灵会话复活）。"""
        if session.dead:
            return False
        context = session.context
        if context is None:
            return False
        try:
            return not bool(context.is_closed())
        except Exception:  # noqa: BLE001 — 探活调用任何异常都视为已死亡
            return False

    def _on_profile_context_closed(self, session: ProfileSessionState) -> None:
        """持久上下文 close 事件回调：浏览器进程意外退出时收尾幽灵会话。

        只做槽位释放与资源收尾调度（回调为同步上下文，不能 await）；
        user_data_dir 里的 cookie/localStorage 原样保留，槽位释放后可再次 start。
        正常 stop 先清槽再关 context，回调命中 ``槽位非本会话`` 即为 no-op。
        """
        if self._profile_session is not session:
            return
        logger.warning("配置会话浏览器意外退出，收尾幽灵会话（user_data_dir 保留）")
        self._profile_session = None
        asyncio.ensure_future(self._teardown_profile_session(session))

    async def _teardown_profile_session(self, session: ProfileSessionState | None) -> None:
        """收尾配置会话：关持久上下文、停独占 Playwright 连接。幂等。

        **绝不清理 user_data_dir**：目录里的 cookie/localStorage 是门户「记住
        密码」的载体，收尾（正常 stop / 幽灵回收 / 退出兜底）一律原样保留。
        资源允许残缺（启动中途失败时仅已创建部分需要回收），逐项判空回收。
        """
        if session is None or session.dead:
            return
        session.dead = True
        if self._profile_session is session:
            self._profile_session = None
        if session.context is not None:
            try:
                await asyncio.wait_for(session.context.close(), timeout=_WAIT_TIMEOUT_SECS)
            except Exception as exc:  # noqa: BLE001 — 上下文可能已随进程死亡
                logger.debug("关闭配置会话持久上下文失败（忽略）: %s", exc)
        if session.own_playwright is not None:
            try:
                await asyncio.wait_for(session.own_playwright.stop(), timeout=_WAIT_TIMEOUT_SECS)
            except Exception as exc:  # noqa: BLE001 — 连接可能已断开
                logger.debug("停止配置会话 Playwright 连接失败（忽略）: %s", exc)
        session.context = None
        session.page = None
        session.own_playwright = None

    def _profile_session_for(self, session_id: str) -> ProfileSessionState:
        """解析配置会话：session_id 为空回退唯一活跃会话（单会话语义，对齐 debug）。"""
        session = self._profile_session
        if session is None:
            raise WorkerError(Outcome.UNKNOWN_ERROR, "当前没有活跃配置会话，请先启动")
        if session_id and session.session_id != session_id:
            raise WorkerError(Outcome.UNKNOWN_ERROR, "配置会话不存在，请先启动配置会话")
        return session

    async def handle_profile_session_start(self, params: dict) -> dict:
        """启动持久配置会话：headed 持久上下文 + 门户页 + 交互录制。

        params：``user_data_dir``（必需）、``portal_url``（可选，给则打开）、
        ``record``（默认 true，非布尔拒绝）、``browser_settings``（可选，
        资源/直连/stealth 口径对齐 _start_browser）、``cancel_id``。
        返回 ``{session_id, recorded}``。

        占用互斥：同一时刻仅允许一个活跃配置会话（重复 start 报错，口径对齐
        debug_start；浏览器进程意外退出的幽灵会话先收尾再放行）；与自动化
        浏览器实例相互独立，不占其单会话槽位。命令级超时只覆盖本命令本身，
        会话存续期不受约束（见 worker_main._INTERRUPTIBLE_METHODS 的豁免说明）。
        """
        user_data_dir = str(params.get("user_data_dir") or "").strip()
        if not user_data_dir:
            raise WorkerError(Outcome.UNKNOWN_ERROR, "profile_session_start 缺少 user_data_dir")
        portal_url = str(params.get("portal_url") or "").strip()
        if portal_url and not portal_url.startswith(("http://", "https://")):
            raise WorkerError(Outcome.UNKNOWN_ERROR, "portal_url 仅支持 http/https 地址")
        record = params.get("record", True)
        if not isinstance(record, bool):
            raise WorkerError(Outcome.UNKNOWN_ERROR, "record 必须是布尔值")
        bs = params.get("browser_settings", {}) or {}
        cancel_id = str(params.get("cancel_id") or "")
        cancel_event = cancel_registry.register(cancel_id) if cancel_id else None

        def _ensure_not_cancelled(stage: str) -> None:
            if cancel_event is not None and cancel_event.is_set():
                raise StepCancelled(f"配置会话已取消（{stage}）")

        # 互斥：活跃会话存续时拒绝重复启动；浏览器进程已退出的幽灵会话
        # 收尾（清槽、停连接、user_data_dir 原样保留）后放行
        existing = self._profile_session
        if existing is not None:
            if self._profile_session_is_alive(existing):
                raise WorkerError(
                    Outcome.UNKNOWN_ERROR, "已存在活跃配置会话，请先停止当前配置会话"
                )
            logger.warning("发现浏览器已退出的配置会话，收尾后重新启动")
            await self._teardown_profile_session(existing)

        session_id = uuid.uuid4().hex
        channel = str(bs.get("browser_channel") or "playwright").strip().lower()
        custom_path = str(bs.get("browser_custom_path") or "").strip()
        pure_mode = bool(bs.get("pure_mode", False))
        # 对齐 _start_browser：未绑定代理即绕过系统代理（直连保障），纯净模式
        # 只关闭可选定制、不关闭直连保障
        launch_args = [] if pure_mode else self._build_launch_args(bs, channel)
        if self._bypass_system_proxy(bs) and self._is_chromium_channel(bs, channel):
            if "--no-proxy-server" not in launch_args:
                launch_args.append("--no-proxy-server")
        ctx_opts = self._build_context_options(bs)

        # 会话对象先建、资源逐项挂载：任一环节失败，except 统一回收已创建部分
        session = ProfileSessionState(
            session_id=session_id,
            user_data_dir=user_data_dir,
            portal_url=portal_url,
            record=record,
            sink=RecorderSink(),
        )
        try:
            _ensure_not_cancelled("浏览器启动前")
            session.own_playwright = await self._start_profile_playwright()
            # headed 持久上下文：headless 恒为 False（用户要在这个窗口里手动登录）
            session.context = await self._launch_persistent_context(
                playwright=session.own_playwright,
                channel=channel,
                custom_path=custom_path,
                headless=False,
                launch_args=launch_args,
                user_data_dir=user_data_dir,
                ctx_opts=ctx_opts,
                bs=bs,
            )
            # 资源/直连/stealth 口径对齐 _start_browser：低资源路由与反检测脚本
            # 作用于配置会话自己的上下文（pure_mode 语义同正式登录：全关闭）
            if not pure_mode:
                await self._apply_stealth_and_routes(bs, context=session.context)
            if record:
                # 录制器：binding 挂上下文级（覆盖弹窗新页）且跨导航存活；
                # init 脚本随每个新文档重装监听器（页面 JS 状态不跨导航）
                await session.context.expose_binding(
                    RECORDER_BINDING_NAME, session.sink.add_json
                )
                await session.context.add_init_script(build_recorder_init_script())
            # 持久上下文可能自带预开页：统一关掉，维持单活跃页语义
            for old_page in list(session.context.pages):
                await self._safe_close(old_page, "config session pre-open page")
            session.page = await session.context.new_page()
            # 门户页弹窗就地自动确认：不走共享会话的 dialog 事件通道，
            # 避免污染 _task_dialogs 与前端事件流（口径同临时捕获浏览器）
            async def _auto_accept(dialog: Any) -> None:
                try:
                    await dialog.accept()
                except Exception as exc:  # noqa: BLE001 — 弹窗可能已在间隙消失
                    logger.debug(f"配置会话页弹窗确认失败（忽略）: {exc}")

            page_on = getattr(session.page, "on", None)
            if callable(page_on):
                page_on("dialog", lambda d: asyncio.ensure_future(_auto_accept(d)))
            # 浏览器进程意外退出 → 幽灵会话收尾（槽位释放，user_data_dir 保留）
            context_on = getattr(session.context, "on", None)
            if callable(context_on):
                context_on("close", lambda _c: self._on_profile_context_closed(session))

            _ensure_not_cancelled("会话建立前")
            self._profile_session = session
            if portal_url:
                # 门户页导航放在会话建成后：失败走 except 统一回收整场，
                # 不留"浏览器开着但 Rust 已收到失败"的半开会话
                await self._navigate(session.page, portal_url, _nav_timeout(bs))
        except BaseException:
            # 启动失败/取消：回收已创建资源，绝不留半开会话（含幽灵收尾语义）
            await self._teardown_profile_session(session)
            raise
        else:
            # 幽灵取消窗口（对齐 debug_start）：取消恰落在会话建成与响应返回
            # 之间时，不能留下 Rust 已判定取消、Worker 却仍视为活跃的会话
            if cancel_event is not None and cancel_event.is_set():
                await self._teardown_profile_session(session)
                raise StepCancelled("配置会话已取消（启动完成阶段）")
            logger.info(
                "配置会话已启动: id=%s, record=%s, portal=%s, user_data_dir 保留挂载",
                session_id,
                record,
                bool(portal_url),
            )
            return {"session_id": session_id, "recorded": record}
        finally:
            # cancel_id 只覆盖启动命令本身；会话存续期不注册任何 cancel 令牌
            if cancel_id:
                cancel_registry.unregister(cancel_id)

    async def handle_profile_session_stop(self, params: dict) -> dict:
        """停止持久配置会话：返回录制动作并关闭持久浏览器。

        user_data_dir 里的 cookie/localStorage（门户「记住密码」载体）原样
        保留——只关浏览器，绝不清理目录。无活跃会话时报错（显式契约，对齐
        debug_stop，不做静默幂等）；session_id 不匹配同样报错。
        返回 ``{recorded_actions, truncated}``（record=false 或无录制时空数组）。
        """
        session_id = str(params.get("session_id") or "")
        session = self._profile_session_for(session_id)
        record = session.record
        actions = session.sink.recorded_actions() if record else []
        truncated = session.sink.truncated if record else False
        await self._teardown_profile_session(session)
        logger.info(
            "配置会话已停止: id=%s, recorded=%d, truncated=%s",
            session.session_id,
            len(actions),
            truncated,
        )
        return {"recorded_actions": actions, "truncated": truncated}

    async def _settle_auto_login_page(self, page: Any, context: Any) -> None:
        """门户页沉降：load 态等待（有界 best-effort）+ 固定沉降窗口。

        「记住密码」的账号密码回填是门户自己的 JS 干的（通常挂在 load /
        onload 后的异步请求上），``goto``（domcontentloaded）返回不代表回填
        已发生。这里等 ``load``（有界，慢资源门户到不了 load 就放弃等待）
        再加固定沉降窗口（常量见 ``auto_login`` 模块，窗口可测）；沉降等待
        经 ``_sleep_cancellable`` 保持取消可响应。沉降**不作为任何成败判定**
        ——登录是否真的成功由 Rust 侧登录后网络验证兜底。
        """
        try:
            await asyncio.wait_for(
                page.wait_for_load_state("load"),
                timeout=auto_login.AUTO_LOGIN_LOAD_STATE_TIMEOUT_MS / 1000,
            )
        except Exception as exc:  # noqa: BLE001 — load 缺席（慢资源/已导航走）不阻断流程
            logger.debug("门户页 load 态等待未达成（忽略，继续沉降）: %s", exc)
        await step_handlers._sleep_cancellable(
            auto_login.AUTO_LOGIN_BACKFILL_SETTLE_MS / 1000, context
        )

    async def handle_profile_auto_login(self, params: dict) -> dict:
        """持久自动登录：无头持久上下文打开门户页 + 回放录制动作 + 结构化结果。

        params：``user_data_dir``（必需）、``portal_url``（必需，仅 http/https）、
        ``recorded_actions``（数组，可为空 = 只打开门户页等回填不点击）、
        ``browser_settings``（可选，直连/资源路由/stealth 口径对齐 _start_browser）、
        ``cancel_id``。

        行为：**headless** ``launch_persistent_context``（与配置会话的 headed
        区分）→ goto portal_url → 回填沉降 → 逐条回放（click → click 处理器、
        change → select 处理器，经 ``run_step_async`` 沿用既有超时/取消契约）
        → 回放后沉降（给提交导航留落地窗口）→ 关浏览器返回结果。

        成败口径：本命令的成功 = 回放全部动作无错误且页面完成导航/加载沉降；
        **登录是否真的成功以 Rust 侧登录后网络验证兜底**（非本命令职责）。
        回放步骤失败 → ``assertion_failed``；门户导航失败按既有导航分类
        （``navigation_timeout`` / ``network_error``）；取消 → ``cancelled``；
        参数/占用错误 → ``unknown_error`` 结构化错误（浏览器启动前拦截）。

        生命周期：与配置会话**共用同一占用注册表**（``self._profile_session``
        槽位，同时仅一个持久类操作）；持久目录**只写不清**（绝不清理
        cookies/localStorage）；幽灵收尾对齐配置会话范式（close 事件回收 +
        失败/取消整场回收 + Worker 退出兜底）；命令级超时覆盖本命令全程，
        不进 ``_INTERRUPTIBLE_METHODS``（独立上下文，关 self._page 既打不着
        它也会误伤 keep_alive，豁免口径同 profile_session_*）。
        """
        user_data_dir = str(params.get("user_data_dir") or "").strip()
        if not user_data_dir:
            raise WorkerError(Outcome.UNKNOWN_ERROR, "profile_auto_login 缺少 user_data_dir")
        portal_url = str(params.get("portal_url") or "").strip()
        if not portal_url:
            raise WorkerError(Outcome.UNKNOWN_ERROR, "profile_auto_login 缺少 portal_url")
        if not portal_url.startswith(("http://", "https://")):
            raise WorkerError(Outcome.UNKNOWN_ERROR, "portal_url 仅支持 http/https 地址")
        raw_actions = params.get("recorded_actions")
        if raw_actions is None:
            raw_actions = []
        if not isinstance(raw_actions, list):
            raise WorkerError(Outcome.UNKNOWN_ERROR, "recorded_actions 必须是数组")
        # 校验前置（浏览器启动之前）：畸形/不认识的 type 直接结构化报错，
        # 不留半开浏览器
        try:
            actions = validate_auto_login_actions(raw_actions)
        except ValueError as exc:
            raise WorkerError(Outcome.UNKNOWN_ERROR, f"recorded_actions 非法: {exc}") from exc
        # 占位符解析 + 首条 navigate 跳过都依赖 portal_url 与凭据映射：
        # 凭据键与浏览器任务执行同名（username/password/isp/auth_url，Rust 侧
        # build_persistent_login_params 下发），缺失时映射不注入、回放退化为
        # 旧行为（只回放 click/change）
        replay_steps = build_replay_steps(
            actions,
            portal_url=portal_url,
            variables=build_replay_variables(params),
        )
        bs = params.get("browser_settings", {}) or {}
        cancel_id = str(params.get("cancel_id") or "")
        cancel_event = cancel_registry.register(cancel_id) if cancel_id else None

        def _ensure_not_cancelled(stage: str) -> None:
            if cancel_event is not None and cancel_event.is_set():
                raise StepCancelled(f"持久自动登录已取消（{stage}）")

        try:
            # 占用互斥：与配置会话共用同一槽位（同时仅一个持久类操作）；
            # 浏览器进程已退出的幽灵会话先收尾再放行（口径同 profile_session_start）
            existing = self._profile_session
            if existing is not None:
                if self._profile_session_is_alive(existing):
                    raise WorkerError(
                        Outcome.UNKNOWN_ERROR,
                        "已存在活跃配置会话，请先停止当前配置会话",
                    )
                logger.warning("发现浏览器已退出的配置会话，收尾后执行自动登录")
                await self._teardown_profile_session(existing)

            channel = str(bs.get("browser_channel") or "playwright").strip().lower()
            custom_path = str(bs.get("browser_custom_path") or "").strip()
            pure_mode = bool(bs.get("pure_mode", False))
            # 对齐 _start_browser / profile_session_start：未绑定代理即绕过系统
            # 代理（直连保障），纯净模式只关闭可选定制、不关闭直连保障
            launch_args = [] if pure_mode else self._build_launch_args(bs, channel)
            if self._bypass_system_proxy(bs) and self._is_chromium_channel(bs, channel):
                if "--no-proxy-server" not in launch_args:
                    launch_args.append("--no-proxy-server")
            ctx_opts = self._build_context_options(bs)

            start = time.perf_counter()
            # 会话对象先建、资源逐项挂载：任一环节失败，except 统一回收已创建
            # 部分（record=False：自动登录只回放，不注入录制器）
            session = ProfileSessionState(
                session_id=uuid.uuid4().hex,
                user_data_dir=user_data_dir,
                portal_url=portal_url,
                record=False,
            )
            try:
                _ensure_not_cancelled("浏览器启动前")
                session.own_playwright = await self._start_profile_playwright()
                # headless 持久上下文：与配置会话（headed 手动登录）区分，
                # user_data_dir 原样挂载（cookies/localStorage 只写不清）
                session.context = await self._launch_persistent_context(
                    playwright=session.own_playwright,
                    channel=channel,
                    custom_path=custom_path,
                    headless=True,
                    launch_args=launch_args,
                    user_data_dir=user_data_dir,
                    ctx_opts=ctx_opts,
                    bs=bs,
                )
                # 资源/直连/stealth 口径对齐 _start_browser：低资源路由与反检测
                # 脚本作用于本命令自己的上下文（pure_mode 语义同正式登录：全关闭）
                if not pure_mode:
                    await self._apply_stealth_and_routes(bs, context=session.context)
                # 持久上下文可能自带预开页：统一关掉，维持单活跃页语义
                for old_page in list(session.context.pages):
                    await self._safe_close(old_page, "auto login pre-open page")
                session.page = await session.context.new_page()

                # 门户页弹窗就地自动确认（口径同配置会话页，不污染共享事件通道）
                async def _auto_accept(dialog: Any) -> None:
                    try:
                        await dialog.accept()
                    except Exception as exc:  # noqa: BLE001 — 弹窗可能已在间隙消失
                        logger.debug(f"自动登录页弹窗确认失败（忽略）: {exc}")

                page_on = getattr(session.page, "on", None)
                if callable(page_on):
                    page_on("dialog", lambda d: asyncio.ensure_future(_auto_accept(d)))
                # 浏览器进程意外退出 → 幽灵收尾（槽位释放，user_data_dir 保留）
                context_on = getattr(session.context, "on", None)
                if callable(context_on):
                    context_on("close", lambda _c: self._on_profile_context_closed(session))

                # 占用注册表在导航前持锁：导航/回放全程拒绝其他持久类操作
                self._profile_session = session
                _ensure_not_cancelled("门户导航前")
                await self._navigate(session.page, portal_url, _nav_timeout(bs))
                # 回填沉降：等门户 JS 把账号密码填好（窗口可测，best-effort）
                step_context = StepContext(
                    page=session.page,
                    cancel_event=cancel_event,
                    default_timeout=_to_ms(bs, "timeout", 10000),
                    navigation_timeout=_nav_timeout(bs),
                )
                await self._settle_auto_login_page(session.page, step_context)
                # 逐条回放（空数组 = 只打开门户页等回填不点击）：既有步骤处理器
                # 沿用既有超时/取消契约；失败即中止（后续动作依赖前序语义）
                for index, step in enumerate(replay_steps):
                    _ensure_not_cancelled(f"回放动作 {index + 1} 前")
                    try:
                        await step_handlers.run_step_async(
                            session.page,
                            step,
                            step_context,
                            step_index=index,
                            total_steps=len(replay_steps),
                        )
                    except WorkerError as exc:
                        if isinstance(exc, StepCancelled):
                            raise
                        # 回放步骤失败统一归 assertion_failed（Rust 侧可重试、
                        # 不回收 Worker）：保留原始消息与动作定位
                        raise WorkerError(
                            Outcome.ASSERTION_FAILED,
                            f"回放第 {index + 1}/{len(replay_steps)} 个动作失败"
                            f"（{actions[index]['type']} {actions[index]['selector']}）"
                            f": {exc.message}",
                        ) from exc
                if replay_steps:
                    # 回放后沉降：登录点击通常触发提交导航，给导航/加载留落地
                    # 窗口（best-effort，超时不判失败——成败由 Rust 网络验证兜底）
                    await self._settle_auto_login_page(session.page, step_context)
            except BaseException:
                # 启动失败 / 导航失败 / 回放失败 / 取消：回收已创建资源，
                # 绝不留半开浏览器（user_data_dir 原样保留，含幽灵收尾语义）
                await self._teardown_profile_session(session)
                raise
            # 幽灵取消窗口（对齐配置会话）：取消恰落在完成与响应返回之间时，
            # 不能留下 Rust 已判定取消、Worker 却报成功的自动登录
            if cancel_event is not None and cancel_event.is_set():
                await self._teardown_profile_session(session)
                raise StepCancelled("持久自动登录已取消（完成阶段）")
            await self._teardown_profile_session(session)
            duration_ms = int((time.perf_counter() - start) * 1000)
            logger.info(
                "持久自动登录完成: actions=%d, 耗时=%dms, user_data_dir 保留挂载",
                len(replay_steps),
                duration_ms,
            )
            return StructuredResult(
                outcome=Outcome.SUCCESS.value,
                message=auto_login_success_message(len(replay_steps)),
                duration_ms=duration_ms,
            ).to_dict()
        finally:
            # cancel_id 只覆盖本命令本身（全程）
            if cancel_id:
                cancel_registry.unregister(cancel_id)

    async def close_profile_session(self) -> None:
        """收尾持久配置会话（Worker 退出路径专用入口；幂等）。

        与 ``close_browser``（自动化浏览器回收）刻意分离：自动化路径的
        重建回滚 / 空闲回收 / 登录终态回收都不得殃及用户正在手动操作的
        配置会话窗口。user_data_dir 原样保留，绝不清理。
        """
        await self._teardown_profile_session(self._profile_session)


__all__ = ["ProfileCommandsMixin"]
