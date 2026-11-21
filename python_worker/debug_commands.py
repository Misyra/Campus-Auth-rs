"""调试会话命令：debug_* 编排与截图治理（WorkerCore 拆分）。

自 playwright_worker.py 迁出（Mixin 化）：方法签名与行为逐字段保留，经
WorkerCore 多继承挂回（见 playwright_worker.py 的组装）。职责边界：

- debug_start / debug_step / debug_run_all / debug_status / debug_stop 命令编排
  （debug_start 另含配置会话存续守卫——补齐 Rust 会话矩阵单向拦截的
  Python 半边缺口）；
- ``_ensure_no_debug_session``：B3 纵深防御（Python 半）的守卫入口，任务/捕获
  类命令经 ``self`` 复用；
- 截图治理：``_capture_debug_frame``（步骤补拍）、``_defer_task_screenshot_cleanup``
  （任务截图延迟删除，为 WebSocket 异步回读预留窗口）、``_cleanup_debug_screenshots``
  （会话级清理）。

补丁语义约定：``TASK_SCREENSHOT_RETENTION_SECS`` 属本模块（属主）；
``run_step_async`` / ``_sleep_cancellable`` 属 step_handlers、
``_debug_screenshot_dir`` 属 worker_paths，均经**模块属性访问**调用。
"""

from __future__ import annotations

import asyncio
import logging
import time
import uuid
from contextlib import asynccontextmanager
from pathlib import Path
from typing import Any, AsyncIterator

from models import Outcome, StepConfig, TaskConfig
from step_handlers import StepCancelled, StepContext, WorkerError
import step_handlers
import worker_paths
from browser_launch import _nav_timeout
from cancel_registry import cancel_registry
from debug_session import DebugSession, _build_steps_info
from step_engine import _normalize_step_failure, _profile_login_url, _resolve_start_url

logger = logging.getLogger(__name__)

# 普通任务截图为 WebSocket 异步回读预留时间；启动时仍会清理异常退出残留。
TASK_SCREENSHOT_RETENTION_SECS = 30

# 调试步骤补拍截图的超时（毫秒）。截图只是给调试面板看的预览，超时即放弃：
# 卡住的页面不得让 debug_step / debug_run_all 的命令响应无限期等待。
DEBUG_FRAME_TIMEOUT_MS = 5000


class DebugCommandsMixin:
    """调试会话命令编排（WorkerCore 的 debug 职责）。"""

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
        # 配置会话存续守卫（对齐 _ensure_no_debug_session 模式）：配置会话
        # 持有独占浏览器与录制状态，调试启动会重建共享浏览器并改写会话类型
        # 标记，双开互扰；反向（调试会话存续期 profile_session_start）已被
        # Rust 会话矩阵拦截，此处补齐 Python 半边的单向缺口
        if self._profile_session is not None:
            raise WorkerError(
                Outcome.UNKNOWN_ERROR, "配置会话进行中，无法启动调试会话，请先停止配置会话"
            )
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
                self._page, variables, bs, cancel_event, worker_paths._debug_screenshot_dir(), task
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

    def _debug_session_for(self, session_id: str) -> DebugSession:
        """解析调试会话：显式 session_id 优先；为空时回退到唯一活跃会话。

        Rust 侧从不显式传 session_id（单会话语义），空串时若恰有一个活跃
        会话则回退到它。「存在多个会话要求显式指定」分支按当前 Rust 契约
        （调试会话互斥，见 bridge/session.rs）不可达，**刻意保留为防御纵深**
        而非待清理的历史遗留：Worker 不假设调用方永远守约——若未来 Rust 放开
        多调试会话、或异常路径在字典里残留幽灵会话，显式报错优于静默命中
        错误会话（调试步骤会写进错误的会话上下文，取证时难以察觉）。
        删除该分支前必须先证明单会话不变式永不变更。
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
    def _debug_response(session: DebugSession) -> dict:
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
        session: DebugSession,
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
            shot_dir = worker_paths._debug_screenshot_dir()
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
        session: DebugSession, idx: int, success: bool, message: str
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
        self, params: dict, session: DebugSession
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
                await step_handlers.run_step_async(
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
                        await step_handlers._sleep_cancellable(session.context.step_delay, session.context)
                    await step_handlers.run_step_async(
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
    def _cleanup_debug_screenshots(session: DebugSession) -> None:
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

    def _teardown_debug_session(self, session: DebugSession) -> None:
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


__all__ = [
    "DEBUG_FRAME_TIMEOUT_MS",
    "DebugCommandsMixin",
    "TASK_SCREENSHOT_RETENTION_SECS",
]
