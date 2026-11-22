"""任务执行：_run_task、cookie 清理矩阵与 execute_* 命令（WorkerCore 拆分）。

自 playwright_worker.py 迁出（Mixin 化）：方法签名与行为逐字段保留，经
WorkerCore 多继承挂回（见 playwright_worker.py 的组装）。职责边界：

- ``_run_task``：确保浏览器 → 会话页 → 首导航 → run_steps → success_condition
  真值判定 → 按最终 outcome 清 cookies（cookie 清理矩阵：失败/异常清、
  SUCCESS/CANCELLED 不清）→ 截图延迟清理；
- ``_cancel_session``：一次性命令（execute_login_attempt / execute_browser_task）
  的公共 setup（注册 cancel_id、解析 TaskConfig，退出时清理）；
- OCR 预热从 ocr_runtime 导入（P2-1 影子模块修复：不得从 worker_main 入口
  模块导入——脚本形态启动会重执行其模块体产生第二份能力字典）。

补丁语义约定：``run_steps`` 属 step_engine、``_sleep_cancellable`` 属
step_handlers、``_debug_screenshot_dir`` 属 worker_paths，本模块经**模块属性
访问**调用——测试对替身的 monkeypatch 统一打在属主模块上。
"""

from __future__ import annotations

import logging
import sys
import threading
import time
from contextlib import asynccontextmanager
from pathlib import Path
from typing import Any, AsyncIterator

from models import Outcome, StructuredResult, TaskConfig
from ocr_runtime import _preload_ocr_deps
from step_handlers import (
    StepCancelled,
    StepContext,
    WorkerError,
    _classify_navigation_error,
)
import step_handlers
import step_engine
import worker_paths
from browser_launch import _nav_timeout, _to_ms
from cancel_registry import cancel_registry
from step_engine import (
    _build_result,
    _is_truthy,
    _profile_login_url,
    _resolve_start_url,
)
from variable_resolver import resolve

logger = logging.getLogger(__name__)


class TaskRunnerMixin:
    """浏览器任务执行（WorkerCore 的任务执行职责）。"""

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
            await step_handlers._sleep_cancellable(task_config.navigation_wait, context)

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
            # 入缓存，后续后台线程的分类识别只命中缓存，避免 Windows loader lock 卡
            # 100s。_preload_ocr_deps 从 ocr_runtime 顶层导入（P2-1 影子模块修复）：
            # 不得从 worker_main 入口模块导入——脚本形态启动时会重执行 worker_main
            # 模块体，force 翻转写进第二份能力字典，health check 读不到。
            if any(s.step_type == "ocr" for s in (task_config.steps or [])):
                try:
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

            result = await step_engine.run_steps(self._page, task_config.steps, context)
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
                    task, bs, variables, cancel_event, worker_paths._debug_screenshot_dir(),
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
                    task, bs, variables, cancel_event, worker_paths._debug_screenshot_dir(),
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
        # 值为 None（Rust 侧 serde null）时跳过注入：str(None) 会把字面 "None"
        # 当成凭据/地址注入任务模板（P2-7）；空串仍原样注入，保持既有语义
        out = {
            sys_key: str(params[src_key])
            for sys_key, src_key in mapping.items()
            if params.get(src_key) is not None
        }
        trigger = params.get("trigger_url", "") or ""
        if isinstance(trigger, str) and trigger.strip():
            out["LOGIN_URL"] = trigger
        return out


__all__ = ["TaskRunnerMixin"]
