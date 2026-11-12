"""持久配置会话纯逻辑：会话状态、录制动作汇聚器与录制器 JS 构造（WorkerCore 拆分）。

「记住密码」模式：门户登录页自带「记住密码」（凭据存 cookies/localStorage），
持久化 user_data_dir 即可跨进程保留；下次打开门户页时门户自己的 JS 会把账号
密码默认填入（纯页面行为，无头可用）。因此自动化只需录制登录点击——本模块
承载与 Playwright 无关的纯逻辑（对齐 ``debug_session.py`` 的拆分纪律）：

- ``ProfileSessionState``：配置会话状态（浏览器资源由 WorkerCore 装配注入）；
- ``RecorderSink``：录制动作汇聚器（去重 / 上限 / 排序的 Python 侧权威）；
- ``build_recorder_init_script``：注入页面的录制器 JS（由 Python 常量构造，
  策略优先级表为两端共同事实源）。

命令编排（profile_session_start/stop）仍留在 WorkerCore——它们驱动真实页面 I/O。
"""

from __future__ import annotations

import json
from dataclasses import dataclass, field
from typing import Any

# ── 录制器常量（单一事实源：同时驱动 Python 侧权威收口与 JS 模板构造）──

#: selector 启发式优先级（顺序即优先级）：#id → [name=] → [data-testid] →
#: 首个其他 data-* → 可见文本（button 文案，截断转义）→ 结构路径（nth-child 链）。
#: 该元组经 JSON 注入录制器 JS 模板驱动策略循环，两端顺序由此单一事实源保证。
SELECTOR_STRATEGY_PRIORITY: tuple[str, ...] = (
    "id",
    "name",
    "data-testid",
    "data",
    "text",
    "structure",
)

#: 动作序列上限：触顶丢弃后续，stop 响应以 ``truncated: true`` 标记。
#: JS 侧同预算截断减少 binding 噪音并在触顶时回传 truncated 标记（文档计数随
#: 导航归零，全局上限与 truncated 语义由 Python 侧权威收口）。
RECORDER_MAX_ACTIONS = 50

#: 防抖/去重窗口（毫秒）：同一元素（同 type + 同 selector）窗口内重复事件只记一次。
#: JS 侧逐文档去重 + Python 侧跨文档去重，窗口常量两端同源。
RECORDER_DEDUP_MS = 500

#: 可见文本（button 文案 / select 选中值）截断长度：控制 IPC 体积与选择器确定性。
RECORDER_TEXT_MAX_CHARS = 40

#: 属性值（name / data-* / id）截断长度：门户属性值可能超长，无界进入 IPC 会撑爆
#: 1MiB NDJSON 行上限（整行丢弃 → 成功信封丢失），与 _DIALOG_MESSAGE_MAX_CHARS 同纪律。
RECORDER_ATTR_MAX_CHARS = 120

#: 结构路径向上回溯的最大层数（nth-child 链深度上限）。
RECORDER_STRUCTURE_MAX_DEPTH = 5

#: expose_binding 绑定名：页面 JS 经 ``window.<名>`` 把动作实时推给 Python。
#: 绑定挂在 BrowserContext 上（覆盖弹窗新页）且跨导航存活——页面 JS 状态会随
#: 文档销毁（登录点击后必然跳转），binding 是动作跨导航存活的唯一通道。
RECORDER_BINDING_NAME = "__campusRecordAction"

#: Python 侧防御性 selector 长度上限（JS 已截断，此处兜底畸形/恶意载荷）。
_RECORDER_SELECTOR_MAX_CHARS = 512

# 录制器 JS 模板：__TOKEN__ 由 build_recorder_init_script 用 Python 常量替换
#（不用 str.format，避免与 JS 花括号冲突）。捕获范围与防抖/上限口径：
# - change 仅捕获 <select>（ISP 选择类）；点击仅捕获 button / [type=submit] /
#   input[type=submit] / [role=button]（closest 兼容按钮内嵌图标等子元素命中）；
# - 输入类交互不录——账号密码回填由门户自己的 JS 负责（「记住密码」语义）；
# - 事件用捕获相位（第三参 true）注册，门户 JS stopPropagation 也不影响录制；
# - select 的 textContent 是全部选项拼接，作为选择器目标是垃圾，text 策略
#   仅对点击路径（button 类元素）生效。
_RECORDER_JS_TEMPLATE = r"""
(() => {
  if (window.__campusAuthRecorderInstalled) return;
  window.__campusAuthRecorderInstalled = true;
  const BINDING = window["__BINDING_NAME__"];
  const MAX_ACTIONS = __MAX_ACTIONS__;
  const DEDUP_MS = __DEDUP_MS__;
  const TEXT_MAX = __TEXT_MAX__;
  const ATTR_MAX = __ATTR_MAX__;
  const STRUCTURE_MAX_DEPTH = __STRUCTURE_DEPTH__;
  const STRATEGIES = __STRATEGIES__;

  const clip = (v, n) => {
    v = String(v == null ? "" : v);
    return v.length > n ? v.slice(0, n) : v;
  };
  const quoteAttr = (v) => clip(v, ATTR_MAX).replace(/\\/g, "\\\\").replace(/"/g, '\\"');
  const cssEscape = (v) => {
    try { return CSS.escape(v); } catch (e) { return String(v).replace(/([^\w-])/g, "\\$1"); }
  };

  const byId = (el) => {
    const id = el.id;
    return id ? { selector: "#" + cssEscape(clip(id, ATTR_MAX)), strategy: "id" } : null;
  };
  const byAttr = (attr, strategy) => (el) => {
    const v = el.getAttribute(attr);
    return v ? { selector: "[" + attr + "=\"" + quoteAttr(v) + "\"]", strategy: strategy } : null;
  };
  const byFirstData = (el) => {
    const attrs = el.attributes || [];
    for (let i = 0; i < attrs.length; i++) {
      const a = attrs[i];
      if (a.name.indexOf("data-") === 0 && a.name !== "data-testid" && a.value) {
        return { selector: "[" + a.name + "=\"" + quoteAttr(a.value) + "\"]", strategy: "data" };
      }
    }
    return null;
  };
  const byText = (el) => {
    const label = clip((el.textContent || el.value || "").trim(), TEXT_MAX);
    // text="..." 为 Playwright 引号精确匹配形态（回放侧即 Playwright，天然支持）
    return label ? { selector: "text=\"" + quoteAttr(label) + "\"", strategy: "text" } : null;
  };
  const structural = (el) => {
    const parts = [];
    let node = el;
    let depth = 0;
    while (node && node.nodeType === 1 && depth < STRUCTURE_MAX_DEPTH &&
           node.tagName !== "BODY" && node.tagName !== "HTML") {
      const parent = node.parentElement;
      let index = 1;
      if (parent) {
        const children = parent.children;
        for (let i = 0; i < children.length; i++) {
          if (children[i] === node) { index = i + 1; break; }
        }
      }
      parts.unshift(node.tagName.toLowerCase() + ":nth-child(" + index + ")");
      node = parent;
      depth++;
    }
    const prefix = node && node.tagName === "BODY" ? "body > " : "";
    return { selector: prefix + parts.join(" > "), strategy: "structure" };
  };

  // 策略表由 Python 侧 SELECTOR_STRATEGY_PRIORITY 注入驱动，顺序即优先级；
  // text 策略仅点击路径允许（select 文本无意义），structure 兜底必命中。
  const TRY = {
    "id": byId,
    "name": byAttr("name", "name"),
    "data-testid": byAttr("data-testid", "data-testid"),
    "data": byFirstData,
    "text": byText,
    "structure": structural,
  };

  const selectorFor = (el, allowText) => {
    for (let i = 0; i < STRATEGIES.length; i++) {
      const s = STRATEGIES[i];
      if (s === "text" && !allowText) continue;
      const fn = TRY[s];
      if (typeof fn !== "function") continue;
      const hit = fn(el);
      if (hit) return hit;
    }
    return structural(el);
  };

  let lastType = null;
  let lastSelector = null;
  let lastTs = 0;
  let sent = 0;
  let truncSent = false;

  const send = (type, el, value) => {
    const sel = selectorFor(el, type === "click");
    const now = Date.now();
    // 防抖/去重：同一元素（同 type + 同 selector）DEDUP_MS 内重复事件只记一次
    if (type === lastType && sel.selector === lastSelector && now - lastTs < DEDUP_MS) return;
    lastType = type;
    lastSelector = sel.selector;
    lastTs = now;
    // 本文档触顶：停止外发动作并向 Python 发 truncated 标记——本文档计数随
    // 导航归零，Python 侧权威计数可能未满（动作分散在多个文档），必须由
    // 标记补齐，否则 truncated 语义漏报 JS 侧丢弃
    if (sent >= MAX_ACTIONS) {
      if (!truncSent) {
        truncSent = true;
        if (typeof BINDING === "function") {
          try { BINDING(JSON.stringify({ type: "truncated" })); } catch (e) {}
        }
      }
      return;
    }
    sent++;
    if (typeof BINDING !== "function") return;  // 绑定未就绪的极端早期事件：丢弃
    const payload = { type: type, selector: sel.selector, strategy: sel.strategy, ts: now };
    if (type === "change") payload.value = clip(value, TEXT_MAX);
    try { BINDING(JSON.stringify(payload)); } catch (e) {}
  };

  // 捕获范围：<select> 的 change（ISP 选择类）+ 按钮/提交类点击；其他交互不录
  document.addEventListener("change", (ev) => {
    const el = ev.target;
    if (!el || el.tagName !== "SELECT") return;
    send("change", el, el.value);
  }, true);
  document.addEventListener("click", (ev) => {
    const t = ev.target;
    const el = t && t.closest
      ? t.closest("button, [type=submit], input[type=submit], [role=button]")
      : null;
    if (!el) return;
    send("click", el, undefined);
  }, true);
})();
"""


def build_recorder_init_script() -> str:
    """构造录制器 init 脚本：Python 常量注入 JS 模板（两端常量单一事实源）。"""
    script = _RECORDER_JS_TEMPLATE
    replacements = {
        "__BINDING_NAME__": RECORDER_BINDING_NAME,
        "__MAX_ACTIONS__": str(RECORDER_MAX_ACTIONS),
        "__DEDUP_MS__": str(RECORDER_DEDUP_MS),
        "__TEXT_MAX__": str(RECORDER_TEXT_MAX_CHARS),
        "__ATTR_MAX__": str(RECORDER_ATTR_MAX_CHARS),
        "__STRUCTURE_DEPTH__": str(RECORDER_STRUCTURE_MAX_DEPTH),
        "__STRATEGIES__": json.dumps(list(SELECTOR_STRATEGY_PRIORITY)),
    }
    for token, value in replacements.items():
        script = script.replace(token, value)
    return script


class RecorderSink:
    """录制动作汇聚器：JS 经 expose_binding 实时推送，Python 侧统一权威收口。

    - 去重：同一 (type, selector) 在窗口期内重复事件只记一次（相邻语义，
      锚点为最近一条**已记录**动作；跨文档仍有效）；
    - 上限：动作数达上限后丢弃后续并置 ``truncated``；JS 侧文档内触顶时回传
      ``{"type": "truncated"}`` 标记补报（文档计数随导航归零，Python 侧权威
      计数可能未满），同样置 ``truncated``；
    - 排序：``recorded_actions`` 按 ts 升序返回（稳定排序，同 ts 保持到达序）。

    add_json 为 expose_binding 回调：页面来源不可信（门户页 JS 任意内容），
    畸形载荷一律静默丢弃，绝不让异常逃逸到 Playwright 绑定层。
    """

    def __init__(
        self,
        *,
        max_actions: int = RECORDER_MAX_ACTIONS,
        dedup_ms: int = RECORDER_DEDUP_MS,
    ) -> None:
        self._max_actions = max_actions
        self._dedup_ms = dedup_ms
        self._actions: list[dict] = []
        self._truncated = False
        self._last_key: tuple[str, str] | None = None
        self._last_ts = 0

    def add_json(self, _source: Any, payload_json: str) -> None:
        """expose_binding 回调入口：解析并记录一条动作（畸形载荷静默丢弃）。"""
        try:
            payload = json.loads(payload_json)
        except (TypeError, ValueError):
            return
        if not isinstance(payload, dict):
            return
        action_type = payload.get("type")
        if action_type == "truncated":
            # JS 侧文档内触顶标记：文档计数随导航归零，Python 权威计数可能未满
            # （动作分散在多个文档），由标记补齐 truncated 语义（不算动作）
            self._truncated = True
            return
        if action_type not in ("click", "change"):
            return
        selector = payload.get("selector")
        if not isinstance(selector, str) or not selector:
            return
        ts_raw = payload.get("ts")
        ts = int(ts_raw) if isinstance(ts_raw, (int, float)) and ts_raw > 0 else 0
        action: dict = {
            "type": action_type,
            "selector": selector[:_RECORDER_SELECTOR_MAX_CHARS],
            "strategy": str(payload.get("strategy") or "unknown"),
            "ts": ts,
        }
        # value 仅对 change 捕获（select 的选中值），点击不录 value
        if action_type == "change":
            value = payload.get("value")
            action["value"] = value if isinstance(value, str) else ""
        self.add(action)

    def add(self, action: dict) -> bool:
        """记录一条规范化动作；返回是否被记录（去重/触顶丢弃返回 False）。"""
        key = (action["type"], action["selector"])
        ts = int(action.get("ts") or 0)
        if key == self._last_key and abs(ts - self._last_ts) < self._dedup_ms:
            return False
        self._last_key = key
        self._last_ts = ts
        if len(self._actions) >= self._max_actions:
            self._truncated = True
            return False
        self._actions.append(action)
        return True

    @property
    def truncated(self) -> bool:
        """是否发生过触顶丢弃（record=false 或未触顶时恒为 False）。"""
        return self._truncated

    def recorded_actions(self) -> list[dict]:
        """按 ts 升序返回已录动作（稳定排序，同 ts 保持到达顺序）。"""
        return sorted(self._actions, key=lambda a: int(a.get("ts") or 0))


@dataclass
class ProfileSessionState:
    """持久配置会话状态。

    浏览器资源（own_playwright / context / page）由 WorkerCore 在启动流程中
    装配注入；``dead`` 标记用于收尾幂等（正常 stop / 幽灵回收 / 退出兜底
    多路径并发触发时只收尾一次）。
    """

    session_id: str
    """会话标识（uuid4 hex），stop 时校验用。"""

    user_data_dir: str
    """Rust 侧给的持久目录：门户「记住密码」的 cookies/localStorage 载体，
    收尾（含幽灵回收）一律原样保留，绝不清理。"""

    portal_url: str
    """启动时打开的门户页地址（空串表示不导航）。"""

    record: bool
    """是否注入录制器（False 时不装 binding 与 init 脚本，动作恒为空）。"""

    sink: RecorderSink = field(default_factory=RecorderSink)
    """录制动作汇聚器（record=false 时无事件喂入，保持为空）。"""

    context: Any = None
    """持久 BrowserContext（即浏览器本体）。"""

    page: Any = None
    """配置会话的门户页。"""

    own_playwright: Any = None
    """配置会话独占的 Playwright 连接：与自动化会话完全隔离——自动化路径
    （close_browser / 空闲回收）会 stop 共享连接，配置会话长存续期不能被其殃及。"""

    dead: bool = False
    """收尾幂等标记：已收尾的会话不再重复关资源。"""


__all__ = [
    "ProfileSessionState",
    "RecorderSink",
    "RECORDER_ATTR_MAX_CHARS",
    "RECORDER_BINDING_NAME",
    "RECORDER_DEDUP_MS",
    "RECORDER_MAX_ACTIONS",
    "RECORDER_STRUCTURE_MAX_DEPTH",
    "RECORDER_TEXT_MAX_CHARS",
    "SELECTOR_STRATEGY_PRIORITY",
    "build_recorder_init_script",
]
