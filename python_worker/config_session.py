"""持久配置会话纯逻辑：会话状态、录制动作汇聚器与录制器 JS 构造（WorkerCore 拆分）。

「记住密码」模式：门户登录页自带「记住密码」（凭据存 cookies/localStorage），
持久化 user_data_dir 即可跨进程保留；下次打开门户页时门户自己的 JS 会把账号
密码默认填入（纯页面行为，无头可用）。自动化录制完整操作序列——点击、
select 选择、文本输入（fill）与页面导航（navigate）：凭据类输入以占位符
录制（密码框值恒为 ``{{PASSWORD}}``，**密码永不落明文**；用户名启发式命中
的框恒为 ``{{USERNAME}}``），回放侧从下发的凭据参数解析。本模块承载与
Playwright 无关的纯逻辑（对齐 ``debug_session.py`` 的拆分纪律）：

- ``ProfileSessionState``：配置会话状态（浏览器资源由 WorkerCore 装配注入）；
- ``RecorderSink``：录制动作汇聚器（去重 / 上限 / 排序 / 面板单步删除的
  Python 侧权威）；
- ``build_recorder_init_script``：注入页面的录制器 JS（由 Python 常量构造，
  策略优先级表为两端共同事实源；含录制悬浮面板——实时步骤列表 + 单步删除）。

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

#: 可见文本（button 文案 / select 选中值 / fill 字面值）截断长度：控制 IPC 体积
#: 与选择器确定性。占位符值（{{USERNAME}}/{{PASSWORD}}）远小于该上限不受影响。
RECORDER_TEXT_MAX_CHARS = 40

#: navigate 动作 value（location.href）截断长度：与 selector 上限同纪律——
#: 门户长查询串 URL 无界进入 IPC 会撑爆 1MiB NDJSON 行上限（整行丢弃 →
#: 成功信封丢失）。
RECORDER_URL_MAX_CHARS = 512

#: 录制占位符（回放侧经 ``variable_resolver`` 从凭据参数解析）：密码框值恒为
#: {{PASSWORD}}——**绝不捕获真实输入值，密码永不落明文**；用户名启发式命中的
#: 文本框值恒为 {{USERNAME}}；其余文本框录实际值字面量。
RECORDER_PLACEHOLDER_PASSWORD = "{{PASSWORD}}"
RECORDER_PLACEHOLDER_USERNAME = "{{USERNAME}}"

#: 用户名启发式关键词（大小写不敏感，注入录制器 JS 构造正则）：命中
#: autocomplete="username" 或 name/id/placeholder/aria-label 即视为用户名框，
#: 值录为 {{USERNAME}} 占位符。
RECORDER_USERNAME_HINTS: tuple[str, ...] = (
    "user", "email", "account", "mobile", "phone", "学号", "账号", "用户名",
)

#: 属性值（name / data-* / id）截断长度：门户属性值可能超长，无界进入 IPC 会撑爆
#: 1MiB NDJSON 行上限（整行丢弃 → 成功信封丢失），与 _DIALOG_MESSAGE_MAX_CHARS 同纪律。
RECORDER_ATTR_MAX_CHARS = 120

#: 结构路径向上回溯的最大层数（nth-child 链深度上限）。
RECORDER_STRUCTURE_MAX_DEPTH = 5

#: expose_binding 绑定名：页面 JS 经 ``window.<名>`` 把动作实时推给 Python。
#: 绑定挂在 BrowserContext 上（覆盖弹窗新页）且跨导航存活——页面 JS 状态会随
#: 文档销毁（登录点击后必然跳转），binding 是动作跨导航存活的唯一通道。
RECORDER_BINDING_NAME = "__campusRecordAction"

#: 面板单步删除控制事件类型（页面 → 汇聚器）：录制悬浮面板删除已上报动作时
#: 经 binding 上报 ``{"type": "__delete__", value: "<ts>", action_type,
#: selector}``，汇聚器按身份删除后经 binding 返回值回推权威全量列表。控制
#: 事件本身绝不进 recorded_actions 输出。
RECORDER_DELETE_ACTION_TYPE = "__delete__"

#: Python 侧防御性 selector 长度上限（JS 已截断，此处兜底畸形/恶意载荷）。
_RECORDER_SELECTOR_MAX_CHARS = 512

# 录制器 JS 模板：__TOKEN__ 由 build_recorder_init_script 用 Python 常量替换
#（不用 str.format，避免与 JS 花括号冲突）。捕获范围与防抖/上限口径：
# - change 仅捕获 <select>（ISP 选择类）；点击仅捕获 button / [type=submit] /
#   input[type=submit] / [role=button]（closest 兼容按钮内嵌图标等子元素命中）；
# - 文本输入录为 fill：密码框值恒替换为 {{PASSWORD}} 占位符（绝不捕获真实
#   输入值，密码永不落明文），用户名启发式命中的框恒替换为 {{USERNAME}}，
#   其余文本框录实际值字面量（空值不上报）；contenteditable 可忽略不录；
#   密码框判定覆盖 autocomplete 含 password 的自绘形态（type=text），且
#   首判结果在录制会话内对该元素锁定（防「显示密码」开关中途切换 type）；
# - 每个新文档（主框架、非 about:blank）上报一条 navigate 导航标记
#   （value=location.href，selector 恒为空串），回放侧跳过与门户地址相同的
#   首条（门户页由回放主流程先行打开）；
# - 事件用捕获相位（第三参 true）注册，门户 JS stopPropagation 也不影响录制；
# - select 的 textContent 是全部选项拼接，作为选择器目标是垃圾，text 策略
#   仅对点击路径（button 类元素）生效；
# - 悬浮面板（仅主框架渲染）：右下角可折叠实时步骤列表 + 逐条删除——
#   未上报（防抖待结算）的 fill 直接清页面缓冲，已上报经 __delete__ 控制
#   事件让 Python 汇聚器按身份删除并经 binding 返回值回推权威全量列表；
#   面板根全部事件 stopPropagation 拦门户冒泡监听，录制器各捕获监听另含
#   面板内判断（document 捕获先于面板根触发，仅 stopPropagation 拦不住
#   录制器自身——面板按钮绝不能被录成动作）。
_RECORDER_JS_TEMPLATE = r"""
(() => {
  if (window.__campusAuthRecorderInstalled) return;
  window.__campusAuthRecorderInstalled = true;
  const BINDING = window["__BINDING_NAME__"];
  const DELETE_TYPE = "__DELETE_TYPE__";
  const MAX_ACTIONS = __MAX_ACTIONS__;
  const DEDUP_MS = __DEDUP_MS__;
  const TEXT_MAX = __TEXT_MAX__;
  const ATTR_MAX = __ATTR_MAX__;
  const STRUCTURE_MAX_DEPTH = __STRUCTURE_DEPTH__;
  const URL_MAX = __URL_MAX__;
  const STRATEGIES = __STRATEGIES__;
  const PASSWORD_VALUE = "__FILL_PASSWORD__";
  const USERNAME_VALUE = "__FILL_USERNAME__";
  const USERNAME_HINTS = __USERNAME_HINTS__;

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

  // ── 文本输入捕获（fill）──
  // 密码框值恒为占位符（绝不捕获真实输入值，密码永不落明文）——判定含
  // autocomplete 形态并在会话内锁定首判结果；用户名启发式命中的框恒为
  // {{USERNAME}}（凭据不在页面明文里留痕）；其余文本框录实际值。
  const TEXT_TYPES = ["text", "password", "tel", "email", "number", "search", "url"];
  const usernameRe = new RegExp("(" + USERNAME_HINTS.join("|") + ")", "i");
  const isTextField = (el) => {
    if (!el || el.nodeType !== 1) return false;
    if (el.tagName === "TEXTAREA") return true;
    if (el.tagName !== "INPUT") return false;
    // 无 type 属性默认 text；checkbox/radio/file/hidden 等非文本类不录
    const t = (el.getAttribute("type") || "text").toLowerCase();
    return TEXT_TYPES.indexOf(t) !== -1;
  };
  const isUsernameField = (el) => {
    if ((el.getAttribute("autocomplete") || "").toLowerCase() === "username") return true;
    // 近似上下文：name/id/placeholder/aria-label 命中账号类关键词即视为用户名框
    const attrs = [el.name, el.id, el.getAttribute("placeholder"), el.getAttribute("aria-label")];
    for (let i = 0; i < attrs.length; i++) {
      if (attrs[i] && usernameRe.test(String(attrs[i]))) return true;
    }
    return false;
  };
  // 密码框判定扩展到 type 之外：自绘密码框常为 type=text +
  // autocomplete="current-password"/"new-password"（无 type=password 可依赖）。
  // autocomplete 值含 password 一律按密码框对待（大小写不敏感、子串语义——
  // token 前后缀变体全覆盖）：宁可多录一个占位符，也不让真密码以字面量落盘
  const isPasswordField = (el) => {
    if ((el.getAttribute("type") || "").toLowerCase() === "password") return true;
    return (el.getAttribute("autocomplete") || "").toLowerCase().indexOf("password") !== -1;
  };
  // 首判锁定（WeakMap，录制会话内对该元素恒定）：防「显示密码」开关中途切换
  // ——首判为密码框的元素，type 被切成 text 后仍保持占位符，真值不再进入录制。
  // 反向迟到升级：首判为字面量的元素中途切成密码形态（type/autocomplete 被
  // 门户 JS 改写）时按当前形态升为占位符——同一输入会话取最终值时，早前字面量
  // 已被占位符顶替（Python 侧同目标 fill 合并取最终值）
  const fillKindCache = new WeakMap();
  const classifyFill = (el) => {
    let kind = fillKindCache.get(el);
    if (kind === undefined) {
      kind = isPasswordField(el) ? "password" : (isUsernameField(el) ? "username" : "literal");
      fillKindCache.set(el, kind);
    }
    if (kind === "literal" && isPasswordField(el)) {
      kind = "password";
      fillKindCache.set(el, kind);
    }
    return kind;
  };
  const fillValueFor = (el) => {
    const kind = classifyFill(el);
    if (kind === "password") return PASSWORD_VALUE;
    if (kind === "username") return USERNAME_VALUE;
    return el.value || "";
  };

  // 输入会话防抖：input 事件只更新待结算值（DEDUP_MS 空闲窗口内取最终值，
  // 不逐键外发），blur/change/回车提交/点击提交按钮时结算
  let pendingFill = null;
  const flushFill = () => {
    if (!pendingFill) return;
    clearTimeout(pendingFill.timer);
    const p = pendingFill;
    pendingFill = null;
    // 面板 pending 条目出列：结算后由 send 以最终值重新入列（权威 selector）
    pageSteps = pageSteps.filter((s) => !s.pending);
    // 空字面值不上报（清空后未再输入的场景交还门户回填语义）；占位符值恒非空
    if (p.value !== "") send("fill", p.el, p.value);
  };

  let lastType = null;
  let lastSelector = null;
  let lastTs = 0;
  let sent = 0;
  let truncSent = false;

  const send = (type, el, value) => {
    const sel = selectorFor(el, type === "click");
    const now = Date.now();
    // 防抖/去重：同一元素（同 type + 同 selector）DEDUP_MS 内重复事件只记一次；
    // fill 不走窗口丢弃——每次结算携带该输入会话最终值，值不同不可丢，
    // 连续 fill 由 Python 侧合并取最终值
    if (type !== "fill" && type === lastType && sel.selector === lastSelector && now - lastTs < DEDUP_MS) return;
    lastType = type;
    lastSelector = sel.selector;
    lastTs = now;
    // 本文档触顶：停止外发动作并向 Python 发 truncated 标记——本文档计数随
    // 导航归零，Python 侧权威计数可能未满（动作分散在多个文档），必须由
    // 标记补齐，否则 truncated 语义漏报 JS 侧丢弃
    if (sent >= MAX_ACTIONS) {
      if (!truncSent) {
        truncSent = true;
        // 经 report 上报：回包（权威全量列表）顺带对齐面板显示
        report({ type: "truncated" });
      }
      return;
    }
    sent++;
    if (typeof BINDING !== "function") return;  // 绑定未就绪的极端早期事件：丢弃
    const payload = { type: type, selector: sel.selector, strategy: sel.strategy, ts: now };
    if (type === "change" || type === "fill") payload.value = clip(value, TEXT_MAX);
    // 入列即镜像（同一缓冲）：乐观渲染先行保证即时可见，汇聚器回包（权威
    // 全量列表）后整体收敛——Python 侧去重/合并可能不采纳本条
    const mirror = { type: type, selector: sel.selector, ts: now };
    if (type === "change" || type === "fill") mirror.value = payload.value;
    pageSteps.push(mirror);
    renderPanel();
    report(payload);
  };

  // ── 录制悬浮面板（实时步骤列表 + 单步删除，仅主框架渲染）──
  // 形态：右下角固定、可折叠（默认展开；折叠态仅存本页内存，不跨页传递——
  // 门户登录必然跳页，每页默认展开让用户始终看得见实时列表，跨页传递需写
  // 门户域 storage 污染用户数据，成本高于收益）、内联样式全套（页面 CSS
  // 隔离）、最高层级、宽 ≤280px、列表高上限滚动。
  // 实时性取舍：页面 JS 无法主动拉取汇聚器状态，expose_binding 的返回值是
  // 唯一回程通道（binding 回调即 Python 汇聚器本体）——汇聚器处理每条上报/
  // 删除后把权威全量列表（≤ MAX_ACTIONS 条，人力节奏下体积可忽略）经返回值
  // 回推，面板显示跨页全量列表 + 头部「共 N 步」权威计数；binding 失联时
  // 退回本页乐观镜像（此时上报本身也失联，仅可见性降级）。
  let serverSteps = null;  // 权威全量列表（binding 回包后取代本地镜像）
  let pageSteps = [];      // 本页已外发动作乐观镜像（含未上报的 pending fill）
  let panelRoot = null;
  let panelListBox = null;
  let panelCountEl = null;
  let collapsed = false;
  const TYPE_LABELS = { click: "点击", change: "选择", fill: "输入", navigate: "跳转" };
  const PANEL_ID = "__campusRecordPanel";
  const SUMMARY_MAX = 96;
  // 面板自防判断：事件源在面板根内（含根自身）即为面板交互
  const inPanel = (t) => !!(panelRoot && t && panelRoot.contains(t));
  // 动作身份 = (type, ts, selector)：面板条目与汇聚器列表的精确对应关系
  // （同毫秒多动作靠 type/selector 区分，删除寻址不误伤）
  const sameIdentity = (a, b) =>
    !!a && !!b && a.type === b.type && a.ts === b.ts && a.selector === b.selector;
  const summarize = (s) => {
    if (s.type === "navigate") return String(s.value || "");
    if (s.type === "fill") return String(s.value || "") + " @ " + String(s.selector || "");
    if (s.type === "change") return String(s.selector || "") + " = " + String(s.value || "");
    return String(s.selector || "");
  };
  const renderPanel = () => {
    if (!panelListBox) return;
    // 显示源：权威全量列表 ⊕ 本页乐观镜像中权威尚未收录的条目（回包延迟/
    // 失联时仍可见，身份对上即被权威列表取代）⊕ 本页未上报的 pending fill
    const settled = serverSteps || [];
    const extra = pageSteps.filter(
      (s) => !s.pending && !settled.some((t) => sameIdentity(t, s)));
    const items = settled.concat(extra).concat(pageSteps.filter((s) => s.pending));
    panelListBox.textContent = "";
    if (!items.length) {
      const empty = document.createElement("div");
      empty.textContent = "暂无动作";
      empty.style.cssText = "padding:6px 10px;color:#9aa3b5;";
      panelListBox.appendChild(empty);
    }
    for (let i = 0; i < items.length; i++) {
      const s = items[i];
      const row = document.createElement("div");
      row.style.cssText = "display:flex;align-items:center;gap:6px;padding:4px 10px;border-top:1px solid rgba(255,255,255,0.08);";
      const idx = document.createElement("span");
      idx.textContent = String(i + 1);
      idx.style.cssText = "min-width:16px;text-align:right;color:#9aa3b5;flex:none;";
      const badge = document.createElement("span");
      badge.textContent = s.pending ? "输入中" : (TYPE_LABELS[s.type] || s.type);
      const badgeColor = { click: "#5b9cf6", change: "#4fc08d", fill: "#e6a23c", navigate: "#b07ef0" }[s.type] || "#9aa3b5";
      badge.style.cssText = "flex:none;padding:0 5px;border-radius:8px;font-size:11px;line-height:16px;color:#11141a;background:" + badgeColor + ";";
      let text = s.pending
        ? String(s.value || "") + " @ " + String(s.selector || "")
        : summarize(s);
      if (text.length > SUMMARY_MAX) text = text.slice(0, SUMMARY_MAX) + "…";
      const summary = document.createElement("span");
      summary.textContent = text;
      summary.title = text;  // 悬停看完整摘要（截断仅是显示宽度限制）
      summary.style.cssText = "flex:1;overflow:hidden;text-overflow:ellipsis;white-space:nowrap;color:#d5dae4;";
      const del = document.createElement("button");
      del.type = "button";
      del.setAttribute("aria-label", "删除第" + (i + 1) + "步");
      del.textContent = "×";
      del.style.cssText = "flex:none;background:none;border:none;color:#9aa3b5;font:14px/1 system-ui,sans-serif;cursor:pointer;padding:0 2px;";
      del.addEventListener("click", (ev) => {
        ev.stopPropagation();
        deleteStep(s);
      });
      row.appendChild(idx);
      row.appendChild(badge);
      row.appendChild(summary);
      row.appendChild(del);
      panelListBox.appendChild(row);
    }
    if (panelCountEl) panelCountEl.textContent = "共 " + items.length + " 步";
  };
  const buildPanel = () => {
    // iframe 不渲染：init 脚本会注入所有 frame（iframe 里的门户表单动作仍要
    // 录制），面板收口在主框架一份；删除按动作身份寻址，与来源 frame 无关
    if (window.top !== window) return;
    if (document.getElementById(PANEL_ID)) return;  // 幂等守卫
    const root = document.createElement("div");
    root.id = PANEL_ID;
    root.style.cssText = "position:fixed;right:12px;bottom:12px;width:264px;max-width:280px;z-index:2147483647;background:rgba(17,20,26,0.88);color:#e8ebf2;border-radius:10px;box-shadow:0 6px 24px rgba(0,0,0,0.4);font:12px/1.4 system-ui,-apple-system,'Segoe UI',sans-serif;overflow:hidden;";
    // 面板自防（拦冒泡）：面板内所有事件不再冒泡出根节点，门户 JS 的监听
    // 看不到面板交互。注意 document 捕获监听先于面板根触发，这里拦不住
    // 录制器自身——录制器 click/fill/change/keydown/blur 捕获监听另含
    // inPanel 判断，两层防线确保面板按钮绝不被录成动作。
    const mutedEvents = ["click", "mousedown", "mouseup", "dblclick", "mousemove", "mouseover", "mouseout", "pointerdown", "pointerup", "input", "change", "keydown", "keyup", "keypress", "submit", "contextmenu", "wheel", "touchstart", "touchend", "focusin", "focusout"];
    for (let i = 0; i < mutedEvents.length; i++) {
      root.addEventListener(mutedEvents[i], (ev) => ev.stopPropagation());
    }
    const head = document.createElement("div");
    head.style.cssText = "display:flex;align-items:center;gap:8px;padding:8px 10px;";
    const title = document.createElement("span");
    title.textContent = "录制面板";
    title.style.cssText = "font-weight:600;letter-spacing:0.5px;flex:none;";
    panelCountEl = document.createElement("span");
    panelCountEl.textContent = "共 0 步";
    panelCountEl.style.cssText = "color:#9aa3b5;flex:none;";
    const toggle = document.createElement("button");
    toggle.type = "button";
    toggle.setAttribute("aria-label", "收起录制面板");
    toggle.textContent = "▾";
    toggle.style.cssText = "margin-left:auto;flex:none;background:none;border:none;color:#9aa3b5;font:12px/1 system-ui,sans-serif;cursor:pointer;padding:2px 4px;";
    toggle.addEventListener("click", (ev) => {
      ev.stopPropagation();
      collapsed = !collapsed;
      toggle.textContent = collapsed ? "▸" : "▾";
      toggle.setAttribute("aria-label", collapsed ? "展开录制面板" : "收起录制面板");
      if (panelListBox) panelListBox.style.display = collapsed ? "none" : "";
    });
    head.appendChild(title);
    head.appendChild(panelCountEl);
    head.appendChild(toggle);
    panelListBox = document.createElement("div");
    panelListBox.style.cssText = "max-height:200px;overflow-y:auto;padding:0 0 4px;";
    root.appendChild(head);
    root.appendChild(panelListBox);
    // init 脚本先于 body 挂载执行：body 未就绪时挂 documentElement 兜底
    (document.body || document.documentElement).appendChild(root);
    panelRoot = root;
    renderPanel();
  };
  // binding 回包收敛：权威列表整体取代显示源，本地镜像只留 pending fill
  const renderFromServer = (list) => {
    serverSteps = Array.isArray(list) ? list : null;
    if (serverSteps) pageSteps = pageSteps.filter((s) => s.pending);
    renderPanel();
  };
  // 上报并同步面板：binding 回调即汇聚器本体，返回值为权威全量列表，回包后
  // 整体收敛；乐观本地渲染先行，保证点击即时可见
  const report = (payload) => {
    if (typeof BINDING !== "function") return;
    let json;
    try { json = JSON.stringify(payload); } catch (e) { return; }
    try {
      const ret = BINDING(json);
      if (ret && typeof ret.then === "function") {
        ret.then((v) => { if (Array.isArray(v)) renderFromServer(v); }).catch(() => {});
      }
    } catch (e) {}
  };
  const deleteStep = (entry) => {
    if (entry.pending) {
      // 未上报（输入会话防抖待结算）：直接清页面缓冲，无需控制事件
      if (pendingFill) { clearTimeout(pendingFill.timer); pendingFill = null; }
      pageSteps = pageSteps.filter((s) => s !== entry);
      renderPanel();
      return;
    }
    // 已上报：乐观移除先行（点击即时可见），控制事件让汇聚器按身份删除，
    // 回包（权威全量列表）后整体收敛
    pageSteps = pageSteps.filter((s) => s.pending || !sameIdentity(s, entry));
    if (serverSteps) serverSteps = serverSteps.filter((s) => !sameIdentity(s, entry));
    renderPanel();
    report({
      type: DELETE_TYPE,
      value: String(entry.ts),
      action_type: entry.type,
      selector: entry.selector,
    });
  };

  // 捕获范围：<select> 的 change（ISP 选择类）+ 按钮/提交类点击 + 文本输入
  document.addEventListener("change", (ev) => {
    const el = ev.target;
    if (!el) return;
    if (inPanel(el)) return;  // 面板自防：面板内目标绝不录入
    if (el.tagName === "SELECT") {
      send("change", el, el.value);
      return;
    }
    // 文本框 change（值变过的框失焦时触发）是输入结算点之一：blur 已先行
    // 结算，此处幂等兜底
    if (pendingFill && pendingFill.el === el) flushFill();
  }, true);
  document.addEventListener("input", (ev) => {
    const el = ev.target;
    if (inPanel(el)) return;  // 面板自防：fill 管道忽略面板内目标
    if (!isTextField(el)) return;
    const value = fillValueFor(el);
    if (pendingFill && pendingFill.el === el) {
      // 同一输入会话：只更新最终值并顺延空闲窗口（DEDUP_MS 内取最终值）
      pendingFill.value = value;
      if (pendingFill.panelEntry) pendingFill.panelEntry.value = value;
      clearTimeout(pendingFill.timer);
      pendingFill.timer = setTimeout(flushFill, DEDUP_MS);
      return;
    }
    flushFill();  // 换目标：先结算上一个输入会话
    // 待结算 fill 同步入面板（未上报态）：防抖窗口内可当场删除——直接清
    // 页面缓冲，无需控制事件（尚未上报给汇聚器）
    const panelEntry = {
      pending: true, type: "fill",
      selector: selectorFor(el, false).selector, value: value, ts: 0,
    };
    pendingFill = {
      el: el, value: value, timer: setTimeout(flushFill, DEDUP_MS),
      panelEntry: panelEntry,
    };
    pageSteps.push(panelEntry);
    renderPanel();
  }, true);
  document.addEventListener("blur", (ev) => {
    if (inPanel(ev.target)) return;  // 面板自防（blur 不冒泡，防御性统一拦截）
    if (pendingFill && pendingFill.el === ev.target) flushFill();
  }, true);
  document.addEventListener("keydown", (ev) => {
    // 回车是门户主流提交方式：表单提交后文档随导航销毁，blur/空闲定时器都
    // 来不及结算，必须在导航开始前把待结算 fill 推出去；面板内按键（如键盘
    // 激活删除按钮）不是门户表单提交，不结算
    if (inPanel(ev.target)) return;
    if (pendingFill && ev.key === "Enter") flushFill();
  }, true);
  document.addEventListener("click", (ev) => {
    // 面板自防：面板内点击（含删除按钮）绝不录入，也不结算输入会话
    if (inPanel(ev.target)) return;
    const t = ev.target;
    const el = t && t.closest
      ? t.closest("button, [type=submit], input[type=submit], [role=button]")
      : null;
    if (!el) return;
    // 记录点击前先结算进行中的输入会话：登录点击（含回车在表单上合成的
    // 提交点击）前，待结算 fill 必须先落序，保持「输入 → 提交」时序
    flushFill();
    send("click", el, undefined);
  }, true);

  // ── 面板装配（主框架）：先于任何用户交互，但 init script 执行时点早于文档
  // 元素创建（body 与 documentElement 均可能为 null，appendChild 直接抛错会
  // 打断整个脚本——navigate 上报块在本调用之后，实测因此全部丢失）。挂载推迟
  // 到 DOMContentLoaded（loading 态）或立即（解析完成态）执行 ──
  if (document.readyState === "loading") {
    document.addEventListener("DOMContentLoaded", buildPanel, { once: true });
  } else {
    buildPanel();
  }

  // 导航标记：主框架的每个新文档上报一条 navigate（value=落地地址）。iframe
  // 与 about:blank（new_page 的初始文档，非用户导航）不上报；selector 恒为
  // 空串——Rust RecordedAction 契约 type/selector/strategy/ts 恒存在、value
  // 可选，navigate 的回放本体是 value；回放侧跳过与门户地址相同的首条
  if (window.top === window && location.href !== "about:blank") {
    if (sent < MAX_ACTIONS) {
      sent++;
      const navTs = Date.now();
      const payload = {
        type: "navigate",
        selector: "",
        strategy: "structure",
        ts: navTs,
        value: clip(location.href, URL_MAX),
      };
      pageSteps.push({ type: "navigate", selector: "", ts: navTs, value: payload.value });
      renderPanel();
      // 首条 navigate 的回包即权威全量列表（含此前页面的累计），面板由此
      // 从本页镜像切换到跨页全量显示。
      // binding 就绪重试：expose_binding 的注入脚本与本 init script 的文档内
      // 执行顺序不受保证（实测 init 阶段 binding 可能尚未定义，report 的
      // typeof 守卫会静默丢弃上报——实测全部 navigate 丢失）；交互期上报
      // （fill/click 等）到达时 binding 早已就绪不受影响。短轮询等 binding
      // 出现后再报，40 次 ×50ms ≈2s 足够覆盖注入时差，仍失败则放弃（会话
      // 即将关闭或环境异常，静默降级与既有口径一致）
      const navReport = (tries) => {
        if (typeof BINDING === "function") {
          report(payload);
          return;
        }
        if (tries > 0) setTimeout(() => navReport(tries - 1), 50);
      };
      navReport(40);
    }
  }
})();
"""


def build_recorder_init_script() -> str:
    """构造录制器 init 脚本：Python 常量注入 JS 模板（两端常量单一事实源）。"""
    script = _RECORDER_JS_TEMPLATE
    replacements = {
        "__BINDING_NAME__": RECORDER_BINDING_NAME,
        "__DELETE_TYPE__": RECORDER_DELETE_ACTION_TYPE,
        "__MAX_ACTIONS__": str(RECORDER_MAX_ACTIONS),
        "__DEDUP_MS__": str(RECORDER_DEDUP_MS),
        "__TEXT_MAX__": str(RECORDER_TEXT_MAX_CHARS),
        "__ATTR_MAX__": str(RECORDER_ATTR_MAX_CHARS),
        "__STRUCTURE_DEPTH__": str(RECORDER_STRUCTURE_MAX_DEPTH),
        "__URL_MAX__": str(RECORDER_URL_MAX_CHARS),
        "__STRATEGIES__": json.dumps(list(SELECTOR_STRATEGY_PRIORITY)),
        "__FILL_PASSWORD__": RECORDER_PLACEHOLDER_PASSWORD,
        "__FILL_USERNAME__": RECORDER_PLACEHOLDER_USERNAME,
        "__USERNAME_HINTS__": json.dumps(list(RECORDER_USERNAME_HINTS)),
    }
    for token, value in replacements.items():
        script = script.replace(token, value)
    return script


class RecorderSink:
    """录制动作汇聚器：JS 经 expose_binding 实时推送，Python 侧统一权威收口。

    - 去重：click / change 同一 (type, selector) 在窗口期内重复事件只记一次
      （相邻语义，锚点为最近一条**已记录**动作；跨文档仍有效）；fill /
      navigate 与最近一条已记录动作同 (type, selector) 时**就地合并取最终值**
      （fill 取输入会话最终值——值不同不可按窗口丢弃；navigate 取重定向链
      最终落地页），不受窗口约束；
    - 上限：动作数达上限后丢弃后续并置 ``truncated``；JS 侧文档内触顶时回传
      ``{"type": "truncated"}`` 标记补报（文档计数随导航归零，Python 侧权威
      计数可能未满），同样置 ``truncated``（合并替换不增计数，触顶下仍放行）；
    - 排序：``recorded_actions`` 按 ts 升序返回（稳定排序，同 ts 保持到达序）；
    - 单步删除（录制悬浮面板）：``__delete__`` 控制事件按身份（ts + 可选
      action_type/selector）删除已累计动作并保持原序；``__delete__`` 本身不进
      ``recorded_actions`` 输出；``truncated`` 不因删除复位（发生过触顶丢弃的
      事实不可逆），删除后计数下降、容量自然重开；去重/合并锚点重算为当前
      最近一条已记录动作（既有去重/合并口径不受影响）。

    add_json 为 expose_binding 回调：页面来源不可信（门户页 JS 任意内容），
    畸形载荷一律静默丢弃，绝不让异常逃逸到 Playwright 绑定层。返回值为处理
    后的权威动作全量列表——经 Playwright 序列化回推页面，录制悬浮面板据此
    显示跨页全量列表与「共 N 步」权威计数（页面 JS 无法主动拉取汇聚器状态，
    binding 返回值是唯一回程通道），也承载单步删除后的即时收敛。
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

    def add_json(self, _source: Any, payload_json: str) -> list[dict]:
        """expose_binding 回调入口：处理一条上报并返回权威动作全量列表。"""
        try:
            payload = json.loads(payload_json)
        except (TypeError, ValueError):
            return self.recorded_actions()
        if not isinstance(payload, dict):
            return self.recorded_actions()
        action_type = payload.get("type")
        if action_type == "truncated":
            # JS 侧文档内触顶标记：文档计数随导航归零，Python 权威计数可能未满
            # （动作分散在多个文档），由标记补齐 truncated 语义（不算动作）
            self._truncated = True
            return self.recorded_actions()
        if action_type == RECORDER_DELETE_ACTION_TYPE:
            # 面板单步删除控制事件：按身份删除已累计动作，控制事件本身绝不
            # 进 recorded_actions 输出
            self._delete_recorded(payload)
            return self.recorded_actions()
        if action_type not in ("click", "change", "fill", "navigate"):
            return self.recorded_actions()
        selector = payload.get("selector")
        if action_type == "navigate":
            # 契约：navigate 无目标元素，selector 恒为空串（value=落地地址才是
            # 回放本体），缺失/非串也归一，不给伪造 selector 留口子
            selector = ""
        elif not isinstance(selector, str) or not selector:
            return self.recorded_actions()
        ts_raw = payload.get("ts")
        ts = int(ts_raw) if isinstance(ts_raw, (int, float)) and ts_raw > 0 else 0
        action: dict = {
            "type": action_type,
            "selector": selector[:_RECORDER_SELECTOR_MAX_CHARS],
            "strategy": str(payload.get("strategy") or "unknown"),
            "ts": ts,
        }
        # value 捕获：change=select 选中值、fill=占位符或字面值（JS 已截断，
        # 此处兜底截断防畸形/恶意载荷撑爆 IPC）、navigate=落地地址（缺失视为
        # 畸形丢弃——无 URL 的 navigate 无回放意义）；点击不录 value
        if action_type in ("change", "fill"):
            value = payload.get("value")
            action["value"] = (value if isinstance(value, str) else "")[:RECORDER_TEXT_MAX_CHARS]
        elif action_type == "navigate":
            value = payload.get("value")
            if not isinstance(value, str) or not value:
                return self.recorded_actions()
            action["value"] = value[:RECORDER_URL_MAX_CHARS]
        self.add(action)
        return self.recorded_actions()

    def _delete_recorded(self, payload: dict) -> bool:
        """按身份删除一条已累计动作（面板单步删除），返回是否删除。

        匹配口径：``value``（动作 ts）必需；``action_type`` / ``selector`` 由
        面板一并携带作精确定位（同毫秒多动作不误删），缺失时退化为仅按 ts
        匹配（宽容兼容畸形/旧载荷）。删除后其余动作保持原序（不重排，ts 升序
        语义由 recorded_actions 保证），去重/合并锚点重算为当前最近一条已记录
        动作（防止删除末条后窗口期新事件被误丢）；``truncated`` 不复位——它
        记录的是「发生过触顶丢弃」，删除动作救不回已丢弃的动作，输出不完整的
        事实不变。
        """
        try:
            ts = int(payload.get("value"))
        except (TypeError, ValueError):
            return False
        want_type = payload.get("action_type")
        want_selector = payload.get("selector")
        for i, action in enumerate(self._actions):
            if int(action.get("ts") or 0) != ts:
                continue
            if want_type is not None and action["type"] != want_type:
                continue
            if want_selector is not None and action["selector"] != want_selector:
                continue
            del self._actions[i]
            if self._actions:
                last = self._actions[-1]
                self._last_key = (last["type"], last["selector"])
                self._last_ts = int(last.get("ts") or 0)
            else:
                self._last_key = None
                self._last_ts = 0
            return True
        return False

    def add(self, action: dict) -> bool:
        """记录一条规范化动作；返回是否被记录（去重/触顶丢弃返回 False）。

        fill / navigate 与最近一条**已记录**动作同 (type, selector) 时就地
        替换（合并取最终值/最终落地页），替换不增计数、触顶下仍放行。
        """
        key = (action["type"], action["selector"])
        ts = int(action.get("ts") or 0)
        # 合并语义只对携带载荷的 fill/navigate 生效：窗口丢弃会让「同一目标的
        # 后一个值」顶不掉「前一个值」（回放必须取最终值）；重定向链多条
        # navigate 合并为最终落地页
        if action["type"] in ("fill", "navigate") and self._actions:
            last = self._actions[-1]
            if (last["type"], last["selector"]) == key:
                self._actions[-1] = action
                self._last_key = key
                self._last_ts = ts
                return True
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
    "RECORDER_DELETE_ACTION_TYPE",
    "RECORDER_DEDUP_MS",
    "RECORDER_MAX_ACTIONS",
    "RECORDER_PLACEHOLDER_PASSWORD",
    "RECORDER_PLACEHOLDER_USERNAME",
    "RECORDER_STRUCTURE_MAX_DEPTH",
    "RECORDER_TEXT_MAX_CHARS",
    "RECORDER_URL_MAX_CHARS",
    "RECORDER_USERNAME_HINTS",
    "SELECTOR_STRATEGY_PRIORITY",
    "build_recorder_init_script",
]
