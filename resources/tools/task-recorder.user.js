// ==UserScript==
// @name         Campus-Auth 任务录制器
// @namespace    https://github.com/Misyra/Campus-Auth
// @version      4.3.0
// @description  可视化选取校园网登录页面元素，自动生成任务 JSON 或结构化文档
// @author       Misyra
// @match        http://*/*
// @match        https://*/*
// @grant        GM_setClipboard
// @grant        GM_setValue
// @grant        GM_getValue
// @grant        GM_deleteValue
// @grant        GM_addStyle
// @run-at       document-idle
// ==/UserScript==

(function () {
  "use strict";

  // ==================== 配置 ====================

  const VERSION = "4.3.0"; // 同步修改顶部 @version

  const STEP_TYPES = {
    username: { category: "basic", label: "账号输入框", icon: "👤", color: "#4CAF50", primary: true, hint: "点击页面上真实的账号输入框（不是旁边的文字标签），支持自动检测隐藏输入框" },
    password: { category: "basic", label: "密码输入框", icon: "🔒", color: "#2196F3", primary: true, hint: "点击密码输入框，录制器会自动检测 display:none 的隐藏密码框" },
    carrier: { category: "basic", label: "运营商选择", icon: "📶", color: "#FF9800", primary: true, hint: "点击运营商下拉框：原生 select 一键完成；自定义 div 自动进入两阶段选选项" },
    captcha_img: { category: "basic", label: "验证码图片", icon: "🖼️", color: "#9C27B0", primary: true, hint: "点击验证码图片，录制器会自动提示继续点击验证码输入框" },
    captcha_input: { category: "basic", label: "验证码输入框", icon: "✏️", color: "#9C27B0", primary: false, hint: "点击验证码输入框，自动弹出验证码类型选择（数字/字母/运算等）" },
    submit: { category: "basic", label: "提交按钮", icon: "🚀", color: "#F44336", primary: true, hint: "点击登录/提交按钮，通常放在最后一步" },
    checkbox: { category: "basic", label: "勾选/协议", icon: "☑️", color: "#FF5722", primary: true, hint: "点击复选框、用户协议勾选框，自动录制勾选操作" },
    smart_detect: { category: "basic", label: "智能检测", icon: "🔍", color: "#00BCD4", primary: true, hint: "打字自动识别账号/密码，点击自动识别勾选/提交/下拉框，按 Esc 停止" },
    click: { category: "advanced", label: "点击元素", icon: "👆", color: "#607D8B", primary: false, hint: "点击任意页面元素，仅记录点击操作，不填空" },
    wait: { category: "advanced", label: "等待元素", icon: "⏳", color: "#795548", primary: false, hint: "鼠标悬停在要等待的元素上，然后按 Enter 键记录" },
    eval: { category: "advanced", label: "执行JS", icon: "⚙️", color: "#00BCD4", primary: false, hint: "输入一段要在页面中执行的 JavaScript 代码" },
    custom: { category: "advanced", label: "自定义步骤", icon: "📝", color: "#9E9E9E", primary: false, hint: "手动填写步骤描述、选择器、填写值，自由度高" },
    sleep: { category: "advanced", label: "延时等待", icon: "⏳", color: "#795548", primary: false, hint: "添加一个等待步骤，页面不操作仅等待指定时间" },
    screenshot: { category: "advanced", label: "页面截图", icon: "📸", color: "#607D8B", primary: false, hint: "截取当前页面状态，用于调试" },
    wait_url: { category: "advanced", label: "等待URL", icon: "🔗", color: "#795548", primary: false, hint: "等待浏览器 URL 匹配指定正则表达式" },
  };

  const CAPTCHA_TYPES = [
    { value: "4digit", label: "4位纯数字", charRange: "纯数字" },
    { value: "4char", label: "4位字母+数字", charRange: "字母和数字" },
    { value: "math", label: "数学运算 (如 1+2=?)", charRange: "数字和 +-*/=xX÷ 运算符" },
    { value: "other", label: "其他（请描述）", charRange: "" },
  ];

  // ==================== 状态 ====================

  const state = {
    active: false,
    recording: false,
    multiStepMode: false,
    hiddenDetectionEnabled: true,
    revealEnabled: false,
    steps: [],
    hoveredEl: null,
    selectedEl: null,
    currentStepType: null,
    panel: null,
    tooltip: null,
    iframeWarning: null,
    carrierClickPhase: null,
  };

  const STORAGE_KEY = "ca_recorder_state";

  const LIMITS = {
    HTML_HIDDEN: 2000,
    HTML_ELEMENT: 3000,
    HTML_CONTAINER: 5000,
    HTML_CONTEXT: 12000,
    STATE_TTL_MS: 2 * 60 * 60 * 1000,
    DOM_GUARD_INTERVAL_MS: 8000,
    TOOLTIP_MAX_WIDTH: 420,
    POPUP_MAX_WIDTH: 300,
  };

  // ==================== 录制安全 / 选择器规范化 ====================

  const SENSITIVE_ATTR_RE = /(?:password|passwd|pwd|secret|token|authorization|credential|session|cookie|csrf|xsrf)/i;

  // CSS.escape 用于标识符，不适合直接塞进 [name="..."] 这样的 CSS 字符串字面量。
  function escapeCssString(value) {
    return String(value)
      .replace(/\\/g, "\\\\")
      .replace(/"/g, '\\"')
      .replace(/\r\n|\r|\n/g, "\\a ");
  }

  // 录制器内部候选带 type；导出给 Playwright 时必须保留选择器类型，否则纯文本会被
  // 当成 CSS 标签，XPath 也会走 CSS parser。
  function selectorForPlayback(candidate) {
    if (!candidate || !candidate.value) return "";
    if (candidate.type === "text") return `text=${JSON.stringify(candidate.value)}`;
    if (candidate.type === "xpath") return `xpath=${candidate.value}`;
    return candidate.value;
  }

  function selectorCandidatesForPlayback(info) {
    return (info?.selectors || []).map(selectorForPlayback).filter(Boolean);
  }

  // 仅用于录制器自己回查元素。Playwright selector 不能直接交给 document.querySelector。
  function queryRecordedElement(selector) {
    if (!selector) return null;
    try {
      if (selector.startsWith("xpath=")) {
        return document.evaluate(
          selector.slice(6), document, null, XPathResult.FIRST_ORDERED_NODE_TYPE, null
        ).singleNodeValue;
      }
      if (selector.startsWith("text=")) {
        let wanted = selector.slice(5);
        try { wanted = JSON.parse(wanted); } catch (_) {}
        wanted = String(wanted).trim();
        const all = document.querySelectorAll("button, a, label, span, div, option, li");
        return Array.from(all).find(el => (el.textContent || "").trim() === wanted) || null;
      }
      return document.querySelector(selector);
    } catch (_) {
      return null;
    }
  }

  function isSensitiveAttributeName(name) {
    return name === "value" || SENSITIVE_ATTR_RE.test(name || "");
  }

  // 复制给 AI 的 HTML 只保留结构，不携带用户刚输入的账号/口令/验证码或页面 token。
  function sanitizeDomHtml(el, inner, maxLen) {
    if (!el) return "";
    try {
      const clone = el.cloneNode(true);
      const nodes = [clone, ...clone.querySelectorAll("*")];
      for (const node of nodes) {
        if (!node.attributes) continue;
        for (const attr of Array.from(node.attributes)) {
          if (isSensitiveAttributeName(attr.name)) node.removeAttribute(attr.name);
        }
        const tag = (node.tagName || "").toLowerCase();
        if (tag === "textarea") node.textContent = "";
        if (tag === "input") node.removeAttribute("checked");
        if (tag === "option") node.removeAttribute("selected");
      }
      const html = inner ? clone.innerHTML : clone.outerHTML;
      return html.substring(0, maxLen);
    } catch (_) {
      return "";
    }
  }

  function saveState() {
    try {
      const slimSteps = state.steps.map(s => {
        const copy = { ...s };
        delete copy.elementHTML;
        delete copy.elementParentContext;
        delete copy.elementContainerHTML;
        delete copy.hiddenRealHTML;
        return copy;
      });
      GM_setValue(STORAGE_KEY, {
        steps: slimSteps,
        savedAt: Date.now(),
        url: window.location.href,
      });
    } catch (e) {
      console.warn("[CA Recorder] saveState 失败:", e);
    }
  }

  function loadState() {
    try {
      const data = GM_getValue(STORAGE_KEY, null);
      if (!data || !data.steps || data.steps.length === 0) return false;
      if (Date.now() - (data.savedAt || 0) > LIMITS.STATE_TTL_MS) {
        clearSavedState();
        return false;
      }
      if (data.url && data.url !== window.location.href) {
        clearSavedState();
        return false;
      }
      data.steps.forEach(function(step) {
        if (step.code !== undefined && step.script === undefined) {
          step.script = step.code;
          delete step.code;
        }
      });
      return data;
    } catch (e) {
      console.warn("[CA Recorder] loadState 失败:", e);
      return false;
    }
  }

  function clearSavedState() {
    try { GM_deleteValue(STORAGE_KEY); } catch (e) { console.warn("[CA Recorder] clearSavedState 失败:", e); }
  }

  function restoreFromSaved(saved) {
    state.steps = saved.steps;
    activate();
    updateRecordedList();
  }

  // ==================== 样式注入 ====================

  GM_addStyle(`
    #ca-recorder-panel {
      --ca-bg: #1a1a2e; --ca-card: #2a2a3e; --ca-card-hover: #333;
      --ca-card-active: #2a2a5e; --ca-text: #e0e0e0; --ca-text-dim: #aaa;
      --ca-text-muted: #888; --ca-border: #444; --ca-divider: #333;
      --ca-primary: #667eea; --ca-primary-grad: linear-gradient(135deg, #667eea 0%, #764ba2 100%);
      --ca-success: #4CAF50; --ca-danger: #e74c3c; --ca-warning: #FF9800;
      --ca-step-username: #4CAF50; --ca-step-password: #2196F3; --ca-step-carrier: #FF9800;
      --ca-step-captcha: #9C27B0; --ca-step-submit: #F44336; --ca-step-checkbox: #FF5722;
      --ca-step-detect: #00BCD4; --ca-step-click: #607D8B; --ca-step-wait: #795548;
    }
    #ca-recorder-panel {
      position: fixed; top: 10px; right: 10px; z-index: 2147483647;
      width: 360px; max-height: 90vh; overflow-y: auto;
      background: var(--ca-bg); color: var(--ca-text); border-radius: 12px;
      box-shadow: 0 8px 32px rgba(0,0,0,0.5);
      font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', sans-serif;
      font-size: 14px; line-height: 1.5;
    }
    #ca-recorder-panel * { box-sizing: border-box; }
    #ca-recorder-panel .ca-header { padding: 16px; background: var(--ca-primary-grad); border-radius: 12px 12px 0 0; cursor: move; user-select: none; }
    #ca-recorder-panel .ca-header h3 { margin: 0; font-size: 16px; color: #fff; }
    #ca-recorder-panel .ca-header small { color: rgba(255,255,255,0.7); }
    #ca-recorder-panel .ca-header-bar { display: flex; align-items: center; justify-content: space-between; }
    #ca-recorder-panel .ca-help-btn { width: 26px; height: 26px; border-radius: 50%; border: 1px solid rgba(255,255,255,0.3); background: rgba(255,255,255,0.1); color: #fff; cursor: pointer; font-size: 14px; font-weight: bold; line-height: 1; }
    #ca-recorder-panel .ca-body { padding: 12px 16px; }
    #ca-recorder-panel .ca-section { margin-bottom: 12px; }
    #ca-recorder-panel .ca-section-title { font-size: 12px; text-transform: uppercase; color: var(--ca-text-muted); letter-spacing: 1px; margin-bottom: 8px; }
    #ca-recorder-panel .ca-btn { display: inline-flex; align-items: center; gap: 6px; padding: 8px 14px; border: none; border-radius: 8px; cursor: pointer; font-size: 13px; font-weight: 500; transition: all 0.2s; }
    #ca-recorder-panel .ca-btn:hover { transform: translateY(-1px); filter: brightness(1.1); }
    #ca-recorder-panel .ca-btn-primary { background: var(--ca-primary); color: #fff; }
    #ca-recorder-panel .ca-btn-success { background: var(--ca-success); color: #fff; }
    #ca-recorder-panel .ca-btn-danger { background: var(--ca-danger); color: #fff; }
    #ca-recorder-panel .ca-btn-secondary { background: var(--ca-card); color: #ccc; }
    #ca-recorder-panel .ca-btn-sm { padding: 4px 10px; font-size: 12px; }
    #ca-recorder-panel .ca-btn-block { width: 100%; justify-content: center; }
    #ca-recorder-panel .ca-btn:disabled { opacity: 0.4; cursor: not-allowed; transform: none; }
    #ca-recorder-panel .ca-step-grid { display: grid; grid-template-columns: repeat(2, 1fr); gap: 6px; }
    #ca-recorder-panel .ca-step-btn { display: flex; align-items: center; gap: 6px; padding: 8px 10px; background: var(--ca-card); border: 2px solid transparent; border-radius: 8px; cursor: pointer; color: #ddd; font-size: 13px; transition: all 0.2s; }
    #ca-recorder-panel .ca-step-btn:hover { background: #3a3a4e; }
    #ca-recorder-panel .ca-step-btn.active { border-color: var(--ca-primary); background: var(--ca-card-active); }
    #ca-recorder-panel .ca-more-btn { border-color: #555; }
    #ca-recorder-panel .ca-more-btn:hover { border-color: var(--ca-primary); }
    #ca-recorder-panel .ca-more-container { grid-column: 1 / -1; margin-top: 2px; }
    #ca-recorder-panel .ca-step-btn .ca-icon { font-size: 16px; }
    #ca-recorder-panel .ca-grid-sep { grid-column: 1 / -1; height: 1px; background: var(--ca-divider); margin: 2px 0; }
    #ca-recorder-panel .ca-recorded-list { list-style: none; padding: 0; margin: 0; }
    #ca-recorder-panel .ca-recorded-item { display: flex; align-items: center; gap: 8px; padding: 8px 10px; margin-bottom: 4px; background: var(--ca-card); border-radius: 8px; font-size: 12px; cursor: pointer; }
    #ca-recorder-panel .ca-recorded-item:hover { background: var(--ca-card-hover); }
    #ca-recorder-panel .ca-recorded-item .ca-idx { background: var(--ca-primary); color: #fff; border-radius: 50%; width: 20px; height: 20px; display: flex; align-items: center; justify-content: center; font-size: 11px; flex-shrink: 0; }
    #ca-recorder-panel .ca-recorded-item .ca-info { flex: 1; min-width: 0; overflow: hidden; }
    #ca-recorder-panel .ca-recorded-item .ca-info .ca-label { font-weight: 600; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
    #ca-recorder-panel .ca-recorded-item .ca-info .ca-selector { color: var(--ca-text-muted); font-size: 11px; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; max-width: 200px; }
    #ca-recorder-panel .ca-recorded-item .ca-del { background: none; border: none; color: var(--ca-danger); cursor: pointer; font-size: 16px; padding: 0 4px; }
    #ca-recorder-panel .ca-footer { padding: 12px 16px; border-top: 1px solid var(--ca-divider); text-align: center; font-size: 12px; color: #666; }
    #ca-recorder-panel .ca-footer a { color: var(--ca-primary); text-decoration: none; display: inline-flex; align-items: center; gap: 4px; }
    #ca-recorder-panel .ca-footer a:hover { text-decoration: underline; }
    #ca-recorder-panel .ca-footer svg { width: 14px; height: 14px; }
    #ca-recorder-panel .ca-footer-sep { margin: 0 6px; }
    #ca-recorder-panel .ca-actions { display: flex; gap: 6px; margin-top: 8px; }
    #ca-recorder-panel .ca-actions-end { justify-content: flex-end; }
    #ca-recorder-panel .ca-status { padding: 8px 12px; background: var(--ca-card); border-radius: 8px; font-size: 12px; text-align: center; margin-top: 8px; }
    #ca-recorder-panel .ca-status.recording { background: #3a1a1a; color: #ff6b6b; animation: ca-pulse 1.5s infinite; }
    @keyframes ca-pulse { 0%,100%{opacity:1} 50%{opacity:0.6} }
    #ca-recorder-panel .ca-toolbar { display: flex; gap: 4px; margin-bottom: 8px; }
    #ca-recorder-panel .ca-toggle { flex: 1; display: flex; align-items: center; justify-content: center; gap: 4px; padding: 6px 8px; background: var(--ca-card); border: 1px solid var(--ca-border); border-radius: 8px; cursor: pointer; color: var(--ca-text-muted); font-size: 12px; transition: all 0.2s; user-select: none; }
    #ca-recorder-panel .ca-toggle:hover { background: var(--ca-card-hover); }
    #ca-recorder-panel .ca-toggle.active { background: var(--ca-card-active); border-color: var(--ca-primary); color: #aab; box-shadow: 0 0 6px rgba(102,126,234,0.25); }
    #ca-recorder-panel .ca-shortcut-bar { font-size: 11px; color: #666; margin-bottom: 4px; }
    #ca-recorder-panel .ca-modal-overlay { position: fixed; inset: 0; background: rgba(0,0,0,0.6); z-index: 2147483646; display: flex; align-items: center; justify-content: center; }
    #ca-recorder-panel .ca-modal { background: var(--ca-bg); border-radius: 12px; padding: 20px; width: 400px; max-width: 90vw; color: var(--ca-text); }
    #ca-recorder-panel .ca-modal h4 { margin: 0 0 12px; }
    #ca-recorder-panel .ca-modal label, #ca-recorder-panel .ca-step-edit-modal label { display: block; margin-bottom: 4px; font-size: 13px; color: var(--ca-text-dim); }
    #ca-recorder-panel .ca-step-edit-modal label { font-size: 12px; color: var(--ca-text-muted); }
    #ca-recorder-panel .ca-form-input { width: 100%; padding: 8px 10px; background: var(--ca-card); border: 1px solid var(--ca-border); border-radius: 6px; color: var(--ca-text); font-size: 13px; margin-bottom: 10px; }
    #ca-recorder-panel textarea.ca-form-input { min-height: 60px; resize: vertical; }
    #ca-recorder-panel .ca-modal-actions { display: flex; gap: 8px; justify-content: flex-end; margin-top: 8px; }
    #ca-recorder-panel .ca-step-edit-overlay { position: fixed; inset: 0; background: rgba(0,0,0,0.5); z-index: 2147483646; display: flex; align-items: center; justify-content: center; }
    #ca-recorder-panel .ca-step-edit-modal { background: var(--ca-bg); border-radius: 12px; padding: 20px; width: 380px; max-width: 90vw; color: var(--ca-text); }
    #ca-recorder-panel .ca-step-edit-modal h4 { margin: 0 0 14px; font-size: 15px; }
    #ca-recorder-panel .ca-selector-status { font-size: 11px; margin-bottom: 8px; min-height: 16px; }
    #ca-recorder-panel .ca-selector-ok { color: var(--ca-success); }
    #ca-recorder-panel .ca-selector-warn { color: var(--ca-danger); }
    #ca-recorder-panel .ca-step-meta { font-size: 11px; color: #666; margin-bottom: 8px; }
    #ca-tooltip { position: fixed; z-index: 2147483645; pointer-events: none; background: rgba(26,26,46,0.95); color: #e0e0e0; padding: 8px 12px; border-radius: 8px; font-size: 12px; font-family: monospace; max-width: 400px; box-shadow: 0 4px 16px rgba(0,0,0,0.4); border-left: 3px solid #667eea; }
    #ca-tooltip .ca-tt-tag { color: #667eea; font-weight: bold; }
    #ca-tooltip .ca-tt-id { color: #4CAF50; }
    #ca-tooltip .ca-tt-class { color: #FF9800; }
    #ca-tooltip .ca-tt-hint { color: #888; font-size: 11px; margin-top: 4px; }
    .ca-highlight { outline: 3px solid #667eea !important; outline-offset: 2px !important; background: rgba(102,126,234,0.1) !important; }
    .ca-highlight-selected { outline: 3px solid #4CAF50 !important; outline-offset: 2px !important; background: rgba(76,175,80,0.1) !important; }
    .ca-revealed-highlight { outline: 3px dashed #4CAF50 !important; outline-offset: 3px !important; background: rgba(76,175,80,0.1) !important; cursor: pointer !important; animation: ca-reveal-pulse 2s infinite; }
    @keyframes ca-reveal-pulse { 0%,100% { outline-color: #4CAF50; } 50% { outline-color: #81C784; } }
    .ca-revealed-label { position: fixed; background: #4CAF50; color: #fff; padding: 2px 6px; border-radius: 3px; font-size: 10px; font-family: monospace; white-space: nowrap; z-index: 2147483646; pointer-events: none; transform: translateY(-110%); }
    #ca-reveal-panel { position: fixed; left: 10px; top: 10px; z-index: 2147483646; width: 260px; max-height: 60vh; overflow-y: auto; background: #1a1a2e; color: #e0e0e0; border-radius: 12px; box-shadow: 0 8px 32px rgba(0,0,0,0.5); font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', sans-serif; font-size: 12px; }
    #ca-reveal-panel .ca-rv-header { padding: 10px 12px; background: #2e7d32; border-radius: 12px 12px 0 0; font-weight: 600; font-size: 13px; display: flex; align-items: center; gap: 6px; }
    #ca-reveal-panel .ca-rv-count { background: #fff; color: #2e7d32; padding: 0 6px; border-radius: 10px; font-size: 11px; }
    #ca-reveal-panel .ca-rv-item { display: flex; align-items: center; gap: 8px; padding: 8px 12px; border-bottom: 1px solid #2a2a3e; cursor: pointer; transition: background 0.15s; }
    #ca-reveal-panel .ca-rv-item:hover { background: #2a2a4e; }
    #ca-reveal-panel .ca-rv-icon { font-size: 14px; flex-shrink: 0; }
    #ca-reveal-panel .ca-rv-info { flex: 1; min-width: 0; }
    #ca-reveal-panel .ca-rv-sel { font-family: monospace; font-size: 11px; color: #81C784; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; max-width: 180px; }
    #ca-reveal-panel .ca-rv-type { font-size: 10px; color: #888; }
    #ca-reveal-panel .ca-rv-btn { flex-shrink: 0; padding: 2px 8px; border: 1px solid #4CAF50; border-radius: 4px; background: transparent; color: #4CAF50; cursor: pointer; font-size: 11px; transition: all 0.15s; }
    #ca-reveal-panel .ca-rv-btn:hover { background: #4CAF50; color: #fff; }
    .ca-reveal-popup { position: fixed; z-index: 2147483647; background: #1a1a2e; color: #e0e0e0; border-radius: 10px; box-shadow: 0 8px 32px rgba(0,0,0,0.6); padding: 12px; font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', sans-serif; font-size: 13px; min-width: 200px; }
    .ca-reveal-popup .ca-rpop-header { margin-bottom: 8px; padding-bottom: 8px; border-bottom: 1px solid #333; font-size: 12px; word-break: break-all; }
    .ca-reveal-popup .ca-rpop-actions { display: flex; flex-wrap: wrap; gap: 6px; }
    .ca-reveal-popup .ca-rpop-actions button { padding: 6px 12px; border: 1px solid #444; border-radius: 6px; background: #2a2a3e; color: #ddd; cursor: pointer; font-size: 12px; transition: all 0.15s; }
    .ca-reveal-popup .ca-rpop-actions button:hover { background: #3a3a5e; border-color: #667eea; }
    .ca-reveal-popup .ca-rpop-actions button[data-rpop-type="dismiss"] { color: #888; border-color: transparent; }
    .ca-reveal-popup .ca-rpop-actions button[data-rpop-type="dismiss"]:hover { color: #e74c3c; }
    #ca-recorder-panel .ca-help-modal { width: 600px; max-height: 82vh; overflow-y: auto; padding: 24px; }
    #ca-recorder-panel .ca-help-header { display: flex; justify-content: space-between; align-items: center; margin-bottom: 16px; }
    #ca-recorder-panel .ca-help-header h4 { margin: 0; font-size: 18px; }
    #ca-recorder-panel .ca-help-close { background: none; border: none; color: var(--ca-text-muted); cursor: pointer; font-size: 20px; }
    #ca-recorder-panel .ca-help-body { line-height: 1.8; color: #ccc; }
    #ca-recorder-panel .ca-help-h5 { color: var(--ca-primary); margin: 14px 0 6px; }
    #ca-recorder-panel .ca-help-list { margin: 4px 0; padding-left: 18px; }
    #ca-recorder-panel .ca-help-list-sm { margin: 0 0 8px; padding-left: 18px; font-size: 12px; }
    #ca-recorder-panel .ca-help-tip { background: rgba(102,126,234,0.08); border-left: 3px solid var(--ca-primary); padding: 8px 12px; margin: 8px 0; border-radius: 0 6px 6px 0; font-size: 12px; line-height: 1.6; }
    #ca-recorder-panel .ca-help-table { width: 100%; font-size: 12px; border-collapse: collapse; margin: 4px 0; }
    #ca-recorder-panel .ca-help-table th, #ca-recorder-panel .ca-help-table td { padding: 3px 6px; }
    #ca-recorder-panel .ca-help-table-header { color: var(--ca-text-dim); }
    #ca-recorder-panel .ca-help-key { color: #fff; }
    #ca-recorder-panel .ca-help-footer { margin-top: 16px; padding-top: 12px; border-top: 1px solid var(--ca-divider); font-size: 11px; color: #666; text-align: center; }
    .ca-entry-btn { position: fixed; bottom: 20px; right: 20px; width: 48px; height: 48px; border-radius: 50%; background: linear-gradient(135deg, #667eea 0%, #764ba2 100%); display: flex; align-items: center; justify-content: center; font-size: 24px; cursor: pointer; z-index: 2147483647; box-shadow: 0 4px 16px rgba(102,126,234,0.4); transition: transform 0.2s; user-select: none; }
  `);

  // ==================== 选择器生成 ====================

  function getSelectors(el) {
    const selectors = [];
    let queryRoot = document;
    let inShadowRoot = false;
    try {
      const root = el.getRootNode?.();
      if (root && root instanceof ShadowRoot) {
        queryRoot = root;
        inShadowRoot = true;
      }
    } catch (_) {}

    function queryCount(sel) {
      try { return queryRoot.querySelectorAll(sel).length; } catch (_) { return 0; }
    }

    if (el.id && !/^\d/.test(el.id)) {
      selectors.push({ type: "css", value: `#${CSS.escape(el.id)}`, reliability: inShadowRoot ? 7 : 10, shadowScoped: inShadowRoot || undefined });
    }

    if (el.name) {
      const nameSelector = `${el.tagName.toLowerCase()}[name="${escapeCssString(el.name)}"]`;
      const matchCount = queryCount(nameSelector);
      const inIframe = el.ownerDocument !== document;
      const baseReliability = matchCount === 1 ? 9 : 6;
      selectors.push({ type: "css", value: nameSelector, reliability: inShadowRoot ? Math.min(baseReliability, 5) : (inIframe ? 5 : baseReliability), shadowScoped: inShadowRoot || undefined });
    }

    if (el.type && (el.tagName === "INPUT" || el.tagName === "BUTTON")) {
      const s = `${el.tagName.toLowerCase()}[type="${escapeCssString(el.type)}"]`;
      if (queryCount(s) === 1) selectors.push({ type: "css", value: s, reliability: inShadowRoot ? 5 : 7, shadowScoped: inShadowRoot || undefined });
    }

    if (el.placeholder) {
      const placeholderSelector = `${el.tagName.toLowerCase()}[placeholder="${escapeCssString(el.placeholder)}"]`;
      const placeholderCount = queryCount(placeholderSelector);
      selectors.push({ type: "css", value: placeholderSelector, reliability: placeholderCount === 1 ? (inShadowRoot ? 3 : 4) : 2, shadowScoped: inShadowRoot || undefined });
    }

    const testId = el.getAttribute("data-testid");
    if (testId) selectors.push({ type: "css", value: `[data-testid="${escapeCssString(testId)}"]`, reliability: inShadowRoot ? 7 : 10, shadowScoped: inShadowRoot || undefined });

    const ariaLabel = el.getAttribute("aria-label");
    if (ariaLabel) selectors.push({ type: "css", value: `[aria-label="${escapeCssString(ariaLabel)}"]`, reliability: inShadowRoot ? 5 : 7, shadowScoped: inShadowRoot || undefined });

    const text = (el.textContent || "").trim();
    if (text && text.length < 30 && ["A", "BUTTON", "SPAN", "DIV", "LABEL", "LI"].includes(el.tagName)) {
      selectors.push({ type: "text", value: text, reliability: 5 });
    }

    try {
      const shortCss = buildShortCss(el);
      if (shortCss && queryCount(shortCss) === 1) selectors.push({ type: "css", value: shortCss, reliability: inShadowRoot ? 2 : 4, shadowScoped: inShadowRoot || undefined });
    } catch (_) {}

    const xpath = buildXPath(el);
    if (xpath && xpath !== "/") selectors.push({ type: "xpath", value: xpath, reliability: inShadowRoot ? 1 : 3 });

    selectors.sort((a, b) => b.reliability - a.reliability);
    return selectors;
  }

  function isHashedClass(name) {
    return /^[a-z]+-[a-z0-9]{6,}(?:-[a-z0-9]+)?$/.test(name)
      || /^[a-zA-Z]+_\w{6,}(?:__\w+)?$/.test(name)
      || /^[a-zA-Z]+-[a-f0-9]{6,}$/.test(name)
      || /^sc-[a-zA-Z]+$/.test(name)
      || /^css-[a-z0-9]+$/.test(name)
      || /^[a-z][a-z0-9]{5,}$/.test(name)
      || /^_[a-z0-9]{4,}$/.test(name)
      || /^jss-\d+$/.test(name);
  }

  function getParentNode(el) {
    if (el.parentElement) return el.parentElement;
    const root = el.getRootNode?.();
    if (root && root !== document && root instanceof ShadowRoot) return root.host;
    return null;
  }

  function buildShortCss(el) {
    const parts = [];
    let current = el;
    while (current && current !== document.body && parts.length < 4) {
      let part = current.tagName.toLowerCase();
      if (current.id) {
        parts.unshift(`#${CSS.escape(current.id)}`);
        break;
      }
      if (current.className && typeof current.className === "string") {
        const classes = current.className.trim().split(/\s+/).filter(c => c && !/^[\d-]/.test(c) && !isHashedClass(c));
        if (classes.length > 0) part += "." + classes.slice(0, 2).map(c => CSS.escape(c)).join(".");
      }
      const parent = getParentNode(current);
      if (parent) {
        const siblings = Array.from(parent.children).filter(c => c.tagName === current.tagName);
        if (siblings.length > 1) part += `:nth-of-type(${siblings.indexOf(current) + 1})`;
      }
      parts.unshift(part);
      current = parent;
    }
    return parts.join(" > ");
  }

  function buildXPath(el) {
    const parts = [];
    let current = el;
    while (current && current !== document.body && parts.length < 6) {
      let part = current.tagName.toLowerCase();
      if (current.id) {
        parts.unshift(`//*[@id=${JSON.stringify(current.id)}]`);
        return parts.join("");
      }
      const parent = getParentNode(current);
      if (parent) {
        const siblings = Array.from(parent.children).filter(c => c.tagName === current.tagName);
        if (siblings.length > 1) part += `[${siblings.indexOf(current) + 1}]`;
      }
      parts.unshift(`/${part}`);
      current = parent;
    }
    return parts.join("") || "/";
  }

  // ==================== iframe 检测 ====================

  function detectIframe(el) {
    try {
      if (el.ownerDocument !== document) {
        let crossOriginFallback = null;
        function searchFrames(doc, depth) {
          if (depth > 10) return null;
          const frames = doc.querySelectorAll("iframe, frame");
          for (const frame of frames) {
            try {
              const contentDoc = frame.contentDocument;
              if (contentDoc === el.ownerDocument) {
                const tag = frame.tagName.toLowerCase();
                return {
                  inIframe: true,
                  frameSrc: frame.src || "",
                  frameName: frame.name || "",
                  frameId: frame.id || "",
                  frameSelector: frame.id ? `#${CSS.escape(frame.id)}` : frame.name ? `${tag}[name="${escapeCssString(frame.name)}"]` : buildShortCss(frame),
                };
              }
              if (contentDoc) {
                const nested = searchFrames(contentDoc, depth + 1);
                if (nested) return nested;
              }
            } catch (_) {
              if (!crossOriginFallback) {
                const tag = frame.tagName.toLowerCase();
                let fallbackSelector = frame.id ? `#${CSS.escape(frame.id)}` : frame.name ? `${tag}[name="${escapeCssString(frame.name)}"]` : buildShortCss(frame);
                if (!fallbackSelector) {
                  const allFrames = Array.from(document.querySelectorAll("iframe, frame"));
                  fallbackSelector = `${tag}:nth-of-type(${allFrames.indexOf(frame) + 1})`;
                }
                crossOriginFallback = { inIframe: true, crossOrigin: true, frameSrc: frame.src || "", frameName: frame.name || "", frameId: frame.id || "", frameSelector: fallbackSelector };
              }
            }
          }
          return null;
        }
        const found = searchFrames(document, 0);
        if (found) return found;
        if (crossOriginFallback) return crossOriginFallback;
        return { inIframe: true, crossOrigin: false };
      }

      if (window.self !== window.top) {
        let frameEl = null;
        try { frameEl = window.frameElement; } catch (_) {}
        if (frameEl) {
          const tag = frameEl.tagName.toLowerCase();
          return {
            inIframe: true,
            frameSrc: frameEl.src || "",
            frameName: frameEl.name || "",
            frameId: frameEl.id || "",
            frameSelector: frameEl.id ? `#${frameEl.id}` : frameEl.name ? `${tag}[name="${escapeCssString(frameEl.name)}"]` : null,
          };
        }
        return { inIframe: true, crossOrigin: true, frameSrc: "" };
      }
    } catch (_) {}
    return { inIframe: false };
  }

  // ==================== 元素信息提取 ====================

  function detectShadowRoot(el) {
    try {
      const root = el.getRootNode?.();
      if (root && root instanceof ShadowRoot) {
        const host = root.host;
        const hostInfo = host ? { tag: host.tagName.toLowerCase(), id: host.id || "", class: (host.className || "").substring(0, 100), selector: host.id ? `#${CSS.escape(host.id)}` : host.tagName.toLowerCase() } : null;
        return { inShadowRoot: true, host: hostInfo };
      }
    } catch (_) {}
    return { inShadowRoot: false, host: null };
  }

  function getElementInfo(el) {
    const tag = el.tagName.toLowerCase();
    const attrs = {};
    for (const attr of el.attributes) {
      if (isSensitiveAttributeName(attr.name)) continue;
      if (["id", "class", "name", "type", "placeholder", "href", "src", "action", "data-testid", "aria-label", "aria-describedby", "role"].includes(attr.name)) attrs[attr.name] = attr.value;
      if (attr.name.startsWith("data-") && !SENSITIVE_ATTR_RE.test(attr.name)) attrs[attr.name] = attr.value;
    }
    return {
      tag,
      attrs,
      text: (el.textContent || "").trim().substring(0, 100),
      selectors: getSelectors(el),
      iframe: detectIframe(el),
      shadowRoot: detectShadowRoot(el),
      visible: el.offsetParent !== null,
      rect: el.getBoundingClientRect().toJSON(),
    };
  }

  // ==================== 隐藏输入框检测 ====================

  function isElementCaptcha(el) {
    const name = (el.name || "").toLowerCase();
    const id = (el.id || "").toLowerCase();
    const cls = (el.className || "").toLowerCase();
    return name.includes("captcha") || id.includes("captcha") || cls.includes("captcha") || name.includes("verify") || id.includes("verify");
  }

  function isMathCaptchaElement(el) {
    const tag = el.tagName.toLowerCase();
    if (tag === "img" || tag === "canvas" || tag === "svg") return false;
    const text = (el.textContent || el.innerText || "").trim();
    return !!text && /\d+\s*[+\-×÷*\/]\s*\d+/.test(text);
  }

  function detectHiddenRealInput(el, stepType) {
    if (isElementHidden(el) && stepType !== "click" && stepType !== "submit") {
      if (el.id) return `#${CSS.escape(el.id)}`;
      if (el.name) return `input[name="${escapeCssString(el.name)}"]`;
    }

    const needPassword = stepType === "password";
    const typeSelector = needPassword ? 'input[type="password"]' : 'input[type="text"], input[type="email"], input[type="tel"], input:not([type])';
    let container = null;
    const knownSelectors = ["form", ".ant-input-affix-wrapper", "div[id$='_posi']", ".login_frame_hang_1", ".input-group, .form-group"];
    for (const sel of knownSelectors) {
      container = el.closest(sel);
      if (container) break;
    }
    if (!container) {
      let cur = el.parentElement;
      let depth = 0;
      while (cur && cur !== document.body && cur !== document.documentElement && depth < 6) {
        const candidates = cur.querySelectorAll(typeSelector);
        if (Array.from(candidates).some(inp => inp !== el && !inp.readOnly && isElementHidden(inp))) { container = cur; break; }
        cur = cur.parentElement;
        depth++;
      }
    }
    if (!container) container = el.parentElement;
    if (!container) return null;

    const searchRoots = [container];
    const parent = el.parentElement;
    if (parent && !searchRoots.includes(parent)) searchRoots.push(parent);

    const clickedIsTextDecoy = needPassword && el.tagName === "INPUT" && el.type === "text";
    if (clickedIsTextDecoy) {
      const roots = el.parentElement ? [el.parentElement, ...searchRoots] : searchRoots;
      for (const root of roots) {
        for (const input of root.querySelectorAll('input[type="password"]')) {
          if (input === el || input.readOnly) continue;
          if (input.id) return `#${CSS.escape(input.id)}`;
          if (input.name) return `input[name="${escapeCssString(input.name)}"]`;
        }
      }
    }

    const distanceCandidates = [];
    for (const root of searchRoots) {
      root.querySelectorAll(typeSelector).forEach(input => {
        if (input === el || input.readOnly || !isElementHidden(input)) return;
        if (stepType !== "captcha_input" && isElementCaptcha(input)) return;
        let distance = 0;
        let node = input.parentElement;
        while (node && node !== root) { distance++; node = node.parentElement; }
        distanceCandidates.push({ input, distance });
      });
    }
    distanceCandidates.sort((a, b) => a.distance - b.distance);
    for (const { input } of distanceCandidates) {
      if (input.id) return `#${CSS.escape(input.id)}`;
      if (input.name) return `input[name="${escapeCssString(input.name)}"]`;
    }

    const clickedIsCorrectType = el.tagName === "INPUT" && ((needPassword && el.type === "password") || (!needPassword && (el.type === "text" || el.type === "" || !el.type)));
    if (!clickedIsCorrectType) {
      const fallbackCandidates = [];
      for (const root of searchRoots) {
        root.querySelectorAll("input").forEach(input => {
          if (input === el || input.readOnly || !isElementHidden(input)) return;
          if (["submit", "button", "checkbox", "radio"].includes(input.type)) return;
          if (stepType !== "captcha_input" && isElementCaptcha(input)) return;
          let distance = 0;
          let node = input.parentElement;
          while (node && node !== root) { distance++; node = node.parentElement; }
          fallbackCandidates.push({ input, distance });
        });
      }
      fallbackCandidates.sort((a, b) => a.distance - b.distance);
      for (const { input } of fallbackCandidates) {
        if (input.id) return `#${CSS.escape(input.id)}`;
        if (input.name) return `input[name="${escapeCssString(input.name)}"]`;
      }
    }
    return null;
  }

  function isElementHidden(el) {
    if (!el) return true;
    try {
      const s = getComputedStyle(el);
      if (s.display === "none" || s.visibility === "hidden") return true;
      if (parseFloat(s.opacity) <= 0) return true;
      const r = el.getBoundingClientRect();
      if (r.width <= 0 || r.height <= 0) return true;
      if (s.clip === "rect(0px, 0px, 0px, 0px)" || s.clip === "rect(0, 0, 0, 0)") return true;
      if (typeof s.clipPath === "string" && s.clipPath.includes("inset(100%")) return true;
      if (r.left < -1000 || r.top < -1000) return true;
      if (el.offsetParent === null && s.position !== "fixed") return true;
    } catch (_) {}
    return false;
  }

  // ==================== UI ====================

  function showTooltip(el, x, y) {
    if (!state.tooltip) {
      state.tooltip = document.createElement("div");
      state.tooltip.id = "ca-tooltip";
      document.body.appendChild(state.tooltip);
    }
    const info = getElementInfo(el);
    const tag = `<span class="ca-tt-tag">&lt;${info.tag}&gt;</span>`;
    const id = info.attrs.id ? ` <span class="ca-tt-id">#${escHtml(info.attrs.id)}</span>` : "";
    const cls = info.attrs.class ? ` <span class="ca-tt-class">.${escHtml(info.attrs.class.split(/\s+/).slice(0, 2).join("."))}</span>` : "";
    const iframeHint = info.iframe.inIframe ? `<div class="ca-tt-hint">⚠️ 位于 frame/iframe 内${info.iframe.crossOrigin ? "（跨域）" : ""}</div>` : "";
    state.tooltip.innerHTML = `${tag}${id}${cls}${iframeHint}<div class="ca-tt-hint">🖱️ 点击记录  |  ⏎ Enter 无click记录</div>`;
    state.tooltip.style.left = `${Math.min(x + 12, window.innerWidth - LIMITS.TOOLTIP_MAX_WIDTH)}px`;
    state.tooltip.style.top = `${Math.min(y + 12, window.innerHeight - 100)}px`;
    state.tooltip.style.display = "block";
  }

  function hideTooltip() { if (state.tooltip) state.tooltip.style.display = "none"; }

  function createPanel() {
    if (state.panel) return;
    state.panel = document.createElement("div");
    state.panel.id = "ca-recorder-panel";
    state.panel.innerHTML = `
      <div class="ca-header" id="ca-drag-handle"><div class="ca-header-bar"><div><h3>🎬 Campus-Auth 任务录制器</h3><small>v${VERSION} — 选取元素，生成任务配置</small></div><button id="ca-btn-help" class="ca-help-btn" title="使用说明">?</button></div></div>
      <div class="ca-body">
        <div class="ca-section"><div class="ca-section-title">选择步骤类型后点击页面元素</div><div class="ca-step-grid" id="ca-step-grid"></div></div>
        <div class="ca-section"><div class="ca-toolbar"><span class="ca-toggle" id="ca-toggle-multistep">🔁 多步录制</span><span class="ca-toggle active" id="ca-toggle-detect">🔍 隐藏检测</span><span class="ca-toggle" id="ca-toggle-reveal">👁️ 显示隐藏</span></div><div class="ca-shortcut-bar">💡 <b>Esc</b> 取消  |  <b>Enter</b> 无 click 记录元素  |  点击 <b>?</b> 查看完整说明</div></div>
        <div class="ca-section"><div class="ca-section-title">已录制步骤</div><ul class="ca-recorded-list" id="ca-recorded-list"></ul><div class="ca-actions"><button class="ca-btn ca-btn-secondary ca-btn-sm" id="ca-btn-undo" disabled>↩ 撤销</button><button class="ca-btn ca-btn-danger ca-btn-sm" id="ca-btn-clear" disabled>🗑 清空</button></div></div>
        <div class="ca-status" id="ca-status">选择步骤类型后点击页面元素</div>
        <div class="ca-actions ca-actions-end" style="margin-top:12px;"><button class="ca-btn ca-btn-primary" id="ca-btn-copy-prompt">📋 复制 AI 提示词</button><button class="ca-btn ca-btn-danger ca-btn-sm" id="ca-btn-close" style="margin-left:auto;">✕</button></div>
      </div>
      <div class="ca-footer"><a href="https://github.com/Misyra/Campus-Auth" target="_blank">GitHub · Misyra/Campus-Auth</a><span class="ca-footer-sep">·</span><span>MIT License</span></div>`;
    document.body.appendChild(state.panel);

    const grid = state.panel.querySelector("#ca-step-grid");
    const primaryEntries = Object.entries(STEP_TYPES).filter(([, cfg]) => cfg.primary !== false);
    const secondaryEntries = Object.entries(STEP_TYPES).filter(([, cfg]) => cfg.primary === false);
    function createStepBtn(key, cfg) {
      const btn = document.createElement("div");
      btn.className = "ca-step-btn";
      btn.dataset.type = key;
      btn.innerHTML = `<span class="ca-icon">${cfg.icon}</span><span>${cfg.label}</span>`;
      btn.title = cfg.hint || cfg.label;
      btn.addEventListener("click", () => selectStepType(key));
      return btn;
    }
    for (const [key, cfg] of primaryEntries) grid.appendChild(createStepBtn(key, cfg));
    if (secondaryEntries.length) {
      const moreToggle = createStepBtn("more", { icon: "📋", label: "更多", hint: "更多步骤" });
      const moreContainer = document.createElement("div");
      moreContainer.id = "ca-more-container";
      moreContainer.className = "ca-more-container";
      moreContainer.style.display = "none";
      secondaryEntries.forEach(([key, cfg]) => moreContainer.appendChild(createStepBtn(key, cfg)));
      moreToggle.addEventListener("click", () => { moreContainer.style.display = moreContainer.style.display === "none" ? "contents" : "none"; });
      grid.appendChild(moreToggle);
      grid.appendChild(moreContainer);
    }

    const toggleMulti = state.panel.querySelector("#ca-toggle-multistep");
    const toggleHiddenDetect = state.panel.querySelector("#ca-toggle-detect");
    const toggleReveal = state.panel.querySelector("#ca-toggle-reveal");
    const refreshToggles = () => {
      toggleMulti.classList.toggle("active", state.multiStepMode);
      toggleHiddenDetect.classList.toggle("active", state.hiddenDetectionEnabled);
      toggleReveal.classList.toggle("active", state.revealEnabled);
    };
    toggleMulti.addEventListener("click", () => { state.multiStepMode = !state.multiStepMode; refreshToggles(); setStatus(state.multiStepMode ? "🔁 多步录制已开启 — 连续点击记录，按 Esc 停止" : "多步录制已关闭"); });
    toggleHiddenDetect.addEventListener("click", () => { state.hiddenDetectionEnabled = !state.hiddenDetectionEnabled; refreshToggles(); setStatus(state.hiddenDetectionEnabled ? "🔍 隐藏元素检测已开启" : "隐藏元素检测已关闭"); });
    toggleReveal.addEventListener("click", () => { state.revealEnabled = !state.revealEnabled; refreshToggles(); state.revealEnabled ? revealHiddenInputsForRecorder() : hideRevealedInputs(); });

    document.addEventListener("input", (e) => {
      const el = e.composedPath()[0];
      if (!el || !["INPUT", "TEXTAREA"].includes(el.tagName)) return;
      if (["checkbox", "radio", "submit", "button"].includes(el.type)) return;
      if (state.currentStepType !== "smart_detect") return;
      const stepType = el.type === "password" ? "password" : "username";
      addManualFillStep(stepType, el, stepType === "password" ? "密码输入框 → {{PASSWORD}}" : "账号输入框 → {{USERNAME}}");
      setStatus("🔍 已记录。继续点击或输入，按 Esc 停止", "recording");
    }, true);

    document.addEventListener("change", (e) => {
      if (state.currentStepType !== "smart_detect") return;
      const el = e.target;
      if (!el || el === state.panel || state.panel?.contains(el)) return;
      const tag = el.tagName.toLowerCase();
      if (tag === "input" && el.type === "checkbox") addManualFillStep("checkbox", el, "勾选: " + (el.name || el.id || "checkbox"));
      else if (tag === "select") addManualFillStep("carrier", el, "运营商选择 → {{ISP}}");
    }, true);

    state.panel.querySelector("#ca-recorded-list").addEventListener("click", (e) => {
      const delBtn = e.target.closest(".ca-del");
      if (delBtn) { e.stopPropagation(); state.steps.splice(parseInt(delBtn.dataset.idx), 1); updateRecordedList(); saveState(); return; }
      const item = e.target.closest(".ca-recorded-item");
      if (item) showStepEditModal(parseInt(item.dataset.idx));
    });
    state.panel.querySelector("#ca-btn-undo").addEventListener("click", undoStep);
    state.panel.querySelector("#ca-btn-clear").addEventListener("click", clearSteps);
    state.panel.querySelector("#ca-btn-copy-prompt").addEventListener("click", () => { GM_setClipboard(generatePrompt(window.location.href)); setStatus("✅ 已复制脱敏后的 AI 提示词"); });
    state.panel.querySelector("#ca-btn-close").addEventListener("click", deactivate);
    state.panel.querySelector("#ca-btn-help").addEventListener("click", showHelpModal);
    makeDraggable(state.panel, state.panel.querySelector("#ca-drag-handle"));
  }

  function selectStepType(type) {
    if (type === "more") return;
    state.currentStepType = type;
    state.carrierClickPhase = null;
    state.panel.querySelectorAll(".ca-step-btn").forEach(b => b.classList.toggle("active", b.dataset.type === type));
    setStatus(`${STEP_TYPES[type]?.icon || "📝"} ${STEP_TYPES[type]?.hint || STEP_TYPES[type]?.label || type}`, "recording");
    state.recording = true;
  }

  function setStatus(msg, cls) {
    const el = state.panel?.querySelector("#ca-status");
    if (!el) return;
    el.textContent = msg;
    el.className = "ca-status" + (cls ? ` ${cls}` : "");
  }

  function updateRecordedList() {
    const list = state.panel.querySelector("#ca-recorded-list");
    list.innerHTML = state.steps.map((s, i) => {
      const displaySelector = s.hiddenRealSelector ? `${s.tipSelector || ""}${s.tipSelector ? " → " : ""}${s.hiddenRealSelector}` : (s.bestSelector || "(无选择器)");
      return `<li class="ca-recorded-item" data-idx="${i}"><span class="ca-idx">${i + 1}</span><div class="ca-info"><div class="ca-label">${STEP_TYPES[s.type]?.icon || "📝"} ${escHtml(s.description || "")}</div><div class="ca-selector">${escHtml(displaySelector)}</div></div><button class="ca-del" data-idx="${i}">✕</button></li>`;
    }).join("");
    updateButtons();
  }

  function showStepEditModal(idx) {
    const step = state.steps[idx];
    if (!step) return;
    createModal({
      title: `✏️ 编辑步骤 ${idx + 1}`,
      fields: [
        { name: "desc", kind: "text", label: "描述", value: step.description || "" },
        { name: "selector", kind: "text", label: "选择器", value: step.bestSelector || "" },
      ],
      onSubmit: (values) => {
        step.description = values.desc || step.description;
        if (values.selector) {
          step.bestSelector = values.selector;
          if (!step.selectorCandidates?.includes(values.selector)) step.selectorCandidates = [values.selector, ...(step.selectorCandidates || [])];
        }
        updateRecordedList(); saveState(); setStatus(`✅ 步骤 ${idx + 1} 已更新`);
      },
    });
  }

  function updateButtons() {
    const has = state.steps.length > 0;
    state.panel.querySelector("#ca-btn-undo").disabled = !has;
    state.panel.querySelector("#ca-btn-clear").disabled = !has;
    state.panel.querySelector("#ca-btn-copy-prompt").style.display = has ? "" : "none";
  }

  function undoStep() { state.steps.pop(); updateRecordedList(); saveState(); setStatus("已撤销最后一步"); }
  function clearSteps() { state.steps = []; state.carrierClickPhase = null; updateRecordedList(); clearSavedState(); setStatus("已清空所有步骤"); }

  // ==================== 元素点击处理 ====================

  function onHover(e) {
    if (!state.recording) return;
    const el = e.target;
    if (!el || el === state.panel || state.panel?.contains(el)) return;
    if (state.hoveredEl && state.hoveredEl !== state.selectedEl) state.hoveredEl.classList.remove("ca-highlight");
    state.hoveredEl = el;
    if (el !== state.selectedEl) el.classList.add("ca-highlight");
    showTooltip(el, e.clientX, e.clientY);
  }

  function onClick(e) {
    if (!e.isTrusted || !state.recording) return;
    let el = e.target;
    if (el.tagName === "OPTION") el = el.closest("select") || el.parentElement || el;
    if (el === state.panel || state.panel?.contains(el) || el.closest("#ca-tooltip")) return;
    const needsClickThrough = state.currentStepType === "smart_detect" || (state.currentStepType === "carrier" && (el.tagName !== "SELECT" || state.carrierClickPhase));
    if (!needsClickThrough) { e.preventDefault(); e.stopPropagation(); }
    el.classList.remove("ca-highlight");
    el.classList.add("ca-highlight-selected");
    if (state.selectedEl && state.selectedEl !== el) state.selectedEl.classList.remove("ca-highlight-selected");
    state.selectedEl = el;
    hideTooltip();
    handleElementSelected(el, getElementInfo(el));
  }

  const STEP_HANDLERS = {
    captcha_img: (el, info) => { addStepFromElement("captcha_img", el, info, isMathCaptchaElement(el) ? "数学验证码容器" : "验证码图片"); selectStepType("captcha_input"); },
    captcha_input: (el, info) => showCaptchaModal(el, info),
    username: (el, info) => addStepFromElement("username", el, info, "账号输入框"),
    password: (el, info) => addStepFromElement("password", el, info, "密码输入框"),
    carrier: (el, info) => handleCarrierClickPhase(el, info),
    submit: (el, info) => addStepFromElement("submit", el, info, "提交按钮"),
    checkbox: (el, info) => addStepFromElement("checkbox", el, info, info.text ? `勾选: ${info.text.substring(0, 30)}` : "勾选/用户协议"),
    smart_detect: (el, info) => handleSmartDetectClick(el, info),
    sleep: (el, info) => showSleepModal(el, info),
    screenshot: (el, info) => showScreenshotModal(el, info),
    wait: (el, info) => addStepFromElement("wait", el, info, info.text ? `等待元素出现: ${info.text.substring(0, 30)}` : `等待元素: ${info.tag}`),
    eval: (el, info) => showEvalModal(el, info),
    wait_url: (el, info) => showWaitUrlModal(el, info),
  };

  function handleElementSelected(el, info) {
    const handler = STEP_HANDLERS[state.currentStepType];
    handler ? handler(el, info) : showCustomStepModal(state.currentStepType, el, info);
  }

  function findStepContainer(el) {
    let cur = el.parentElement;
    let best = null;
    let depth = 0;
    while (cur && cur !== document.body && cur !== document.documentElement && depth < 5) {
      best = cur;
      const tag = cur.tagName.toLowerCase();
      const marker = `${typeof cur.className === "string" ? cur.className : ""} ${cur.id || ""}`;
      if (tag === "form" || tag === "fieldset" || /login|auth|form|panel|container/i.test(marker)) break;
      cur = cur.parentElement;
      depth++;
    }
    return best;
  }

  function _detectHiddenInputInfo(el, type) {
    const result = { hiddenRealSelector: null, hiddenRealHTML: "", hiddenRealTag: "", hiddenRealRelation: "" };
    if (!["username", "password", "captcha_input"].includes(type) || !state.hiddenDetectionEnabled) return result;
    result.hiddenRealSelector = detectHiddenRealInput(el, type);
    if (!result.hiddenRealSelector) return result;
    const hiddenEl = queryRecordedElement(result.hiddenRealSelector);
    if (hiddenEl) {
      result.hiddenRealHTML = sanitizeDomHtml(hiddenEl, false, LIMITS.HTML_HIDDEN);
      result.hiddenRealTag = hiddenEl.tagName.toLowerCase();
      if (hiddenEl.parentElement === el.parentElement) result.hiddenRealRelation = `同一 <${el.parentElement.tagName.toLowerCase()}> 内的兄弟元素`;
      else result.hiddenRealRelation = "位于同一登录容器内";
    }
    return result;
  }

  function buildStepBase(type, el, info, description) {
    return {
      type,
      description,
      tag: info.tag,
      bestSelector: selectorForPlayback(info.selectors[0]),
      selectorCandidates: selectorCandidatesForPlayback(info),
      iframe: info.iframe,
      shadowRoot: info.shadowRoot,
      attrs: info.attrs,
      text: info.text,
      visible: info.visible,
      elementHTML: sanitizeDomHtml(el, false, LIMITS.HTML_ELEMENT),
      elementParentContext: sanitizeDomHtml(el.parentElement, true, LIMITS.HTML_ELEMENT),
      elementContainerHTML: sanitizeDomHtml(findStepContainer(el), true, LIMITS.HTML_CONTAINER),
    };
  }

  function commitStep(step) {
    state.steps.push(step);
    state.selectedEl?.classList.remove("ca-highlight-selected");
    state.selectedEl = null;
    updateRecordedList(); saveState();
  }

  function maybeStopRecording() {
    const isSmartDetect = state.currentStepType === "smart_detect";
    if (!state.multiStepMode && !isSmartDetect) {
      state.recording = false;
      state.panel.querySelectorAll(".ca-step-btn").forEach(b => b.classList.remove("active"));
    }
  }

  function addStepFromElement(type, el, info, description) {
    let tipSelector = null;
    if (el.tagName === "LABEL" && el.htmlFor) {
      const target = document.getElementById(el.htmlFor);
      if (target && ["INPUT", "TEXTAREA", "SELECT"].includes(target.tagName)) {
        if (isElementHidden(target)) tipSelector = selectorForPlayback(info.selectors[0]);
        info = getElementInfo(target);
        el = target;
      }
    }
    const bestSelector = selectorForPlayback(info.selectors[0]);
    if (state.steps.some(s => s.type === type && s.bestSelector === bestSelector)) { setStatus(`⏭️ 已跳过重复: ${description}`, "recording"); return; }
    const hiddenInfo = _detectHiddenInputInfo(el, type);
    if (!tipSelector && hiddenInfo.hiddenRealSelector && hiddenInfo.hiddenRealSelector !== bestSelector) tipSelector = bestSelector;
    const step = buildStepBase(type, el, info, description);
    Object.assign(step, hiddenInfo, { tipSelector, hiddenWarning: hiddenInfo.hiddenRealSelector ? `检测到隐藏输入框 ${hiddenInfo.hiddenRealSelector}` : "" });
    commitStep(step);
    setStatus(`已添加: ${description}`);
    maybeStopRecording();
  }

  function addManualFillStep(type, el, description) {
    const info = getElementInfo(el);
    const bestSelector = selectorForPlayback(info.selectors[0]);
    if (state.steps.some(s => s.type === type && s.bestSelector === bestSelector)) return;
    const step = buildStepBase(type, el, info, description);
    Object.assign(step, _detectHiddenInputInfo(el, type));
    commitStep(step);
  }

  function handleSmartDetectClick(el, info) {
    const tag = el.tagName.toLowerCase();
    const type = (el.type || "").toLowerCase();
    if (type === "submit" || (tag === "button" && /登录|提交|submit|login/i.test(el.textContent || el.value || ""))) return addStepFromElement("submit", el, info, "提交按钮");
    if (tag === "img") return addStepFromElement("captcha_img", el, info, "验证码图片");
    if (isMathCaptchaElement(el)) return addStepFromElement("captcha_img", el, info, "数学验证码容器");
    if (["input", "select", "textarea"].includes(tag) || (tag === "label" && el.htmlFor)) return;
    addStepFromElement("click", el, info, info.text ? `点击: ${info.text.substring(0, 30)}` : `点击: ${tag}`);
  }

  function findOptionContainer(el) {
    let cur = el.parentElement;
    let depth = 0;
    while (cur && cur !== document.body && depth < 6) {
      const marker = `${cur.tagName} ${cur.className || ""} ${cur.id || ""}`;
      if (/UL|OL|dropdown|select|menu|list|option|popup|pull-down/i.test(marker)) return cur;
      if (cur.children.length >= 2) return cur;
      cur = cur.parentElement;
      depth++;
    }
    return el.parentElement;
  }

  function handleCarrierClickPhase(el, info) {
    if (!state.carrierClickPhase && info.tag === "select") return addStepFromElement("carrier", el, info, "运营商选择 → {{ISP}}");
    if (!state.carrierClickPhase) {
      const group = detectButtonGroup(el);
      if (group) return recordButtonGroupCarrier(el, info, group);
      state.carrierClickPhase = { triggerEl: el, triggerInfo: info };
      state.selectedEl = null;
      setStatus("🔽 已记录触发器，现在点击一个运营商选项", "recording");
      return;
    }
    const triggerInfo = state.carrierClickPhase.triggerInfo;
    const optionContainer = findOptionContainer(el);
    const containerInfo = optionContainer ? getElementInfo(optionContainer) : null;
    const step = {
      ...buildStepBase("carrier", state.carrierClickPhase.triggerEl, triggerInfo, `运营商选择 → {{ISP}}（示例: ${(el.textContent || "").trim().substring(0, 50)}）`),
      optionText: (el.textContent || "").trim().substring(0, 50),
      optionTag: info.tag,
      optionSelector: selectorForPlayback(containerInfo?.selectors?.[0] || info.selectors[0]),
    };
    commitStep(step);
    state.carrierClickPhase = null;
    setStatus(`已添加: 运营商选择 → {{ISP}}`);
    maybeStopRecording();
  }

  function detectButtonGroup(el) {
    for (let depth = 0; depth < 3; depth++) {
      if (!el || !el.parentElement) break;
      el = el.parentElement;
      if (el.children.length < 2) continue;
      const siblings = Array.from(el.children).filter(s => (s.textContent || "").trim().length > 0 && (s.textContent || "").trim().length < 40);
      const tagCounts = {};
      siblings.forEach(s => { tagCounts[s.tagName] = (tagCounts[s.tagName] || 0) + 1; });
      const mode = Object.entries(tagCounts).sort((a, b) => b[1] - a[1])[0];
      if (!mode) continue;
      const similar = siblings.filter(s => s.tagName === mode[0]);
      if (similar.length >= 2) return similar;
    }
    return null;
  }

  function recordButtonGroupCarrier(el, info, group) {
    const groupContainer = group[0].parentElement;
    const groupContainerInfo = groupContainer ? getElementInfo(groupContainer) : { selectors: [] };
    const containerSelector = selectorForPlayback(groupContainerInfo.selectors[0]);
    const allOptions = group.map(s => (s.textContent || "").trim().substring(0, 30)).filter(Boolean);
    const step = {
      ...buildStepBase("carrier", el, info, `运营商按钮组 → {{ISP}}（示例: ${(el.textContent || "").trim().substring(0, 50)}）`),
      optionText: (el.textContent || "").trim().substring(0, 50),
      optionTag: info.tag,
      optionSelector: containerSelector,
      carrierMode: "button_group",
      allOptions,
      containerSelector,
    };
    commitStep(step);
    setStatus(`已添加: 运营商按钮组 → {{ISP}}（${allOptions.join("、")}）`);
    maybeStopRecording();
  }

  // ==================== 弹窗 ====================

  function createModal({ title, fields, onSubmit, onCancel, ctx }) {
    const overlay = document.createElement("div");
    overlay.className = "ca-modal-overlay";
    const renderField = (f) => {
      if (f.condition && !f.condition(ctx)) return "";
      const id = `ca-mf-${f.name}`;
      const label = f.label ? `<label>${f.label}</label>` : "";
      if (f.kind === "textarea") return `${label}<textarea class="ca-form-input" id="${id}" placeholder="${escHtml(f.placeholder || "")}">${escHtml(f.value || "")}</textarea>`;
      if (f.kind === "select") return `${label}<select class="ca-form-input" id="${id}">${(f.options || []).map(o => `<option value="${escHtml(o.value)}">${escHtml(o.label)}</option>`).join("")}</select>`;
      return `${label}<input class="ca-form-input" type="${f.kind}" id="${id}" value="${escHtml(f.value ?? "")}" placeholder="${escHtml(f.placeholder || "")}" />`;
    };
    overlay.innerHTML = `<div class="ca-modal"><h4>${title}</h4>${fields.map(renderField).join("")}<div class="ca-modal-actions"><button class="ca-btn ca-btn-secondary ca-btn-sm" id="ca-mf-cancel">取消</button><button class="ca-btn ca-btn-primary ca-btn-sm" id="ca-mf-ok">确定</button></div></div>`;
    state.panel.appendChild(overlay);
    const getValues = () => Object.fromEntries(fields.filter(f => !f.condition || f.condition(ctx)).map(f => {
      const el = overlay.querySelector(`#ca-mf-${f.name}`);
      return [f.name, f.kind === "number" ? (parseInt(el?.value) || 0) : (el?.value || "").trim()];
    }));
    const setField = (name, value) => { const el = overlay.querySelector(`#ca-mf-${name}`); if (el) el.value = value; };
    fields.forEach(f => { if (f.kind === "select" && f.onChange) overlay.querySelector(`#ca-mf-${f.name}`)?.addEventListener("change", () => f.onChange(getValues(), setField)); });
    const close = () => { document.removeEventListener("keydown", onKey, true); overlay.remove(); };
    const cancel = () => { close(); (onCancel || (() => { state.recording = true; }))(); };
    const onKey = e => { if (e.key === "Escape") cancel(); };
    overlay.addEventListener("click", e => { if (e.target === overlay) cancel(); });
    overlay.querySelector("#ca-mf-cancel").addEventListener("click", cancel);
    overlay.querySelector("#ca-mf-ok").addEventListener("click", () => { if (onSubmit(getValues(), { close, ctx }) !== false) close(); });
    document.addEventListener("keydown", onKey, true);
    setTimeout(() => overlay.querySelector(".ca-form-input")?.focus(), 0);
    return { overlay, getValues, close };
  }

  function showCaptchaModal(el, info) {
    const { overlay } = createModal({
      title: "🖼️ 验证码设置",
      fields: [
        { name: "type", kind: "select", label: "验证码类型", options: CAPTCHA_TYPES.map(t => ({ value: t.value, label: t.label })), onChange: (values, setField) => { const t = CAPTCHA_TYPES.find(c => c.value === values.type); if (t) setField("charRange", t.charRange); } },
        { name: "charRange", kind: "text", label: "OCR 字符范围" },
        { name: "desc", kind: "text", label: "自定义描述（可选）" },
      ],
      onSubmit: values => {
        addStepFromElement("captcha_input", el, info, `验证码输入: ${values.desc || values.type}`);
        const imgStep = [...state.steps].reverse().find(s => s.type === "captcha_img");
        if (imgStep) { imgStep.captchaType = values.type; if (values.charRange) imgStep.charRange = values.charRange; }
        const inputStep = state.steps[state.steps.length - 1];
        if (inputStep) { inputStep.captchaType = values.type; if (values.charRange) inputStep.charRange = values.charRange; }
      },
    });
    if (overlay.querySelector("#ca-mf-charRange")) overlay.querySelector("#ca-mf-charRange").value = CAPTCHA_TYPES[0].charRange;
  }

  function showCustomStepModal(type, el, info) {
    createModal({
      title: `${STEP_TYPES[type]?.icon || "📝"} ${STEP_TYPES[type]?.label || type}`,
      ctx: { type },
      fields: [
        { name: "desc", kind: "text", label: "步骤描述" },
        { name: "value", kind: "text", label: "填入的值（可选）", condition: c => c.type !== "click" },
        { name: "selector", kind: "text", label: "选择器", value: selectorForPlayback(info.selectors[0]) },
      ],
      onSubmit: values => {
        const step = buildStepBase(type, el, info, values.desc || STEP_TYPES[type]?.label || type);
        if (values.selector) {
          step.bestSelector = values.selector;
          step.selectorCandidates = [values.selector, ...step.selectorCandidates.filter(x => x !== values.selector)];
        }
        if (values.value) step.value = values.value;
        commitStep(step); setStatus(`已添加: ${step.description}`); maybeStopRecording();
      },
    });
  }

  function showEvalModal() {
    createModal({ title: "⚙️ 执行 JavaScript", fields: [{ name: "code", kind: "textarea", label: "JS 代码" }, { name: "desc", kind: "text", label: "描述（可选）" }], onSubmit: values => {
      if (!values.code) return false;
      state.steps.push({ type: "eval", description: values.desc || `执行 JS: ${values.code.substring(0, 40)}`, script: values.code }); updateRecordedList(); saveState(); state.recording = false;
    }});
  }

  function showSleepModal() {
    createModal({ title: "⏳ 延时等待", fields: [{ name: "duration", kind: "number", label: "等待时长（毫秒）", value: 1000 }, { name: "desc", kind: "text", label: "描述（可选）" }], onSubmit: values => {
      state.steps.push({ type: "sleep", description: values.desc || `等待 ${values.duration || 1000}ms`, duration: values.duration || 1000 }); updateRecordedList(); saveState(); state.recording = false;
    }});
  }

  function showScreenshotModal() {
    createModal({ title: "📸 页面截图", fields: [{ name: "path", kind: "text", label: "截图名称（可选）" }, { name: "desc", kind: "text", label: "描述（可选）" }], onSubmit: values => {
      const step = { type: "screenshot", description: values.desc || "页面截图" }; if (values.path) step.path = values.path; state.steps.push(step); updateRecordedList(); saveState(); state.recording = false;
    }});
  }

  function showWaitUrlModal() {
    createModal({ title: "🔗 等待 URL", fields: [{ name: "pattern", kind: "text", label: "URL 正则表达式" }, { name: "timeout", kind: "number", label: "超时（毫秒）", value: 10000 }, { name: "desc", kind: "text", label: "描述（可选）" }], onSubmit: values => {
      if (!values.pattern) return false;
      state.steps.push({ type: "wait_url", description: values.desc || `等待 URL 匹配: ${values.pattern}`, pattern: values.pattern, timeout: values.timeout || 10000 }); updateRecordedList(); saveState(); state.recording = false;
    }});
  }

  // ==================== AI 提示词 ====================

  function generatePrompt(url) {
    let prompt = `请根据以下校园网登录页面的录制信息，生成 Campus-Auth 任务 JSON。\n\n`;
    prompt += `任务规范以 docs/guides/task-writing-guide.md 为准。录制器提供的 HTML 已脱敏：输入 value、口令/token/session/cookie/CSRF 等属性不会复制到剪贴板。\n\n`;
    prompt += `## 页面地址\n${url}\n\n`;
    prompt += `不要硬编码 url；优先留空或使用 {{LOGIN_URL}}。\n\n`;
    prompt += `## 变量\n- {{USERNAME}} 账号\n- {{PASSWORD}} 密码\n- {{ISP}} 运营商\n- {{LOGIN_URL}} 认证地址\n- eval/ocr 的 store_as 结果可供后续 {{变量名}} 使用\n\n`;
    prompt += `## 映射规则\n- username/password → input\n- carrier 原生 select → select；自定义下拉/按钮组 → click_select，selector 为触发器，option_selector 只限定搜索范围，value={{ISP}}\n- captcha → ocr；数学验证码可用 ocr + eval\n- submit/checkbox/click → click\n- wait → 等元素出现；sleep → 固定延时\n- eval → eval；wait_url → wait_url\n\n`;

    const stepEls = [];
    for (const s of state.steps) {
      const el = queryRecordedElement(s.bestSelector);
      if (el && !stepEls.includes(el)) stepEls.push(el);
    }
    if (stepEls.length) {
      let common = stepEls[0];
      for (let i = 1; i < stepEls.length; i++) {
        let b = stepEls[i];
        const parents = [];
        for (let a = common; a; a = a.parentElement) parents.push(a);
        while (b && !parents.includes(b)) b = b.parentElement;
        if (b) common = b;
      }
      if (common?.parentElement && common.parentElement !== document.body && common.parentElement !== document.documentElement) common = common.parentElement;
      const html = sanitizeDomHtml(common, true, LIMITS.HTML_CONTEXT);
      if (html) prompt += `## 页面上下文 HTML（已脱敏）\n\`\`\`html\n${html}\n\`\`\`\n\n`;
    }

    prompt += `## 录制步骤 (${state.steps.length})\n\n`;
    state.steps.forEach((s, i) => {
      prompt += `### ${i + 1}. ${STEP_TYPES[s.type]?.label || s.type}\n`;
      prompt += `- 类型: ${s.type}\n- 描述: ${s.description}\n`;
      if (s.bestSelector) prompt += `- 最佳选择器: \`${s.bestSelector}\`\n`;
      if (s.selectorCandidates?.length > 1) prompt += `- 候选: ${s.selectorCandidates.map(x => `\`${x}\``).join(", ")}\n`;
      if (s.elementHTML) prompt += `- 元素 HTML（已脱敏）:\n\`\`\`html\n${s.elementHTML}\n\`\`\`\n`;
      if (s.hiddenRealSelector) prompt += `- 隐藏真实输入框: \`${s.hiddenRealSelector}\`\n`;
      if (s.iframe?.inIframe && s.iframe.frameSelector) prompt += `- frame: \`${s.iframe.frameSelector}\`\n`;
      if (s.shadowRoot?.inShadowRoot) prompt += `- Shadow DOM: open shadow root 可由 Playwright CSS locator 穿透；避免使用 XPath 穿透 Shadow Root\n`;
      if (s.type === "carrier") {
        if (s.carrierMode === "button_group") prompt += `- 按钮组选项容器: \`${s.optionSelector}\`；选项: ${(s.allOptions || []).join("、")}\n`;
        else if (s.optionText) prompt += `- 自定义下拉 option_selector: \`${s.optionSelector || ""}\`；示例选项: ${s.optionText}\n`;
      }
      if (s.captchaType) prompt += `- 验证码类型: ${s.captchaType}${s.charRange ? `；char_range=${s.charRange}` : ""}\n`;
      if (s.type === "eval" && s.script) prompt += `- script:\n\`\`\`js\n${s.script}\n\`\`\`\n`;
      if (s.type === "sleep") prompt += `- duration: ${s.duration || 1000}ms\n`;
      if (s.type === "wait_url") prompt += `- pattern: \`${s.pattern}\`；timeout=${s.timeout || 10000}ms\n`;
      prompt += `\n`;
    });

    prompt += `## 输出要求\n1. 直接输出完整 JSON，所有关键步骤加语义化 id。\n2. required 默认是 true；只有明确允许缺失的步骤才写 required:false。\n3. 优先使用录制器的 Playwright-ready 选择器（text= / xpath= 已带类型），并结合脱敏 HTML 验证。\n4. 输出后询问“任务是否成功？”，失败时再给针对当前页面的 eval 兜底。\n`;
    return prompt;
  }

  // ==================== 拖拽 / frame / SPA ====================

  function makeDraggable(panel, handle) {
    let dragging = false, startX = 0, startY = 0, left = 0, top = 0;
    handle.addEventListener("mousedown", e => {
      if (e.target.tagName === "BUTTON") return;
      dragging = true; startX = e.clientX; startY = e.clientY;
      const r = panel.getBoundingClientRect(); left = r.left; top = r.top; e.preventDefault();
    });
    document.addEventListener("mousemove", e => { if (!dragging) return; panel.style.left = `${left + e.clientX - startX}px`; panel.style.top = `${top + e.clientY - startY}px`; panel.style.right = "auto"; });
    document.addEventListener("mouseup", () => { dragging = false; });
  }

  function attachFrameListeners(doc) { try { doc.addEventListener("mouseover", onHover, true); doc.addEventListener("click", onClick, true); doc.addEventListener("keydown", onKeyDown, true); } catch (_) {} }
  function detachFrameListeners(doc) { try { doc.removeEventListener("mouseover", onHover, true); doc.removeEventListener("click", onClick, true); doc.removeEventListener("keydown", onKeyDown, true); } catch (_) {} }

  let _iframeObserver = null;
  function bindIframeLoad(frame) {
    try { if (frame.contentDocument) attachFrameListeners(frame.contentDocument); } catch (_) {}
    frame.addEventListener("load", () => { try { if (frame.contentDocument) attachFrameListeners(frame.contentDocument); } catch (_) {} });
  }
  function attachAllFrameListeners() {
    document.querySelectorAll("iframe, frame").forEach(bindIframeLoad);
    if (!_iframeObserver && document.body) {
      _iframeObserver = new MutationObserver(records => records.forEach(r => r.addedNodes.forEach(node => {
        if (node.nodeType !== 1) return;
        if (["IFRAME", "FRAME"].includes(node.tagName)) bindIframeLoad(node);
        node.querySelectorAll?.("iframe, frame").forEach(bindIframeLoad);
      })));
      _iframeObserver.observe(document.body, { childList: true, subtree: true });
    }
  }
  function detachAllFrameListeners() {
    document.querySelectorAll("iframe, frame").forEach(frame => { try { if (frame.contentDocument) detachFrameListeners(frame.contentDocument); } catch (_) {} });
    _iframeObserver?.disconnect(); _iframeObserver = null;
  }

  let _spaFormObserver = null;
  function startSpaFormWatcher() {
    if (_spaFormObserver || !document.body) return;
    _spaFormObserver = new MutationObserver(records => {
      const hasForm = records.some(r => Array.from(r.addedNodes).some(node => node.nodeType === 1 && (node.matches?.("form,input,select,button") || node.querySelector?.("form,input,select,button"))));
      if (hasForm && state.active) setStatus("🆕 检测到新表单元素，可继续录制");
    });
    _spaFormObserver.observe(document.body, { childList: true, subtree: true });
  }
  function stopSpaFormWatcher() { _spaFormObserver?.disconnect(); _spaFormObserver = null; }

  // ==================== 帮助 / 隐藏输入框显示 ====================

  function showHelpModal() {
    createModal({
      title: `📖 任务录制器 v${VERSION}`,
      fields: [],
      onSubmit: () => true,
    });
    const modal = state.panel.querySelector(".ca-modal-overlay:last-child .ca-modal");
    if (modal) modal.insertAdjacentHTML("afterbegin", `<div class="ca-help-body"><p>推荐使用“智能检测”，正常操作登录页即可。录制器只复制脱敏后的 DOM 结构，不复制输入 value 或 token/session 等敏感属性。</p><p>选择器优先级：稳定 id/data-testid/name → aria/type → 文本 → CSS 路径 → XPath。导出的 text=/xpath= 前缀可直接交给 Playwright。</p><p>运营商自定义下拉请先点触发器，再点一个选项；回放时 option_selector 只限定范围，实际按 {{ISP}} 文本匹配。</p></div>`);
  }

  let _revealedInputs = [];
  let _revealScrollHandler = null;
  function _scanDocForHiddenInputs(doc) {
    doc.querySelectorAll("input").forEach(el => {
      if (!isElementHidden(el) || ["submit", "button", "hidden"].includes(el.type)) return;
      el.dataset.caOrigDisplay = el.style.display || "";
      el.dataset.caOrigVisibility = el.style.visibility || "";
      el.dataset.caOrigOpacity = el.style.opacity || "";
      el.style.setProperty("display", "inline-block", "important");
      el.style.setProperty("visibility", "visible", "important");
      el.style.setProperty("opacity", "1", "important");
      el.classList.add("ca-revealed-highlight");
      const info = getElementInfo(el);
      _revealedInputs.push({ el, selector: selectorForPlayback(info.selectors[0]), inputType: el.type || "text", labelText: el.name || el.id || el.placeholder || el.type || "input" });
    });
  }
  function revealHiddenInputsForRecorder() {
    if (_revealedInputs.length) return;
    _scanDocForHiddenInputs(document);
    document.querySelectorAll("iframe, frame").forEach(frame => { try { if (frame.contentDocument) _scanDocForHiddenInputs(frame.contentDocument); } catch (_) {} });
    createRevealPanel(); setStatus(`👁️ 已显示 ${_revealedInputs.length} 个隐藏输入框`);
  }
  function hideRevealedInputs() {
    _revealedInputs.forEach(({ el }) => {
      el.style.display = el.dataset.caOrigDisplay || "";
      el.style.visibility = el.dataset.caOrigVisibility || "";
      el.style.opacity = el.dataset.caOrigOpacity || "";
      delete el.dataset.caOrigDisplay; delete el.dataset.caOrigVisibility; delete el.dataset.caOrigOpacity;
      el.classList.remove("ca-revealed-highlight");
    });
    _revealedInputs = [];
    document.getElementById("ca-reveal-panel")?.remove();
    document.querySelectorAll(".ca-reveal-popup").forEach(x => x.remove());
    if (_revealScrollHandler) { window.removeEventListener("scroll", _revealScrollHandler, true); _revealScrollHandler = null; }
  }
  function onRevealedClick(e) {
    if (!state.revealEnabled) return;
    const el = e.target.closest?.(".ca-revealed-highlight");
    if (!el) return;
    e.preventDefault(); e.stopImmediatePropagation(); showRevealPopup(el, e.clientX, e.clientY);
  }
  function showRevealPopup(el, x, y) {
    document.querySelectorAll(".ca-reveal-popup").forEach(p => p.remove());
    const info = getElementInfo(el);
    const selector = selectorForPlayback(info.selectors[0]);
    const popup = document.createElement("div");
    popup.className = "ca-reveal-popup";
    popup.style.left = Math.min(x, window.innerWidth - LIMITS.POPUP_MAX_WIDTH) + "px";
    popup.style.top = Math.min(y, window.innerHeight - 160) + "px";
    popup.innerHTML = `<div class="ca-rpop-header">${escHtml(selector)}</div><div class="ca-rpop-actions"><button data-type="username">👤 账号</button><button data-type="password">🔒 密码</button><button data-type="checkbox">☑️ 勾选</button><button data-type="click">👆 点击</button></div>`;
    document.body.appendChild(popup);
    popup.querySelectorAll("button").forEach(btn => btn.addEventListener("click", ev => {
      ev.stopPropagation();
      const type = btn.dataset.type;
      const step = buildStepBase(type, el, info, type === "password" ? "密码输入框 → {{PASSWORD}}" : type === "username" ? "账号输入框 → {{USERNAME}}" : `${type}: ${el.name || el.id || el.tagName}`);
      step._revealRecorded = true; commitStep(step); popup.remove();
      _revealedInputs = _revealedInputs.filter(item => item.el !== el); el.classList.remove("ca-revealed-highlight"); refreshRevealPanel();
    }));
  }
  function createRevealPanel() {
    document.getElementById("ca-reveal-panel")?.remove();
    const panel = document.createElement("div"); panel.id = "ca-reveal-panel"; panel.innerHTML = `<div class="ca-rv-header">👁️ 隐藏输入框 <span class="ca-rv-count">${_revealedInputs.length}</span></div><div id="ca-rv-list"></div>`; document.body.appendChild(panel); refreshRevealPanel();
  }
  function refreshRevealPanel() {
    const list = document.getElementById("ca-rv-list"); if (!list) return;
    list.innerHTML = _revealedInputs.map((r, i) => `<div class="ca-rv-item" data-idx="${i}"><div class="ca-rv-info"><div class="ca-rv-sel">${escHtml(r.selector)}</div><div class="ca-rv-type">type=${escHtml(r.inputType)} · ${escHtml(r.labelText)}</div></div><button class="ca-rv-btn">记录</button></div>`).join("");
    list.querySelectorAll(".ca-rv-item").forEach(row => row.addEventListener("click", () => { const item = _revealedInputs[parseInt(row.dataset.idx)]; if (item) { const r = item.el.getBoundingClientRect(); showRevealPopup(item.el, r.left, r.top); } }));
  }

  function escHtml(s) { return String(s ?? "").replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;").replace(/"/g, "&quot;"); }

  // ==================== 激活 / 停用 / 快捷键 ====================

  function activate() {
    if (state.active) return;
    state.active = true; createPanel(); updateRecordedList();
    document.addEventListener("mouseover", onHover, true);
    document.addEventListener("click", onRevealedClick, true);
    document.addEventListener("click", onClick, true);
    document.addEventListener("keydown", onKeyDown, true);
    attachAllFrameListeners(); startSpaFormWatcher(); domGuard.register(state.panel);
  }
  function deactivate() {
    if (!state.active) return;
    state.active = false; state.recording = false; state.carrierClickPhase = null;
    hideRevealedInputs();
    document.removeEventListener("mouseover", onHover, true);
    document.removeEventListener("click", onRevealedClick, true);
    document.removeEventListener("click", onClick, true);
    document.removeEventListener("keydown", onKeyDown, true);
    detachAllFrameListeners(); stopSpaFormWatcher(); hideTooltip();
    state.hoveredEl?.classList.remove("ca-highlight"); state.selectedEl?.classList.remove("ca-highlight-selected");
    if (state.panel) { domGuard.unregister(state.panel); state.panel.remove(); state.panel = null; }
  }
  function onKeyDown(e) {
    if (e.key === "Escape" && state.recording) {
      state.recording = false; state.currentStepType = null; state.carrierClickPhase = null; hideTooltip(); setStatus("已取消选择"); e.preventDefault();
    }
    if (e.key === "Enter" && state.recording && state.hoveredEl && state.currentStepType) {
      e.preventDefault(); e.stopPropagation();
      const el = state.hoveredEl; const info = getElementInfo(el);
      state.currentStepType === "carrier" ? handleCarrierClickPhase(el, info) : addStepFromElement(state.currentStepType, el, info, (el.textContent || "").trim().substring(0, 50) || STEP_TYPES[state.currentStepType]?.label || "");
      el.classList.remove("ca-highlight"); state.hoveredEl = null;
    }
    if (e.ctrlKey && e.shiftKey && e.key.toUpperCase() === "E") { state.active ? deactivate() : activate(); e.preventDefault(); }
  }

  // ==================== DOM 守护 ====================

  const domGuard = {
    _elems: new Set(), _observer: null, _interval: 0,
    register(el) { if (!el) return; this._elems.add(el); },
    unregister(el) { this._elems.delete(el); },
    _restoreAll() { const body = document.body; if (!body) return; this._elems.forEach(el => { if (el && !el.isConnected) body.appendChild(el); }); },
    start() {
      if (document.body && !this._observer) { this._observer = new MutationObserver(() => this._restoreAll()); this._observer.observe(document.body, { childList: true, subtree: true }); }
      if (!this._interval) this._interval = setInterval(() => this._restoreAll(), LIMITS.DOM_GUARD_INTERVAL_MS);
    },
  };

  const savedData = loadState();
  if (savedData) restoreFromSaved(savedData);

  const entryBtn = document.createElement("div");
  entryBtn.innerHTML = "🎬";
  entryBtn.title = "Campus-Auth 任务录制器 (Ctrl+Shift+E)";
  entryBtn.className = "ca-entry-btn";
  entryBtn.addEventListener("mouseenter", () => (entryBtn.style.transform = "scale(1.1)"));
  entryBtn.addEventListener("mouseleave", () => (entryBtn.style.transform = "scale(1)"));
  entryBtn.addEventListener("click", () => (state.active ? deactivate() : activate()));
  document.body.appendChild(entryBtn);
  domGuard.register(entryBtn);
  domGuard.start();
})();
