"""步骤执行引擎：run_steps、失败归一与首导航解析（WorkerCore 拆分）。

自 playwright_worker.py 迁出（原 browser_runner 注释块随之迁移）。纯步骤编排，
不直接持有浏览器实例——page 与 StepContext 由调用方（task_runner /
debug_commands / profile_commands）注入。

补丁语义约定：``run_step_async`` / ``_sleep_cancellable`` 属 step_handlers（单步
执行与可取消延迟的属主），本模块经**模块属性访问**调用它们——测试对这两处
取消/超时契约替身的 monkeypatch 统一打在 step_handlers 上，对 run_steps 与
各命令编排（debug/profile/task）同时生效。其余跨模块引用（未被测试替换的
叶子函数）走常规 from-import。
"""

from __future__ import annotations

import logging
import time
from typing import Any

from models import Outcome, StepConfig, StructuredResult
from step_handlers import (
    StepCancelled,
    StepContext,
    WorkerError,
    _check_cancel,
)
import step_handlers
from variable_resolver import resolve

logger = logging.getLogger(__name__)


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
                await step_handlers._sleep_cancellable(context.step_delay, context)
            try:
                await step_handlers.run_step_async(page, step, context, step_index=idx, total_steps=total)
            except WorkerError as exc:
                if isinstance(exc, StepCancelled):
                    raise
                if step.required or exc.outcome == Outcome.MANUAL_REQUIRED.value:
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


__all__ = [
    "_build_result",
    "_is_truthy",
    "_normalize_step_failure",
    "_profile_login_url",
    "_resolve_start_url",
    "run_steps",
]
