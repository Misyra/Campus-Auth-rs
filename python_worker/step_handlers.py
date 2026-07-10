"""步骤处理器。

定义 10 种基础步骤处理器（input / click / select / wait / screenshot /
evaluate / navigate / wait_for_selector / upload_file / custom），并兼容 Rust
侧 ``StepConfig`` 中出现的别名与扩展类型（click_select / wait_url / ocr）。

每个处理器签名统一为 ``async def handle(page, step, context)``：
- ``page``：Playwright 异步 Page 对象
- ``step``：已解析模板变量后的 ``StepConfig``
- ``context``：``StepContext`` 执行上下文

处理器通过抛出 :class:`WorkerError` 表达可分类的失败（对应 Outcome 枚举），
由 ``browser_runner`` 捕获并转换为 StructuredResult。
"""

from __future__ import annotations

import base64
import logging
import threading
import time
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any, Callable

from models import Outcome, StepConfig
from playwright.async_api import TimeoutError as PlaywrightTimeoutError

logger = logging.getLogger(__name__)


class WorkerError(Exception):
    """可分类的 Worker 执行错误。

    携带 ``outcome`` 字段，直接映射到 Rust 侧 ``Outcome`` 枚举，
    用于决定重试 / 回收策略。
    """

    def __init__(self, outcome: Outcome | str, message: str) -> None:
        self.outcome = outcome.value if isinstance(outcome, Outcome) else str(outcome)
        self.message = message
        super().__init__(message)


class StepCancelled(WorkerError):
    """步骤执行被取消（cancel_event 触发）。"""

    def __init__(self, message: str = "步骤执行已取消") -> None:
        super().__init__(Outcome.CANCELLED, message)


@dataclass
class StepContext:
    """单步执行的上下文。"""

    page: Any
    """Playwright 异步 Page 对象。"""

    variables: dict[str, str] = field(default_factory=dict)
    """模板变量映射。"""

    browser_settings: dict[str, Any] = field(default_factory=dict)
    """浏览器设置（供低资源 / 反检测等判断）。"""

    cancel_event: threading.Event | None = None
    """跨线程取消事件，处理器在边界处检查。"""

    screenshot_dir: Path | None = None
    """截图保存目录。"""

    default_timeout: int = 10000
    """单步默认超时（毫秒）。"""

    navigation_timeout: int = 15000
    """导航超时（毫秒）。"""

    reveal_hidden: bool = False
    """是否揭示隐藏输入框（用 JS 设置值）。"""

    step_delay: float = 0.5
    """步骤间延迟（秒）。"""

    emit: Callable[[str, dict], None] = lambda event_type, data: None
    """事件推送回调（step_progress / screenshot）。"""

    frame: str | None = None
    """iframe 选择器。非 None 时 _locator 会在指定 iframe 内定位元素。"""

    results: dict[str, Any] = field(default_factory=dict)
    """store_as 结果存储。"""

    screenshots: list[str] = field(default_factory=list)
    """本次动作产生的截图路径收集。"""


def _check_cancel(context: StepContext) -> None:
    """在步骤边界检查取消事件，若已触发则抛出 StepCancelled。"""
    if context.cancel_event is not None and context.cancel_event.is_set():
        raise StepCancelled()


def _resolve(step: StepConfig, context: StepContext) -> StepConfig:
    """解析步骤中的模板变量（selector / value / pattern / script / path）。"""
    from variable_resolver import resolve

    if context.variables:
        if step.selector:
            step.selector = resolve(step.selector, context.variables)
        if step.value:
            step.value = resolve(step.value, context.variables)
        if step.pattern:
            step.pattern = resolve(step.pattern, context.variables)
        if step.path:
            step.path = resolve(step.path, context.variables)
        script = step.effective_script
        if script:
            step.code = resolve(script, context.variables)
            step.script = step.code
    return step


def _locator(context: StepContext, selector: str):
    """根据是否位于 iframe 返回对应的 Locator。"""
    if context.page is None:
        raise WorkerError(Outcome.SELECTOR_FAILED, "页面未初始化")
    if getattr(context, "frame", None):
        frame = context.frame  # type: ignore[attr-defined]
        return context.page.frame_locator(frame).locator(selector)
    return context.page.locator(selector)


async def _safe_op(context: StepContext, coro, outcome_on_timeout: Outcome):
    """执行 Playwright 操作并归一化超时异常。"""
    try:
        return await coro
    except PlaywrightTimeoutError as exc:
        raise WorkerError(outcome_on_timeout, f"操作超时: {exc}") from exc


# ── 各类型处理器 ──


async def handle_input(page, step: StepConfig, context: StepContext) -> None:
    """在输入框填写值。"""
    _check_cancel(context)
    if not step.selector:
        raise WorkerError(Outcome.SELECTOR_FAILED, "input 步骤缺少 selector")
    value = step.value or ""
    locator = _locator(context, step.selector)
    timeout = step.timeout or context.default_timeout
    if context.reveal_hidden:
        # 隐藏输入框无法用 fill，改用 JS 直接赋值并派发 input 事件
        await page.evaluate(
            "(sel, val) => {"
            "  const el = document.querySelector(sel);"
            "  if (!el) throw new Error('selector not found');"
            "  el.value = val;"
            "  el.dispatchEvent(new Event('input', {bubbles: true}));"
            "}",
            step.selector,
            value,
        )
        return
    if step.clear:
        await _safe_op(
            context, locator.fill(value, timeout=timeout), Outcome.SELECTOR_FAILED
        )
    else:
        await _safe_op(
            context,
            locator.press_sequentially(value, timeout=timeout),
            Outcome.SELECTOR_FAILED,
        )


async def handle_click(page, step: StepConfig, context: StepContext) -> None:
    """点击元素。"""
    _check_cancel(context)
    if not step.selector:
        raise WorkerError(Outcome.SELECTOR_FAILED, "click 步骤缺少 selector")
    locator = _locator(context, step.selector)
    timeout = step.timeout or context.default_timeout
    await _safe_op(context, locator.click(timeout=timeout), Outcome.SELECTOR_FAILED)


async def handle_select(page, step: StepConfig, context: StepContext) -> None:
    """在下拉框中选择选项。"""
    _check_cancel(context)
    if not step.selector:
        raise WorkerError(Outcome.SELECTOR_FAILED, "select 步骤缺少 selector")
    if step.value is None:
        raise WorkerError(Outcome.SELECTOR_FAILED, "select 步骤缺少 value")
    locator = _locator(context, step.selector)
    timeout = step.timeout or context.default_timeout
    await _safe_op(
        context,
        locator.select_option(value=step.value, timeout=timeout),
        Outcome.SELECTOR_FAILED,
    )


async def handle_click_select(page, step: StepConfig, context: StepContext) -> None:
    """点击选项容器中的目标选项（两步式选择）。"""
    _check_cancel(context)
    if not step.selector or not step.option_selector:
        raise WorkerError(
            Outcome.SELECTOR_FAILED, "click_select 步骤缺少 selector 或 option_selector"
        )
    timeout = step.timeout or context.default_timeout
    container = _locator(context, step.selector)
    await _safe_op(context, container.click(timeout=timeout), Outcome.SELECTOR_FAILED)
    option = context.page.locator(step.option_selector)
    await _safe_op(context, option.click(timeout=timeout), Outcome.SELECTOR_FAILED)


async def handle_wait(page, step: StepConfig, context: StepContext) -> None:
    """等待固定时长（毫秒）。支持取消。"""
    duration = max(0, step.duration)
    # 分片等待以便及时响应取消
    slice_ms = 100
    waited = 0
    while waited < duration:
        _check_cancel(context)
        await asyncio_sleep(min(slice_ms, duration - waited) / 1000)
        waited += slice_ms


async def handle_wait_for_selector(page, step: StepConfig, context: StepContext) -> None:
    """等待选择器出现。"""
    _check_cancel(context)
    if not step.selector:
        raise WorkerError(Outcome.SELECTOR_FAILED, "wait_for_selector 步骤缺少 selector")
    timeout = step.timeout or context.default_timeout
    if getattr(context, "frame", None):
        target = context.page.frame_locator(context.frame).locator(step.selector)
    else:
        target = context.page.locator(step.selector)
    await _safe_op(
        context, target.first.wait_for(state="visible", timeout=timeout), Outcome.SELECTOR_FAILED
    )


async def handle_wait_url(page, step: StepConfig, context: StepContext) -> None:
    """等待当前 URL 匹配指定正则（pattern）。"""
    _check_cancel(context)
    import re

    if not step.pattern:
        raise WorkerError(Outcome.NAVIGATION_TIMEOUT, "wait_url 步骤缺少 pattern")
    timeout = step.timeout or context.navigation_timeout
    deadline = time.monotonic() + timeout / 1000
    regex = re.compile(step.pattern)
    while time.monotonic() < deadline:
        _check_cancel(context)
        current = context.page.url
        if regex.search(current):
            return
        await asyncio_sleep(0.2)
    raise WorkerError(Outcome.NAVIGATION_TIMEOUT, f"URL 未匹配: {step.pattern}")


async def handle_screenshot(page, step: StepConfig, context: StepContext) -> None:
    """对当前页面截图并保存。"""
    _check_cancel(context)
    directory = context.screenshot_dir or Path(".")
    directory.mkdir(parents=True, exist_ok=True)
    filename = step.path or f"step_{step.id}_{int(time.time() * 1000)}.png"
    if not str(filename).lower().endswith((".png", ".jpg", ".jpeg")):
        filename = f"{filename}.png"
    local_path = str(directory / Path(filename).name)
    full_page = bool(step.extra_fields.get("full_page", True))
    await page.screenshot(path=local_path, full_page=full_page)
    context.screenshots.append(local_path)
    context.emit("screenshot", {"path": local_path, "step_id": step.id})


async def handle_evaluate(page, step: StepConfig, context: StepContext) -> None:
    """执行 JavaScript 并可选地存储结果（store_as）。"""
    _check_cancel(context)
    script = step.effective_script
    if not script:
        raise WorkerError(Outcome.UNKNOWN_ERROR, "evaluate 步骤缺少 script/code")
    try:
        result = await page.evaluate(script)
    except Exception as exc:  # noqa: BLE001
        raise WorkerError(Outcome.UNKNOWN_ERROR, f"JS 执行失败: {exc}") from exc
    # 结果尽量转字符串存储
    if step.store_as:
        if isinstance(result, (dict, list)):
            import json

            context.results[step.store_as] = json.dumps(result, ensure_ascii=False)
        else:
            context.results[step.store_as] = str(result)


async def handle_navigate(page, step: StepConfig, context: StepContext) -> None:
    """导航到指定 URL。"""
    _check_cancel(context)
    url = step.value or step.selector
    if not url:
        raise WorkerError(Outcome.NAVIGATION_TIMEOUT, "navigate 步骤缺少目标 URL")
    wait_until = step.extra_fields.get("wait_until", "domcontentloaded")
    try:
        await page.goto(url, wait_until=wait_until, timeout=context.navigation_timeout)
    except Exception as exc:  # noqa: BLE001
        if isinstance(exc, PlaywrightTimeoutError):
            raise WorkerError(Outcome.NAVIGATION_TIMEOUT, f"导航超时: {url}") from exc
        raise WorkerError(Outcome.NETWORK_ERROR, f"导航失败: {exc}") from exc


async def handle_upload_file(page, step: StepConfig, context: StepContext) -> None:
    """向文件输入上传本地文件。"""
    _check_cancel(context)
    if not step.selector:
        raise WorkerError(Outcome.SELECTOR_FAILED, "upload_file 步骤缺少 selector")
    if not step.value:
        raise WorkerError(Outcome.SELECTOR_FAILED, "upload_file 步骤缺少文件路径")
    locator = _locator(context, step.selector)
    timeout = step.timeout or context.default_timeout
    await _safe_op(
        context, locator.set_input_files(step.value, timeout=timeout), Outcome.SELECTOR_FAILED
    )


async def handle_ocr(page, step: StepConfig, context: StepContext) -> None:
    """对元素截图后使用 ddddocr 识别，并填入目标输入框。"""
    _check_cancel(context)
    if not step.selector:
        raise WorkerError(Outcome.SELECTOR_FAILED, "ocr 步骤缺少 selector")
    try:
        import ddddocr  # type: ignore
    except Exception as exc:  # noqa: BLE001
        raise WorkerError(Outcome.UNKNOWN_ERROR, f"ddddocr 未安装: {exc}") from exc

    locator = _locator(context, step.selector)
    timeout = step.timeout or context.default_timeout
    await _safe_op(
        context, locator.wait_for(state="visible", timeout=timeout), Outcome.SELECTOR_FAILED
    )
    img_bytes = await locator.screenshot()
    ocr = ddddocr.DdddOcr(show_ad=False)
    if step.old:
        ocr = ddddocr.DdddOcr(old=True, show_ad=False)
    text = ocr.classification(img_bytes)
    if step.store_as:
        context.results[step.store_as] = text
    if step.target_selector:
        target = context.page.locator(step.target_selector)
        await _safe_op(
            context, target.fill(text, timeout=timeout), Outcome.SELECTOR_FAILED
        )


# 自定义脚本步骤：与 evaluate 同义（执行 JS）
handle_custom = handle_evaluate
handle_eval = handle_evaluate
handle_custom_js = handle_evaluate


def _get_handler(step_type: str):
    """根据步骤类型返回处理器，兼容别名。"""
    handlers = {
        "input": handle_input,
        "click": handle_click,
        "select": handle_select,
        "click_select": handle_click_select,
        "wait": handle_wait,
        "sleep": handle_wait,
        "wait_for_selector": handle_wait_for_selector,
        "wait_url": handle_wait_url,
        "screenshot": handle_screenshot,
        "evaluate": handle_evaluate,
        "eval": handle_eval,
        "custom_js": handle_custom_js,
        "custom": handle_custom,
        "navigate": handle_navigate,
        "upload_file": handle_upload_file,
        "ocr": handle_ocr,
    }
    return handlers.get(step_type)


async def run_step_async(page, raw_step: StepConfig, context: StepContext) -> None:
    """异步执行单个步骤。

    参数:
        page: Playwright Page。
        raw_step: 原始 StepConfig（模板变量未解析）。
        context: 执行上下文。

    抛出:
        WorkerError: 可分类失败。
        StepCancelled: 被取消。
    """
    step = _resolve(raw_step, context)
    handler = _get_handler(step.step_type)
    if handler is None:
        raise WorkerError(Outcome.UNKNOWN_ERROR, f"未知步骤类型: {step.step_type}")
    context.emit(
        "step_progress",
        {"step_id": step.id, "step_type": step.step_type, "description": step.description},
    )
    await handler(page, step, context)


def asyncio_sleep(seconds: float):
    """包装 asyncio.sleep，隔离导入。"""
    import asyncio

    return asyncio.sleep(seconds)
