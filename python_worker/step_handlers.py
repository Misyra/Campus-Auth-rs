"""步骤处理器。

定义浏览器任务的步骤执行语义，并兼容 Rust ``StepConfig`` 的历史别名。
本模块刻意把选择器、模板变量、frame 与超时语义收敛在公共辅助函数中，
避免不同步骤各自实现一套略有差异的行为。

每个处理器签名统一为 ``async def handle(page, step, context)``：
- ``page``：Playwright 异步 Page 对象
- ``step``：已解析模板变量后的 ``StepConfig``
- ``context``：``StepContext`` 执行上下文

处理器通过抛出 :class:`WorkerError` 表达可分类的失败（对应 Outcome 枚举），
由 ``playwright_worker`` 捕获并转换为 StructuredResult。
"""

from __future__ import annotations

import asyncio
import json
import logging
import re
import threading
import time
from dataclasses import dataclass, field, replace
from pathlib import Path
from typing import Any, Callable

from models import Outcome, StepConfig
from playwright.async_api import Error as PlaywrightError
from playwright.async_api import TimeoutError as PlaywrightTimeoutError

logger = logging.getLogger(__name__)


class WorkerError(Exception):
    """可分类的 Worker 执行错误。"""

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
    """浏览器设置。"""

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
    """frame name、``url=`` URL 片段或 iframe/frame CSS 选择器。"""

    results: dict[str, Any] = field(default_factory=dict)
    """store_as 运行时结果。运行时结果在模板解析中优先于静态变量。"""

    screenshots: list[str] = field(default_factory=list)
    """本次动作产生的截图路径收集。"""


def _check_cancel(context: StepContext) -> None:
    """在步骤边界检查取消事件，若已触发则抛出 StepCancelled。"""
    if context.cancel_event is not None and context.cancel_event.is_set():
        raise StepCancelled()


def _template_value(value: Any) -> str:
    """把运行时结果转换成稳定的模板字符串。"""
    if value is None:
        return ""
    if isinstance(value, bool):
        return "true" if value else "false"
    if isinstance(value, (dict, list)):
        return json.dumps(value, ensure_ascii=False, separators=(",", ":"))
    return str(value)


def _template_variables(context: StepContext) -> dict[str, str]:
    """合并静态变量与 store_as 运行时结果，运行时结果优先。"""
    variables = dict(context.variables)
    variables.update({key: _template_value(value) for key, value in context.results.items()})
    return variables


def _resolve_extra(value: Any, variables: dict[str, str]) -> Any:
    """递归解析 extras 中的字符串模板（例如 goto.url / wait_until）。"""
    from variable_resolver import resolve

    if isinstance(value, str):
        return resolve(value, variables)
    if isinstance(value, list):
        return [_resolve_extra(item, variables) for item in value]
    if isinstance(value, dict):
        return {key: _resolve_extra(item, variables) for key, item in value.items()}
    return value


def _resolve(step: StepConfig, context: StepContext) -> StepConfig:
    """解析步骤中所有可模板化字段。

    运行时 ``store_as`` 结果优先于任务/系统静态变量，因此 OCR/Eval 的输出可以在
    后续步骤通过 ``{{变量}}`` 直接引用。返回副本而非原地改写，保证调试会话重跑
    同一步骤时不会发生二次解析。
    """
    from variable_resolver import resolve

    variables = _template_variables(context)
    if not variables:
        return step

    resolved = replace(step, extras=dict(step.extras))
    for attr in (
        "description",
        "selector",
        "value",
        "pattern",
        "path",
        "option_selector",
        "target_selector",
        "frame",
        "store_as",
    ):
        value = getattr(resolved, attr)
        if isinstance(value, str) and value:
            setattr(resolved, attr, resolve(value, variables))

    if isinstance(resolved.char_range, str) and resolved.char_range:
        resolved.char_range = resolve(resolved.char_range, variables)

    script = resolved.effective_script
    if script:
        code = resolve(script, variables)
        resolved.code = code
        resolved.script = code

    resolved.extras = _resolve_extra(resolved.extras, variables)
    return resolved


# OCR 实例缓存/图片预处理已迁至 ocr_runtime.py，此处再导出兼容旧调用方
from ocr_runtime import OCR_TIMEOUT_SECS, _get_ocr, _preprocess_ocr_image  # noqa: E402,F401


# 连接级错误代码：含这些消息的异常归为 NETWORK_ERROR
_CONNECTION_ERROR_PATTERNS = (
    "ERR_CONNECTION_TIMED_OUT",
    "ERR_NAME_NOT_RESOLVED",
    "ERR_CONNECTION_REFUSED",
    "ERR_INTERNET_DISCONNECTED",
    "ERR_CONNECTION_RESET",
    "ERR_NETWORK_CHANGED",
    "ERR_ADDRESS_UNREACHABLE",
    "ERR_CONNECTION_CLOSED",
    "ERR_NAME_RESOLUTION_FAILED",
    "ERR_PROXY_CONNECTION_FAILED",
)

# click/input 降级到 attached 元素操作的最小等待时长（毫秒）
_MIN_ATTACHED_MS = 500

# click_select 展开面板后的默认动画/渲染缓冲（毫秒）
_DEFAULT_SELECT_DELAY_MS = 500

# 强制输入 JS：绕过可见性检查，用原生 setter 写值并派发完整用户事件。
_FORCE_INPUT_JS = """(el, params) => {
  const val = params.val;
  const doClear = params.doClear;
  if (el.isContentEditable) {
    if (doClear) el.textContent = '';
    const finalVal = doClear ? val : (el.textContent || '') + val;
    el.focus();
    el.dispatchEvent(new InputEvent('beforeinput', {bubbles:true, inputType:'insertText', data: finalVal}));
    el.textContent = finalVal;
    el.dispatchEvent(new InputEvent('input', {bubbles:true, inputType:'insertText', data: finalVal}));
    el.dispatchEvent(new Event('change', {bubbles:true}));
    el.blur();
    return;
  }
  const isTextarea = el.tagName === 'TEXTAREA';
  const proto = isTextarea ? HTMLTextAreaElement.prototype : HTMLInputElement.prototype;
  const desc = Object.getOwnPropertyDescriptor(proto, 'value');
  const nativeSet = desc && desc.set;
  if (!nativeSet) { el.value = doClear ? val : el.value + val; }
  else {
    el.dispatchEvent(new FocusEvent('focus', {bubbles:true}));
    if (doClear) nativeSet.call(el, '');
    const finalVal = doClear ? val : el.value + val;
    el.dispatchEvent(new InputEvent('beforeinput', {bubbles:true, inputType:'insertText', data: finalVal}));
    nativeSet.call(el, finalVal);
    el.dispatchEvent(new InputEvent('input', {bubbles:true, inputType:'insertText', data: finalVal}));
    el.dispatchEvent(new KeyboardEvent('keyup', {bubbles:true}));
    el.dispatchEvent(new Event('change', {bubbles:true}));
    el.dispatchEvent(new FocusEvent('blur', {bubbles:true}));
  }
}"""


def _classify_navigation_error(exc: Exception, url: str) -> WorkerError:
    """按异常消息细分导航错误。"""
    msg = str(exc)
    if any(pattern in msg for pattern in _CONNECTION_ERROR_PATTERNS):
        return WorkerError(Outcome.NETWORK_ERROR, f"导航失败（网络连接错误）: {url}: {msg}")
    if isinstance(exc, PlaywrightTimeoutError):
        return WorkerError(Outcome.NAVIGATION_TIMEOUT, f"导航超时: {url}")
    return WorkerError(Outcome.NETWORK_ERROR, f"导航失败: {msg}")


def _normalize_selector(selector: str) -> str:
    """把录制器/旧任务常见的 XPath 形式规范化为 Playwright selector。"""
    value = selector.strip()
    if value.startswith("/") and not value.startswith("//?"):
        return f"xpath={value}"
    return value


def _split_selector_candidates(selector: str) -> list[str]:
    """仅在 CSS 顶层逗号处分割候选选择器。

    ``:is(.a,.b)``、``[data-x='a,b']`` 等合法 CSS 中的逗号不能被当成候选分隔符。
    """
    result: list[str] = []
    buf: list[str] = []
    quote: str | None = None
    escaped = False
    paren_depth = 0
    bracket_depth = 0

    for char in selector:
        if escaped:
            buf.append(char)
            escaped = False
            continue
        if char == "\\":
            buf.append(char)
            escaped = True
            continue
        if quote is not None:
            buf.append(char)
            if char == quote:
                quote = None
            continue
        if char in ("'", '"'):
            quote = char
            buf.append(char)
            continue
        if char == "(":
            paren_depth += 1
        elif char == ")" and paren_depth > 0:
            paren_depth -= 1
        elif char == "[":
            bracket_depth += 1
        elif char == "]" and bracket_depth > 0:
            bracket_depth -= 1
        elif char == "," and paren_depth == 0 and bracket_depth == 0:
            item = "".join(buf).strip()
            if item:
                result.append(item)
            buf = []
            continue
        buf.append(char)

    item = "".join(buf).strip()
    if item:
        result.append(item)
    return result or [selector.strip()]


def _looks_like_plain_text(selector: str) -> bool:
    """判断录制器候选是否像纯文本，而不是 CSS/XPath。"""
    value = selector.strip()
    if not value or len(value) > 80:
        return False
    if value.startswith(("text=", "xpath=", "/")):
        return False
    return not any(ch in value for ch in "#.[>+~:=*|^$(),")


def _frame_scope(context: StepContext):
    """返回当前步骤的 Page / Frame / FrameLocator 查询作用域。

    frame 字段支持三类既有契约：frame name、``url=片段``、iframe/frame CSS。
    name/URL 能直接解析为 Frame 时优先使用；否则按 CSS 交给 ``frame_locator``。
    """
    page = context.page
    if page is None:
        raise WorkerError(Outcome.SELECTOR_FAILED, "页面未初始化")
    spec = (context.frame or "").strip()
    if not spec:
        return page

    frames = getattr(page, "frames", None)
    if frames is None:
        frames = []

    if spec.startswith("url="):
        fragment = spec[4:]
        matches = [frame for frame in frames if fragment and fragment in getattr(frame, "url", "")]
        if len(matches) == 1:
            return matches[0]
        if len(matches) > 1:
            raise WorkerError(Outcome.SELECTOR_FAILED, f"frame URL 匹配不唯一: {spec}")
        raise WorkerError(Outcome.SELECTOR_FAILED, f"未找到 frame URL: {spec}")

    name_matches = [frame for frame in frames if getattr(frame, "name", "") == spec]
    if len(name_matches) == 1:
        return name_matches[0]
    if len(name_matches) > 1:
        raise WorkerError(Outcome.SELECTOR_FAILED, f"frame name 匹配不唯一: {spec}")

    return page.frame_locator(spec)


def _locator(context: StepContext, selector: str):
    """在当前 Page/Frame 作用域内创建 Locator。"""
    return _frame_scope(context).locator(_normalize_selector(selector))


def _remaining_ms(deadline: float) -> int:
    """返回截止时间剩余毫秒，至少为 0。"""
    return max(0, int((deadline - time.monotonic()) * 1000))


async def _safe_op(coro, outcome_on_timeout: Outcome):
    """执行 Playwright 操作并归一化超时/瞬时元素异常。"""
    try:
        return await coro
    except PlaywrightTimeoutError as exc:
        raise WorkerError(outcome_on_timeout, f"操作超时: {exc}") from exc
    except PlaywrightError as exc:
        raise WorkerError(Outcome.SELECTOR_FAILED, f"元素操作失败: {exc}") from exc


async def _sleep_cancellable_ms(duration_ms: int, context: StepContext) -> None:
    """分片休眠，保证长延时时能及时响应取消。"""
    duration_ms = max(0, duration_ms)
    deadline = time.monotonic() + duration_ms / 1000
    while True:
        _check_cancel(context)
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            return
        await asyncio.sleep(min(0.1, remaining))


async def _click_locator(locator, timeout_ms: int) -> bool:
    """在同一预算内尝试正常点击和 attached 强制点击。"""
    if timeout_ms <= 0:
        return False
    target = locator.first
    deadline = time.monotonic() + timeout_ms / 1000
    try:
        await _safe_op(target.click(timeout=timeout_ms), Outcome.SELECTOR_FAILED)
        return True
    except WorkerError:
        pass

    remaining = _remaining_ms(deadline)
    if remaining <= 0:
        return False
    try:
        await target.wait_for(
            state="attached", timeout=max(_MIN_ATTACHED_MS, min(remaining, 1000))
        )
        await target.dispatch_event("click")
        return True
    except Exception:  # noqa: BLE001 — 候选失败后由调用方继续尝试
        return False


def _choose_text_index(texts: list[str], value: str) -> int | None:
    """从候选文本中选择唯一的精确/子串匹配项。"""
    needle = value.strip().casefold()
    normalized = [text.strip().casefold() for text in texts]
    exact = [idx for idx, text in enumerate(normalized) if text == needle]
    if len(exact) == 1:
        return exact[0]
    partial = [idx for idx, text in enumerate(normalized) if needle and needle in text]
    if len(partial) == 1:
        return partial[0]
    return None


async def _skip_or_fail(step: StepConfig, message: str) -> None:
    """按 required 语义决定容错跳过还是失败。"""
    if step.required:
        raise WorkerError(Outcome.SELECTOR_FAILED, message)
    logger.info("[step:%s] 可选步骤跳过: %s", step.id or step.step_type, message)


# ── 各类型处理器 ──


async def handle_input(page, step: StepConfig, context: StepContext) -> None:
    """在输入框填写值；普通操作失败后自动降级为 JS 原生 setter。"""
    _check_cancel(context)
    if not step.selector:
        raise WorkerError(Outcome.SELECTOR_FAILED, "input 步骤缺少 selector")
    value = step.value or ""
    locator = _locator(context, step.selector)
    timeout = step.timeout or context.default_timeout

    async def _force_input() -> None:
        await _locator(context, step.selector).evaluate(
            _FORCE_INPUT_JS, {"val": value, "doClear": bool(step.clear)}
        )

    if context.reveal_hidden:
        await _force_input()
        return
    try:
        if step.clear:
            await _safe_op(locator.fill(value, timeout=timeout), Outcome.SELECTOR_FAILED)
        else:
            await _safe_op(
                locator.press_sequentially(value, timeout=timeout), Outcome.SELECTOR_FAILED
            )
    except WorkerError:
        await _safe_op(
            _locator(context, step.selector).first.wait_for(
                state="attached", timeout=max(_MIN_ATTACHED_MS, min(timeout, 1000))
            ),
            Outcome.SELECTOR_FAILED,
        )
        await _force_input()


async def handle_click(page, step: StepConfig, context: StepContext) -> None:
    """点击元素，支持顶层逗号候选、隐藏元素降级与录制器文本候选兜底。"""
    _check_cancel(context)
    if not step.selector:
        raise WorkerError(Outcome.SELECTOR_FAILED, "click 步骤缺少 selector")
    timeout = step.timeout or context.default_timeout
    deadline = time.monotonic() + timeout / 1000

    for selector in _split_selector_candidates(step.selector):
        remaining = _remaining_ms(deadline)
        if remaining <= 0:
            break
        if await _click_locator(_locator(context, selector), remaining):
            return

        # 录制器历史版本可能把按钮文字直接作为候选值；CSS 尝试失败后再按文本兜底。
        if _looks_like_plain_text(selector):
            remaining = _remaining_ms(deadline)
            if remaining <= 0:
                break
            try:
                text_locator = _frame_scope(context).get_by_text(selector.strip(), exact=True)
                if await _click_locator(text_locator, remaining):
                    return
            except Exception:  # noqa: BLE001 — 继续尝试下一候选
                pass

    raise WorkerError(Outcome.SELECTOR_FAILED, f"未找到可点击元素: {step.selector}")


async def handle_select(page, step: StepConfig, context: StepContext) -> None:
    """原生 select 选择。

    先按 option value / 精确文本匹配，再按唯一子串文本匹配。空 value 自动跳过；
    元素或选项找不到时由 ``required`` 决定失败还是跳过。
    """
    _check_cancel(context)
    if not step.selector:
        raise WorkerError(Outcome.SELECTOR_FAILED, "select 步骤缺少 selector")
    value = (step.value or "").strip()
    if not value:
        return

    timeout = step.timeout or context.default_timeout
    locator = _locator(context, step.selector).first
    try:
        await _safe_op(
            locator.wait_for(state="attached", timeout=timeout), Outcome.SELECTOR_FAILED
        )
        options = locator.locator("option")
        items = await options.evaluate_all(
            "els => els.map(el => ({value: String(el.value ?? ''), text: String(el.textContent ?? '')}))"
        )
    except WorkerError as exc:
        await _skip_or_fail(step, f"找不到下拉框: {step.selector}: {exc.message}")
        return
    except Exception as exc:  # noqa: BLE001
        await _skip_or_fail(step, f"读取下拉选项失败: {exc}")
        return

    chosen_value: str | None = None
    for item in items:
        if str(item.get("value", "")) == value:
            chosen_value = str(item.get("value", ""))
            break

    if chosen_value is None:
        texts = [str(item.get("text", "")) for item in items]
        idx = _choose_text_index(texts, value)
        if idx is not None:
            chosen_value = str(items[idx].get("value", ""))

    if chosen_value is None:
        await _skip_or_fail(step, f"下拉框未找到唯一匹配选项: {value}")
        return

    try:
        await _safe_op(
            locator.select_option(value=chosen_value, timeout=timeout), Outcome.SELECTOR_FAILED
        )
    except WorkerError as exc:
        await _skip_or_fail(step, f"选择下拉项失败: {value}: {exc.message}")


async def _find_click_select_option(
    context: StepContext, option_selector: str | None, value: str
):
    """根据文本在 option_selector 范围内寻找唯一选项 Locator。"""
    scope = _frame_scope(context)
    if not option_selector:
        exact = scope.get_by_text(value, exact=True)
        if await exact.count() == 1:
            return exact.first
        partial = scope.get_by_text(value, exact=False)
        if await partial.count() == 1:
            return partial.first
        return None

    base = scope.locator(_normalize_selector(option_selector))
    count = await base.count()
    if count == 0:
        return None

    # option_selector 指向多个选项元素时，直接按元素文本选择唯一项。
    if count > 1:
        texts = await base.all_inner_texts()
        idx = _choose_text_index(texts, value)
        return base.nth(idx) if idx is not None else None

    # 只匹配一个节点时，它可能本身就是选项，也可能是整个选项容器。
    first = base.first
    try:
        own_text = (await first.inner_text()).strip()
    except Exception:  # noqa: BLE001
        own_text = ""
    if own_text.casefold() == value.strip().casefold():
        return first

    exact = first.get_by_text(value, exact=True)
    if await exact.count() == 1:
        return exact.first
    partial = first.get_by_text(value, exact=False)
    if await partial.count() == 1:
        return partial.first
    return None


async def handle_click_select(page, step: StepConfig, context: StepContext) -> None:
    """执行自定义下拉/按钮组选择。

    ``selector`` 只负责展开，``option_selector`` 负责限定搜索范围，真正的目标选项
    始终按 ``value`` 文本匹配。整个动作共用同一个步骤 timeout 预算，避免两次点击
    各自消耗完整 timeout 导致单步实际耗时翻倍。
    """
    _check_cancel(context)
    if not step.selector:
        raise WorkerError(Outcome.SELECTOR_FAILED, "click_select 步骤缺少 selector")
    value = (step.value or "").strip()
    if not value:
        return

    timeout = step.timeout or context.default_timeout
    deadline = time.monotonic() + timeout / 1000
    if not await _click_locator(_locator(context, step.selector), _remaining_ms(deadline)):
        await _skip_or_fail(step, f"找不到下拉触发器: {step.selector}")
        return

    raw_delay = step.extra_fields.get("select_delay", _DEFAULT_SELECT_DELAY_MS)
    try:
        delay_ms = max(0, min(int(raw_delay), timeout))
    except (TypeError, ValueError):
        delay_ms = _DEFAULT_SELECT_DELAY_MS
    if delay_ms:
        await _sleep_cancellable_ms(min(delay_ms, _remaining_ms(deadline)), context)

    if _remaining_ms(deadline) <= 0:
        await _skip_or_fail(step, f"展开下拉框后已超时: {value}")
        return

    try:
        option = await _find_click_select_option(context, step.option_selector, value)
    except (PlaywrightError, WorkerError) as exc:
        await _skip_or_fail(step, f"查找下拉选项失败: {value}: {exc}")
        return

    if option is None:
        await _skip_or_fail(step, f"未找到唯一匹配的下拉选项: {value}")
        return
    if not await _click_locator(option, _remaining_ms(deadline)):
        await _skip_or_fail(step, f"点击下拉选项失败: {value}")


async def handle_wait(page, step: StepConfig, context: StepContext) -> None:
    """兼容 wait 的两种历史语义。

    有 selector 时按任务指南等待元素可见；没有 selector 时保留旧 Worker 的固定延时
    行为，避免历史 ``type=wait + duration`` 任务突然失效。
    """
    if step.selector:
        await handle_wait_for_selector(page, step, context)
    else:
        await _sleep_cancellable_ms(step.duration, context)


async def handle_wait_for_selector(page, step: StepConfig, context: StepContext) -> None:
    """等待选择器对应元素可见。"""
    _check_cancel(context)
    if not step.selector:
        raise WorkerError(Outcome.SELECTOR_FAILED, "wait_for_selector 步骤缺少 selector")
    timeout = step.timeout or context.default_timeout
    await _safe_op(
        _locator(context, step.selector).first.wait_for(state="visible", timeout=timeout),
        Outcome.SELECTOR_FAILED,
    )


async def handle_wait_url(page, step: StepConfig, context: StepContext) -> None:
    """等待当前 URL 匹配指定正则。"""
    _check_cancel(context)
    if not step.pattern:
        raise WorkerError(Outcome.NAVIGATION_TIMEOUT, "wait_url 步骤缺少 pattern")
    try:
        regex = re.compile(step.pattern)
    except re.error as exc:
        raise WorkerError(Outcome.UNKNOWN_ERROR, f"URL 正则非法: {step.pattern}: {exc}") from exc

    timeout = step.timeout or context.navigation_timeout
    deadline = time.monotonic() + timeout / 1000
    while time.monotonic() < deadline:
        _check_cancel(context)
        try:
            current = context.page.url
        except Exception as exc:  # noqa: BLE001
            raise WorkerError(
                Outcome.NAVIGATION_TIMEOUT, f"读取页面 URL 失败: {exc}"
            ) from exc
        if regex.search(current):
            return
        await asyncio.sleep(0.2)
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
    await _safe_op(
        page.screenshot(path=local_path, full_page=full_page), Outcome.SELECTOR_FAILED
    )
    context.screenshots.append(local_path)
    context.emit("screenshot", {"path": local_path, "step_id": step.id})


async def handle_evaluate(page, step: StepConfig, context: StepContext) -> None:
    """执行 JavaScript 并可选存储原生结果。"""
    _check_cancel(context)
    script = step.effective_script
    if not script:
        raise WorkerError(Outcome.UNKNOWN_ERROR, "evaluate 步骤缺少 script/code")
    timeout_s = max(0.1, (step.timeout or context.default_timeout) / 1000)

    task = asyncio.ensure_future(page.evaluate(script))
    deadline = time.monotonic() + timeout_s
    while not task.done():
        if context.cancel_event is not None and context.cancel_event.is_set():
            task.cancel()
            try:
                await page.close()
            except Exception:  # noqa: BLE001
                pass
            raise StepCancelled("JS 执行已取消，页面已中断")
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            task.cancel()
            try:
                await page.close()
            except Exception:  # noqa: BLE001
                pass
            raise WorkerError(
                Outcome.UNKNOWN_ERROR, f"JS 执行超时（{timeout_s}s），已强制中断"
            )
        await asyncio.wait({task}, timeout=min(0.1, remaining))

    try:
        result = task.result()
    except Exception as exc:  # noqa: BLE001
        raise WorkerError(Outcome.UNKNOWN_ERROR, f"JS 执行失败: {exc}") from exc

    if step.store_as:
        # 保留原生类型，success_condition 不会把 JS null 错判为字符串 "None" 的真值；
        # 模板引用时再通过 _template_value 做稳定字符串化。
        context.results[step.store_as] = result


async def handle_navigate(page, step: StepConfig, context: StepContext) -> None:
    """导航到指定 URL（``navigate`` / ``goto`` 共用）。"""
    _check_cancel(context)
    url = step.extra_fields.get("url") or step.value or step.selector
    if not url:
        raise WorkerError(Outcome.NAVIGATION_TIMEOUT, "导航步骤缺少目标 URL")

    valid_wait_until = ("load", "domcontentloaded", "networkidle", "commit")
    raw = step.extra_fields.get("wait_until", "domcontentloaded")
    wait_until = raw if isinstance(raw, str) and raw in valid_wait_until else "load"
    if wait_until != raw:
        logger.warning(
            "[navigate] wait_until 值 '%s' 无效，可选: %s，使用默认 'load'",
            raw,
            ", ".join(valid_wait_until),
        )

    timeout = step.timeout or context.navigation_timeout
    try:
        await page.goto(url, wait_until=wait_until, timeout=timeout)
    except Exception as exc:  # noqa: BLE001
        raise _classify_navigation_error(exc, str(url))


async def handle_assert_text(page, step: StepConfig, context: StepContext) -> None:
    """断言页面出现指定文本。"""
    _check_cancel(context)
    value = step.value
    if not value:
        raise WorkerError(Outcome.SELECTOR_FAILED, "assert_text 步骤需要 value")
    timeout = step.timeout or context.default_timeout
    try:
        await page.wait_for_function(
            "arg => document.body.innerText.includes(arg)", arg=value, timeout=timeout
        )
    except PlaywrightTimeoutError as exc:
        raise WorkerError(
            Outcome.ASSERTION_FAILED, f"等待文本超时 ({timeout}ms): {value}"
        ) from exc
    except Exception as exc:  # noqa: BLE001
        raise WorkerError(
            Outcome.UNKNOWN_ERROR, f"等待文本失败: {value}, 错误: {exc}"
        ) from exc
    logger.info("[assert_text] 检测到文本: '%s'", value)


async def handle_upload_file(page, step: StepConfig, context: StepContext) -> None:
    """向文件输入上传本地文件，兼容 path/value 两种历史写法。"""
    _check_cancel(context)
    if not step.selector:
        raise WorkerError(Outcome.SELECTOR_FAILED, "upload_file 步骤缺少 selector")
    file_path = step.path or step.value
    if not file_path:
        raise WorkerError(Outcome.SELECTOR_FAILED, "upload_file 步骤缺少文件路径")
    timeout = step.timeout or context.default_timeout
    await _safe_op(
        _locator(context, step.selector).set_input_files(file_path, timeout=timeout),
        Outcome.SELECTOR_FAILED,
    )


async def handle_ocr(page, step: StepConfig, context: StepContext) -> None:
    """对元素截图后使用 ddddocr 识别，并填入目标输入框。"""
    _check_cancel(context)
    if not step.selector:
        raise WorkerError(Outcome.SELECTOR_FAILED, "ocr 步骤缺少 selector")
    try:
        ocr = await asyncio.wait_for(
            asyncio.to_thread(_get_ocr, step.old, step.char_range),
            timeout=OCR_TIMEOUT_SECS,
        )
    except asyncio.TimeoutError:
        raise WorkerError(
            Outcome.UNKNOWN_ERROR,
            f"OCR 模型加载超时（>{OCR_TIMEOUT_SECS}s）。模型为包内自带，仅本地加载，"
            "若持续超时请检查 OCR 依赖是否完整（uv add ddddocr）",
        ) from None
    except Exception as exc:  # noqa: BLE001
        raise WorkerError(
            Outcome.UNKNOWN_ERROR,
            f"ddddocr 未安装: {exc}。请在设置页点「安装 OCR 依赖」（uv add ddddocr）后重试",
        ) from exc

    locator = _locator(context, step.selector)
    timeout = step.timeout or context.default_timeout
    await _safe_op(locator.wait_for(state="visible", timeout=timeout), Outcome.SELECTOR_FAILED)
    img_bytes = await _safe_op(locator.screenshot(), Outcome.SELECTOR_FAILED)
    img_bytes = _preprocess_ocr_image(img_bytes)
    try:
        text = await asyncio.wait_for(
            asyncio.to_thread(ocr.classification, img_bytes), timeout=OCR_TIMEOUT_SECS
        )
    except asyncio.TimeoutError:
        raise WorkerError(
            Outcome.UNKNOWN_ERROR, f"OCR 识别超时（>{OCR_TIMEOUT_SECS}s）"
        ) from None

    if step.store_as:
        context.results[step.store_as] = text
    if step.target_selector:
        await _safe_op(
            _locator(context, step.target_selector).fill(text, timeout=timeout),
            Outcome.SELECTOR_FAILED,
        )


# 步骤类型 → 处理器映射。wait/sleep 共用兼容处理器：有 selector 等元素，无 selector 休眠。
_STEP_HANDLERS: dict[str, Callable] = {
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
    "eval": handle_evaluate,
    "custom_js": handle_evaluate,
    "custom": handle_evaluate,
    "navigate": handle_navigate,
    "goto": handle_navigate,
    "assert_text": handle_assert_text,
    "upload_file": handle_upload_file,
    "ocr": handle_ocr,
}


def _get_handler(step_type: str):
    """根据步骤类型返回处理器，兼容别名。"""
    return _STEP_HANDLERS.get(step_type)


async def run_step_async(
    page,
    raw_step: StepConfig,
    context: StepContext,
    step_index: int | None = None,
    total_steps: int | None = None,
) -> None:
    """异步执行单个步骤。"""
    step = _resolve(raw_step, context)
    handler = _get_handler(step.step_type)
    if handler is None:
        raise WorkerError(Outcome.UNKNOWN_ERROR, f"未知步骤类型: {step.step_type}")

    context.emit(
        "step_progress",
        {
            "step_id": step.id,
            "step_type": step.step_type,
            "description": step.description,
            **({"step_index": step_index} if step_index is not None else {}),
            **({"total_steps": total_steps} if total_steps is not None else {}),
        },
    )

    prev_frame = context.frame
    context.frame = step.frame or None
    try:
        await handler(page, step, context)
    finally:
        context.frame = prev_frame
