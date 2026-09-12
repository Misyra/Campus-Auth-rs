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
# OCR 实例缓存/图片预处理已迁至 ocr_runtime.py；顶部导入同时兼作再导出，
# 兼容旧调用方 `from step_handlers import _get_ocr` 等用法
from ocr_runtime import (
    OCR_TIMEOUT_SECS,
    _get_ocr,
    _preprocess_ocr_image,
    ocr_load_in_progress,
)
from playwright.async_api import Error as PlaywrightError
from playwright.async_api import TimeoutError as PlaywrightTimeoutError
from variable_resolver import resolve, resolve_javascript

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

    frame_scope: Any = None
    """当前步骤已解析的 Frame / FrameLocator；仅在步骤执行期间有效。"""

    on_page: Callable[[Any], None] | None = None
    """发现弹窗或新标签页时通知 Worker 更新活动 Page。"""

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
        code = resolve_javascript(script, variables)
        resolved.code = code
        resolved.script = code

    resolved.extras = _resolve_extra(resolved.extras, variables)
    return resolved


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

# click/input 降级路径希望至少预留的等待预算（毫秒）；小 timeout 会自动按比例收缩
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
    value = selector.strip()
    # 显式 Playwright selector engine 的逗号属于该引擎语法或文本内容，
    # 不能按 CSS 候选列表拆分；裸 XPath 同理。
    if value.startswith("/") or re.match(r"^[A-Za-z_][A-Za-z0-9_-]*=", value):
        return [value]

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


def _match_frame(page: Any, spec: str, *, allow_css_fallback: bool) -> Any:
    """按 frame 规格解析 Page / Frame / FrameLocator 查询作用域（两类步骤共用）。

    匹配规则（与历史契约一致）：
    - 空规格 → 返回 Page 本身（主 frame 执行）；
    - ``url=片段`` → 在 ``page.frames`` 中按 URL 子串匹配，必须唯一命中；
      空片段视为未找到（空子串会命中全部 frame）；
    - 其他规格 → 按 frame name 精确匹配，必须唯一命中。

    异常语义（均抛 ``SELECTOR_FAILED``）：
    - URL 匹配多个或未找到：合并为同一文案（历史上两类步骤文案发散，现统一）；
    - name 匹配多个：报"匹配不唯一"；
    - name 未命中：``allow_css_fallback=True``（元素查询步骤）时降级为
      ``page.frame_locator(spec)`` 按 CSS 定位 iframe/frame；``False``
      （脚本执行步骤）时显式报错——脚本无法经 frame_locator 执行，
      静默落到主 frame 会让 iframe 门户场景必然假失败。
    """
    if page is None:
        raise WorkerError(Outcome.SELECTOR_FAILED, "页面未初始化")
    if not spec:
        return page

    frames = getattr(page, "frames", None) or []
    if spec.startswith("url="):
        fragment = spec[4:]
        matches = [frame for frame in frames if fragment and fragment in getattr(frame, "url", "")]
        if len(matches) == 1:
            return matches[0]
        raise WorkerError(Outcome.SELECTOR_FAILED, f"frame URL 匹配不唯一或未找到: {spec}")

    name_matches = [frame for frame in frames if getattr(frame, "name", "") == spec]
    if len(name_matches) == 1:
        return name_matches[0]
    if len(name_matches) > 1:
        raise WorkerError(Outcome.SELECTOR_FAILED, f"frame name 匹配不唯一: {spec}")
    if allow_css_fallback:
        return page.frame_locator(spec)
    raise WorkerError(
        Outcome.SELECTOR_FAILED,
        f"脚本/URL 操作的 frame 仅支持 name 或 url= 规格，不支持 CSS 选择器: {spec}",
    )


def _frame_scope(context: StepContext) -> Any:
    """返回当前步骤的 Page / Frame / FrameLocator 查询作用域。

    frame 字段支持三类既有契约：frame name、``url=片段``、iframe/frame CSS。
    name/URL 能直接解析为 Frame 时优先使用；否则按 CSS 交给 ``frame_locator``。
    """
    if context.frame_scope is not None:
        return context.frame_scope
    return _match_frame(context.page, (context.frame or "").strip(), allow_css_fallback=True)


def _looks_like_frame_css(spec: str) -> bool:
    """保守识别 iframe/frame CSS，避免把普通 frame name 误作 CSS。"""
    value = spec.strip().lower()
    if value.startswith(("#", ".", "[", "//", "css=", "xpath=")):
        return True
    return bool(re.match(r"^(?:iframe|frame)(?:[.#\[:>+~\s]|$)", value))


async def _resolve_frame_scope(
    context: StepContext,
    spec: str,
    *,
    allow_css_fallback: bool,
) -> Any:
    """在步骤预算内等待按 name/URL 动态出现的 Frame。"""
    if not spec:
        return context.page
    if context.page is None:
        return None
    if _looks_like_frame_css(spec):
        if allow_css_fallback:
            return context.page.frame_locator(_normalize_selector(spec))
        raise WorkerError(
            Outcome.SELECTOR_FAILED,
            f"脚本/URL 操作的 frame 仅支持 name 或 url= 规格，不支持 CSS 选择器: {spec}",
        )

    while True:
        _check_cancel(context)
        frames = getattr(context.page, "frames", None) or []
        if spec.startswith("url="):
            fragment = spec[4:]
            matches = [
                frame for frame in frames
                if fragment and fragment in getattr(frame, "url", "")
            ]
            label = "URL"
        else:
            matches = [frame for frame in frames if getattr(frame, "name", "") == spec]
            label = "name"
        if len(matches) == 1:
            return matches[0]
        if len(matches) > 1:
            raise WorkerError(Outcome.SELECTOR_FAILED, f"frame {label} 匹配不唯一: {spec}")
        await asyncio.sleep(0.05)


def _page_is_open(page: Any) -> bool:
    """兼容真实 Playwright Page 与测试替身地判断页面是否仍可用。"""
    try:
        checker = getattr(page, "is_closed", None)
        return not bool(checker()) if callable(checker) else True
    except Exception:  # noqa: BLE001 — 页面切换本身是 best-effort
        return False


def _adopt_latest_page(context: StepContext) -> None:
    """将浏览器上下文中新出现的最后一个活动页面接管为后续步骤目标。"""
    current = context.page
    browser_context = getattr(current, "context", None)
    try:
        pages = list(browser_context.pages) if browser_context is not None else []
    except Exception:  # noqa: BLE001 — 页面枚举失败时继续使用当前页
        return
    live_pages = [page for page in pages if _page_is_open(page)]
    if not live_pages:
        return
    latest = live_pages[-1]
    if latest is current:
        return
    context.page = latest
    if context.on_page is not None:
        context.on_page(latest)
    logger.info("检测到新标签页/弹窗，后续步骤已切换到新页面")


def _locator(context: StepContext, selector: str) -> Any:
    """在当前 Page/Frame 作用域内创建 Locator。"""
    return _frame_scope(context).locator(_normalize_selector(selector))


def _remaining_ms(deadline: float) -> int:
    """返回截止时间剩余毫秒，至少为 0。"""
    return max(0, int((deadline - time.monotonic()) * 1000))


def _primary_timeout_ms(total_ms: int) -> int:
    """给正常元素操作分配预算，并为 attached/JS 降级路径预留时间。

    长 timeout 最多预留 1s；短 timeout 最多预留一半，避免降级机制因为正常
    Playwright 操作把整个预算吃光而形同虚设。
    """
    total_ms = max(0, int(total_ms))
    if total_ms <= 1:
        return total_ms
    reserve = min(
        total_ms // 2,
        max(_MIN_ATTACHED_MS, min(total_ms // 5, 1000)),
    )
    return max(1, total_ms - reserve)


async def _safe_op(coro: Any, outcome_on_timeout: Outcome) -> Any:
    """执行 Playwright 操作并归一化超时/瞬时元素异常。"""
    try:
        return await coro
    except PlaywrightTimeoutError as exc:
        raise WorkerError(outcome_on_timeout, f"操作超时: {exc}") from exc
    except PlaywrightError as exc:
        raise WorkerError(Outcome.SELECTOR_FAILED, f"元素操作失败: {exc}") from exc


async def _sleep_cancellable(seconds: float, context: StepContext, *, slice_s: float = 0.2) -> None:
    """分片可取消休眠（秒版）：每片结束前检查取消事件。

    取消检查时机：进入循环先查一次（时长 ≤0 时也检查，保证已取消的调用
    立即抛出），随后每片 sleep 结束回到循环顶部再查，长延时最多延迟一个
    分片（默认 0.2s）响应取消。
    """
    deadline = time.monotonic() + max(0.0, seconds)
    while True:
        _check_cancel(context)
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            return
        await asyncio.sleep(min(slice_s, remaining))


async def _sleep_cancellable_ms(duration_ms: int, context: StepContext) -> None:
    """分片休眠（毫秒版，分片粒度 0.1s），保证长延时时能及时响应取消。

    与秒版共用同一实现，仅分片粒度不同（历史行为保持不变）。
    """
    await _sleep_cancellable(duration_ms / 1000, context, slice_s=0.1)


async def _click_locator(locator, timeout_ms: int) -> bool:
    """在同一个截止时间内尝试正常点击和 attached 强制点击。"""
    if timeout_ms <= 0:
        return False
    target = locator.first
    deadline = time.monotonic() + timeout_ms / 1000
    primary_timeout = _primary_timeout_ms(timeout_ms)
    try:
        await _safe_op(target.click(timeout=primary_timeout), Outcome.SELECTOR_FAILED)
        return True
    except WorkerError as exc:
        logger.debug("常规点击失败，尝试强制点击降级: %s", exc.message)

    remaining = _remaining_ms(deadline)
    if remaining <= 0:
        return False
    try:
        await target.wait_for(state="attached", timeout=min(remaining, 1000))
        remaining = _remaining_ms(deadline)
        if remaining <= 0:
            return False
        await target.dispatch_event("click", timeout=remaining)
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


def _raise_step_failure(message: str) -> None:
    """将处理器失败统一上抛，由任务编排层决定必需/可选语义。"""
    raise WorkerError(Outcome.SELECTOR_FAILED, message)


# ── 各类型处理器 ──


async def handle_input(page, step: StepConfig, context: StepContext) -> None:
    """在输入框填写值；普通操作失败后自动降级为 JS 原生 setter。"""
    _check_cancel(context)
    if not step.selector:
        raise WorkerError(Outcome.SELECTOR_FAILED, "input 步骤缺少 selector")
    value = step.value or ""
    locator = _locator(context, step.selector)
    timeout = step.timeout or context.default_timeout
    deadline = time.monotonic() + timeout / 1000

    async def _force_input(timeout_ms: int) -> None:
        await _locator(context, step.selector).evaluate(
            _FORCE_INPUT_JS,
            {"val": value, "doClear": bool(step.clear)},
            timeout=max(1, timeout_ms),
        )

    if context.reveal_hidden:
        await _safe_op(_force_input(timeout), Outcome.SELECTOR_FAILED)
        return

    primary_timeout = _primary_timeout_ms(timeout)
    try:
        if step.clear:
            await _safe_op(
                locator.fill(value, timeout=primary_timeout), Outcome.SELECTOR_FAILED
            )
        else:
            await _safe_op(
                locator.press_sequentially(value, timeout=primary_timeout),
                Outcome.SELECTOR_FAILED,
            )
        return
    except WorkerError as exc:
        logger.debug("常规输入失败，尝试 JS 强制输入降级: %s", exc.message)

    remaining = _remaining_ms(deadline)
    if remaining <= 0:
        raise WorkerError(Outcome.SELECTOR_FAILED, f"输入元素操作超时: {step.selector}")
    await _safe_op(
        _locator(context, step.selector).first.wait_for(
            state="attached", timeout=min(remaining, 1000)
        ),
        Outcome.SELECTOR_FAILED,
    )
    remaining = _remaining_ms(deadline)
    if remaining <= 0:
        raise WorkerError(Outcome.SELECTOR_FAILED, f"输入元素降级操作超时: {step.selector}")
    await _safe_op(_force_input(remaining), Outcome.SELECTOR_FAILED)


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
            except Exception as exc:  # noqa: BLE001 — 继续尝试下一候选
                logger.debug("文本候选 %s 点击失败，继续尝试下一候选: %s", selector, exc)

    raise WorkerError(Outcome.SELECTOR_FAILED, f"未找到可点击元素: {step.selector}")


async def handle_select(page, step: StepConfig, context: StepContext) -> None:
    """原生 select 选择。

    先按 option value / 精确文本匹配，再按唯一子串文本匹配。空 value 显式失败；
    元素或选项找不到时统一上抛，由任务编排层处理 ``required`` 语义。元素等待
    与最终选择共用同一个步骤 timeout 截止时间。
    """
    _check_cancel(context)
    if not step.selector:
        raise WorkerError(Outcome.SELECTOR_FAILED, "select 步骤缺少 selector")
    value = (step.value or "").strip()
    if not value:
        # 空 value 静默 no-op 会让任务"全绿"但运营商根本没选（校验层已拦截
        # 新任务，此处对绕过校验的存量任务显式报错而非静默跳过）
        raise WorkerError(Outcome.UNKNOWN_ERROR, "select 步骤缺少 value（必填）")

    timeout = step.timeout or context.default_timeout
    deadline = time.monotonic() + timeout / 1000
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
        _raise_step_failure(f"找不到下拉框: {step.selector}: {exc.message}")
        return
    except Exception as exc:  # noqa: BLE001
        _raise_step_failure(f"读取下拉选项失败: {exc}")
        return

    # 精确值匹配：取首个 option value 与目标值完全相等的项（未命中再走文本匹配兜底）
    chosen_value = next(
        (str(item.get("value", "")) for item in items if str(item.get("value", "")) == value),
        None,
    )

    if chosen_value is None:
        texts = [str(item.get("text", "")) for item in items]
        idx = _choose_text_index(texts, value)
        if idx is not None:
            chosen_value = str(items[idx].get("value", ""))

    if chosen_value is None:
        _raise_step_failure(f"下拉框未找到唯一匹配选项: {value}")
        return

    remaining = _remaining_ms(deadline)
    if remaining <= 0:
        _raise_step_failure(f"选择下拉项超时: {value}")
        return
    try:
        await _safe_op(
            locator.select_option(value=chosen_value, timeout=remaining),
            Outcome.SELECTOR_FAILED,
        )
    except WorkerError as exc:
        _raise_step_failure(f"选择下拉项失败: {value}: {exc.message}")


async def _find_click_select_option(
    context: StepContext, option_selector: str | None, value: str
) -> Any:
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
    except Exception as exc:  # noqa: BLE001
        logger.debug("读取选项自身文本失败，按空处理: %s", exc)
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
        # 与 select 分支同语义：静默 no-op 会让任务"全绿"但实际什么都没选
        #（校验层已拦截新任务，此处对绕过校验的存量任务显式报错而非静默跳过）
        raise WorkerError(Outcome.UNKNOWN_ERROR, "click_select 步骤缺少 value（必填）")

    timeout = step.timeout or context.default_timeout
    deadline = time.monotonic() + timeout / 1000
    if not await _click_locator(_locator(context, step.selector), _remaining_ms(deadline)):
        _raise_step_failure(f"找不到下拉触发器: {step.selector}")
        return

    raw_delay = step.extra_fields.get("select_delay", _DEFAULT_SELECT_DELAY_MS)
    try:
        delay_ms = max(0, min(int(raw_delay), timeout))
    except (TypeError, ValueError):
        delay_ms = _DEFAULT_SELECT_DELAY_MS
    if delay_ms:
        await _sleep_cancellable_ms(min(delay_ms, _remaining_ms(deadline)), context)

    if _remaining_ms(deadline) <= 0:
        _raise_step_failure(f"展开下拉框后已超时: {value}")
        return

    try:
        option = await _find_click_select_option(context, step.option_selector, value)
    except (PlaywrightError, WorkerError) as exc:
        _raise_step_failure(f"查找下拉选项失败: {value}: {exc}")
        return

    if option is None:
        _raise_step_failure(f"未找到唯一匹配的下拉选项: {value}")
        return
    if not await _click_locator(option, _remaining_ms(deadline)):
        _raise_step_failure(f"点击下拉选项失败: {value}")


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
    """等待当前页面或目标 Frame 的 URL 匹配指定正则。"""
    _check_cancel(context)
    if not step.pattern:
        raise WorkerError(Outcome.NAVIGATION_TIMEOUT, "wait_url 步骤缺少 pattern")
    try:
        regex = re.compile(step.pattern)
    except re.error as exc:
        raise WorkerError(Outcome.UNKNOWN_ERROR, f"URL 正则非法: {step.pattern}: {exc}") from exc

    timeout = step.timeout or context.navigation_timeout
    deadline = time.monotonic() + timeout / 1000
    scope = _script_scope(context)
    while time.monotonic() < deadline:
        _check_cancel(context)
        try:
            current = scope.url
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
    timeout = step.timeout or context.default_timeout
    await _safe_op(
        page.screenshot(path=local_path, full_page=full_page, timeout=timeout),
        Outcome.SELECTOR_FAILED,
    )
    context.screenshots.append(local_path)
    context.emit("screenshot", {"path": local_path, "step_id": step.id})


def _script_scope(context: StepContext) -> Any:
    """返回可执行 JS 或读取 URL 的 Page / Frame 作用域。

    这类操作无法走 ``frame_locator``（仅支持元素查询）：
    frame 规格只能解析为 Frame（name / ``url=`` 片段）；CSS 选择器形式显式报错，
    避免脚本静默在主 frame 执行（iframe 门户场景必然假失败）。
    """
    if context.frame_scope is not None:
        return context.frame_scope
    return _match_frame(context.page, (context.frame or "").strip(), allow_css_fallback=False)


async def _suppress_task(task: "asyncio.Future[Any]") -> None:
    """有限等待被取消的任务退出（仅本地断开 await，不关共享页）。

    任务的 CancelledError 不应外溢；evaluate 内部把取消包装成其他异常时同样忽略。
    Playwright 驱动异常时取消也可能迟迟不返回，因此最多等待一秒，避免步骤超时
    处理自身再次无限挂起。
    """
    try:
        done, _ = await asyncio.wait({task}, timeout=1.0)
        if done:
            task.result()
        else:
            # 任务稍后才结算时也要取走异常，避免事件循环输出
            # "Task exception was never retrieved" 干扰 Worker stderr。
            def _consume_result(future: "asyncio.Future[Any]") -> None:
                try:
                    future.result()
                except (asyncio.CancelledError, Exception):
                    pass

            task.add_done_callback(_consume_result)
    except asyncio.CancelledError:
        pass
    except Exception:  # noqa: BLE001 — 包装型异常一并吞掉，错误由调用方显式抛出
        pass


async def handle_evaluate(page, step: StepConfig, context: StepContext) -> None:
    """执行 JavaScript 并可选存储原生结果（``context.frame`` 非空时在对应 Frame 内执行）。"""
    _check_cancel(context)
    script = step.effective_script
    if not script:
        raise WorkerError(Outcome.UNKNOWN_ERROR, "evaluate 步骤缺少 script/code")
    timeout_s = max(0.1, (step.timeout or context.default_timeout) / 1000)

    scope = _script_scope(context)
    task = asyncio.ensure_future(scope.evaluate(script))
    deadline = time.monotonic() + timeout_s
    try:
        while not task.done():
            if context.cancel_event is not None and context.cancel_event.is_set():
                raise StepCancelled("JS 执行已取消")
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise WorkerError(
                    Outcome.UNKNOWN_ERROR, f"JS 执行超时（{timeout_s}s），已中断 JS 调用"
                )
            await asyncio.wait({task}, timeout=min(0.1, remaining))

        result = task.result()
    except Exception as exc:  # noqa: BLE001
        if isinstance(exc, WorkerError):
            raise
        raise WorkerError(Outcome.UNKNOWN_ERROR, f"JS 执行失败: {exc}") from exc
    finally:
        if not task.done():
            # 只取消 JS 调用本身，不再 page.close()：关页会让后续步骤
            # （含失败截图）全部报废。外层步骤 deadline 取消处理器时也走此收尾。
            task.cancel()
            await _suppress_task(task)

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
    wait_until = raw if isinstance(raw, str) and raw in valid_wait_until else "domcontentloaded"
    if wait_until != raw:
        logger.warning(
            "[navigate] wait_until 值 '%s' 无效，可选: %s，使用默认 'domcontentloaded'",
            raw,
            ", ".join(valid_wait_until),
        )

    timeout = step.timeout or context.navigation_timeout
    try:
        await page.goto(url, wait_until=wait_until, timeout=timeout)
    except Exception as exc:  # noqa: BLE001
        raise _classify_navigation_error(exc, str(url))


async def handle_assert_text(page, step: StepConfig, context: StepContext) -> None:
    """断言指定元素或页面正文中出现文本。"""
    _check_cancel(context)
    value = step.value
    if not value:
        raise WorkerError(Outcome.SELECTOR_FAILED, "assert_text 步骤需要 value")
    timeout = step.timeout or context.default_timeout
    selector = step.selector or "body"
    try:
        await _locator(context, selector).filter(has_text=value).first.wait_for(
            state="visible", timeout=timeout
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
    deadline = time.monotonic() + (step.timeout or context.default_timeout) / 1000
    try:
        remaining = max(0.001, min(OCR_TIMEOUT_SECS, deadline - time.monotonic()))
        load_budget = remaining
        ocr = await asyncio.wait_for(
            asyncio.to_thread(_get_ocr, step.old, step.char_range),
            timeout=remaining,
        )
    except asyncio.TimeoutError:
        # 文案分流：模型首次加载中的超时（并发/重试等待或慢加载）不应误导用户重装依赖
        if ocr_load_in_progress(step.old, step.char_range):
            raise WorkerError(
                Outcome.UNKNOWN_ERROR,
                f"OCR 模型仍在首次加载（已超过 {load_budget:g}s）。"
                "加载完成会写入日志，稍后可直接重试，无需重装依赖",
            ) from None
        raise WorkerError(
            Outcome.UNKNOWN_ERROR,
            f"OCR 模型加载超时（>{load_budget:g}s）。模型为包内自带，仅本地加载，"
            "若持续超时请检查 OCR 依赖是否完整（uv add ddddocr）",
        ) from None
    except Exception as exc:  # noqa: BLE001
        raise WorkerError(
            Outcome.UNKNOWN_ERROR,
            f"ddddocr 未安装: {exc}。请在设置页点「安装 OCR 依赖」（uv add ddddocr）后重试",
        ) from exc

    locator = _locator(context, step.selector)
    remaining_ms = _remaining_ms(deadline)
    if remaining_ms <= 0:
        raise WorkerError(Outcome.SELECTOR_FAILED, "OCR 步骤在等待验证码前已超时")
    await _safe_op(
        locator.wait_for(state="visible", timeout=remaining_ms), Outcome.SELECTOR_FAILED
    )
    remaining_ms = _remaining_ms(deadline)
    if remaining_ms <= 0:
        raise WorkerError(Outcome.SELECTOR_FAILED, "OCR 步骤在截取验证码前已超时")
    img_bytes = await _safe_op(
        locator.screenshot(timeout=remaining_ms), Outcome.SELECTOR_FAILED
    )
    img_bytes = _preprocess_ocr_image(img_bytes)
    try:
        remaining = max(0.001, min(OCR_TIMEOUT_SECS, deadline - time.monotonic()))
        inference_budget = remaining
        text = await asyncio.wait_for(
            asyncio.to_thread(ocr.classification_with_timeout, img_bytes, remaining),
            timeout=remaining,
        )
    except asyncio.TimeoutError:
        raise WorkerError(
            Outcome.UNKNOWN_ERROR, f"OCR 识别超时（>{inference_budget:g}s）"
        ) from None

    # 与 ocr_recognize 命令口径一致：验证码文本可能被用户视为敏感内容，只记长度
    logger.info("[step:%s] [ocr] 识别完成，结果长度=%d", step.id or "", len(text or ""))

    if step.store_as:
        context.results[step.store_as] = text
    if step.target_selector:
        remaining_ms = _remaining_ms(deadline)
        if remaining_ms <= 0:
            raise WorkerError(Outcome.SELECTOR_FAILED, "OCR 步骤在回填识别结果前已超时")
        await _safe_op(
            _locator(context, step.target_selector).fill(text, timeout=remaining_ms),
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


async def run_step_async(
    page: Any,
    raw_step: StepConfig,
    context: StepContext,
    step_index: int | None = None,
    total_steps: int | None = None,
) -> None:
    """在统一截止时间内执行单个步骤，并在步骤边界接管新页面。"""
    step = _resolve(raw_step, context)
    handler = _STEP_HANDLERS.get(step.step_type)
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
    prev_frame_scope = context.frame_scope
    context.frame = step.frame or None
    context.frame_scope = None
    if step.timeout:
        timeout_ms = int(step.timeout)
    elif step.step_type in {"navigate", "goto", "wait_url"}:
        timeout_ms = int(context.navigation_timeout)
    elif step.step_type in {"wait", "sleep"} and not step.selector:
        # 固定休眠的 duration 本身就是业务语义，默认步骤超时不能把合法长等待截短；
        # 用户显式配置 timeout 时仍以显式值为准。
        timeout_ms = max(int(context.default_timeout), int(step.duration) + 1000)
    else:
        timeout_ms = int(context.default_timeout)
    timeout_ms = max(1, timeout_ms)
    timeout_outcome = {
        "navigate": Outcome.NAVIGATION_TIMEOUT,
        "goto": Outcome.NAVIGATION_TIMEOUT,
        "wait_url": Outcome.NAVIGATION_TIMEOUT,
        "assert_text": Outcome.ASSERTION_FAILED,
        "evaluate": Outcome.UNKNOWN_ERROR,
        "eval": Outcome.UNKNOWN_ERROR,
        "custom_js": Outcome.UNKNOWN_ERROR,
        "custom": Outcome.UNKNOWN_ERROR,
        "ocr": Outcome.UNKNOWN_ERROR,
    }.get(step.step_type, Outcome.SELECTOR_FAILED)

    async def _execute() -> None:
        _check_cancel(context)
        _adopt_latest_page(context)
        if context.frame:
            allow_css = step.step_type not in {
                "eval", "custom_js", "evaluate", "custom", "wait_url"
            }
            context.frame_scope = await _resolve_frame_scope(
                context,
                context.frame.strip(),
                allow_css_fallback=allow_css,
            )
        await handler(context.page, step, context)
        _adopt_latest_page(context)

    task = asyncio.create_task(_execute())
    deadline = time.monotonic() + timeout_ms / 1000
    try:
        while not task.done():
            if context.cancel_event is not None and context.cancel_event.is_set():
                task.cancel()
                await _suppress_task(task)
                raise StepCancelled()
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                task.cancel()
                await _suppress_task(task)
                raise WorkerError(
                    timeout_outcome,
                    f"步骤 {step.id or step.step_type} 执行超时（{timeout_ms}ms）",
                )
            await asyncio.wait({task}, timeout=min(0.1, remaining))
        await task
    finally:
        context.frame = prev_frame
        context.frame_scope = prev_frame_scope
