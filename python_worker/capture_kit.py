"""页面捕获工具箱：结构化控件摘要与 CSS/JS 资源快照（WorkerCore 拆分）。

自 playwright_worker.py 迁出。两条产物链路共用本工具箱：

- ``_capture_page_structure``：提取适合模型消费的页面结构（跨 shadow DOM 的
  控件/表单/验证码候选 + 脱敏局部 HTML），供 AI 任务生成；
- ``_capture_page_resources``：抓取页面 CSS/JS 资源落盘——CDP 路径（Chromium 系，
  内容与执行时一致）优先，页面枚举 + HTTP 回补（引擎无关）兜底；配套
  ``_rewrite_resource_urls`` 把 HTML 引用改写为本地相对路径，供离线副本还原。

消费者为 capture_commands（page_capture / feedback_capture）。渠道判定
``_channel_supports_cdp`` 属 browser_launch（启动参数过滤共用同一口径）。
"""

from __future__ import annotations

import base64
import hashlib
import logging
from html import escape as _html_escape
from pathlib import Path
from typing import Any, Iterable

from browser_launch import _channel_supports_cdp

logger = logging.getLogger(__name__)

# ── 资源快照（feedback_capture / page_capture 的 CSS/JS 落盘辅助）──

#: MIME → 扩展名（仅覆盖资源快照关注的类型，其余按 txt 兜底）
_RESOURCE_EXT_BY_MIME = {
    "text/css": "css",
    "application/javascript": "js",
    "text/javascript": "js",
    "application/x-javascript": "js",
}

#: 单文件与总量上限：防资源列表异常的页面把反馈包撑到不可分发
_RESOURCE_MAX_FILES = 200
_RESOURCE_MAX_BYTES = 5 * 1024 * 1024
_RESOURCE_MAX_TOTAL_BYTES = 50 * 1024 * 1024

#: HTTP 回补单资源超时：资源通常与页面同源、缓存友好，给 10s 足够
_RESOURCE_FETCH_TIMEOUT_MS = 10_000

# ── 结构提取上限 ──

_STRUCTURE_MAX_CONTROLS = 300
_STRUCTURE_MAX_LOCAL_HTML = 40_000
_CAPTURE_MAX_SCREENSHOT_BYTES = 12 * 1024 * 1024


async def _capture_page_structure(page: Any) -> dict[str, Any]:
    """提取适合模型消费的页面结构，并保留脱敏局部 HTML。

    原始 input value、textarea 内容和 token 类隐藏字段不会进入摘要；局部 HTML
    仍保留标签层级与属性，作为稳定 selector 判断的必要材料。
    """
    script = r"""
    (limits) => {
      const visible = (el) => {
        const s = getComputedStyle(el); const r = el.getBoundingClientRect();
        return s.display !== 'none' && s.visibility !== 'hidden' && Number(s.opacity) !== 0 && r.width > 0 && r.height > 0;
      };
      const escAttr = (v) => String(v || '').replace(/\\/g, '\\\\').replace(/"/g, '\\"');
      const roots = [document];
      for (let i = 0; i < roots.length; i++) {
        for (const el of roots[i].querySelectorAll('*')) if (el.shadowRoot) roots.push(el.shadowRoot);
      }
      const all = (selector) => roots.flatMap((root) => Array.from(root.querySelectorAll(selector)));
      const unique = (selector) => { try { return all(selector).length === 1; } catch (_) { return false; } };
      const selectorCandidates = (el) => {
        const out = [];
        if (el.id) { const s = '#' + CSS.escape(el.id); if (unique(s)) out.push(s); }
        for (const a of ['data-testid', 'data-test', 'name', 'autocomplete', 'placeholder']) {
          const v = el.getAttribute(a); if (!v) continue;
          const s = `${el.localName}[${a}="${escAttr(v)}"]`; if (unique(s)) out.push(s);
        }
        const type = el.getAttribute('type');
        if (type) { const s = `${el.localName}[type="${escAttr(type)}"]`; if (unique(s)) out.push(s); }
        if (!out.length) {
          const parts = []; let node = el;
          while (node && node.nodeType === 1 && parts.length < 5) {
            let part = node.localName;
            if (node.id) { part += '#' + CSS.escape(node.id); parts.unshift(part); break; }
            const siblings = node.parentElement ? Array.from(node.parentElement.children).filter(x => x.localName === node.localName) : [];
            if (siblings.length > 1) part += `:nth-of-type(${siblings.indexOf(node) + 1})`;
            parts.unshift(part); node = node.parentElement;
          }
          if (parts.length) out.push(parts.join(' > '));
        }
        return out.slice(0, 4);
      };
      const labelFor = (el) => {
        const aria = el.getAttribute('aria-label'); if (aria) return aria.trim();
        if (el.labels?.length) return Array.from(el.labels).map(x => x.innerText || x.textContent || '').join(' ').trim();
        const labelled = el.getAttribute('aria-labelledby');
        if (labelled) return labelled.split(/\s+/).map(id => document.getElementById(id)?.textContent || '').join(' ').trim();
        return (el.closest('label')?.innerText || '').trim();
      };
      const controls = all('input,button,select,textarea').slice(0, limits.controls).map((el, index) => ({
        index, tag: el.localName, type: el.getAttribute('type') || '', id: el.id || '', name: el.getAttribute('name') || '',
        label: labelFor(el).slice(0, 200), placeholder: (el.getAttribute('placeholder') || '').slice(0, 200),
        text: (el.innerText || el.textContent || '').trim().slice(0, 200),
        autocomplete: el.getAttribute('autocomplete') || '', required: !!el.required, disabled: !!el.disabled,
        visible: visible(el), selectors: selectorCandidates(el),
        options: el.localName === 'select' ? Array.from(el.options).slice(0, 50).map(o => ({value: o.value, text: (o.textContent || '').trim().slice(0, 200)})) : undefined
      }));
      const forms = all('form').slice(0, 50).map((form, index) => ({
        index, id: form.id || '', name: form.getAttribute('name') || '', method: (form.method || 'get').toLowerCase(),
        action: form.action || '', visible: visible(form), selectors: selectorCandidates(form)
      }));
      const captcha = all('img,input,canvas').filter((el) => {
        const hay = [el.id, el.className, el.getAttribute('name'), el.getAttribute('alt'), el.getAttribute('placeholder'), el.getAttribute('src')].join(' ').toLowerCase();
        return /captcha|verify|valid|checkcode|验证码|校验码/.test(hay);
      }).slice(0, 30).map(el => ({tag: el.localName, id: el.id || '', name: el.getAttribute('name') || '', selectors: selectorCandidates(el)}));
      const sources = forms.length ? all('form') : (() => {
        const pwd = all('input[type="password"]')[0]; return pwd ? [pwd.closest('section,main,div') || pwd.parentElement || pwd] : [];
      })();
      const localHtml = sources.slice(0, 20).map((source) => {
        const clone = source.cloneNode(true);
        for (const el of clone.querySelectorAll('input')) {
          el.removeAttribute('value');
          if (/token|secret|password|pwd/i.test((el.getAttribute('name') || '') + ' ' + (el.id || ''))) el.setAttribute('value', '[REDACTED]');
        }
        for (const el of clone.querySelectorAll('textarea')) el.textContent = '';
        for (const el of clone.querySelectorAll('script,style,noscript')) el.remove();
        return clone.outerHTML;
      }).join('\n').slice(0, limits.localHtml);
      return { forms, controls, captcha_candidates: captcha, has_shadow_dom: roots.length > 1, local_html: localHtml };
    }
    """
    frames: list[dict[str, Any]] = []
    page_frames = list(page.frames)
    for index, frame in enumerate(page_frames):
        frame_ref = "main" if frame == page.main_frame else (frame.name or f"frame-{index}")
        parent = frame.parent_frame
        parent_ref = None
        if parent is not None:
            try:
                parent_index = page_frames.index(parent)
            except ValueError:
                parent_index = -1
            parent_ref = (
                "main"
                if parent == page.main_frame
                else (parent.name or (f"frame-{parent_index}" if parent_index >= 0 else "detached"))
            )
        try:
            detail = await frame.evaluate(
                script,
                {"controls": _STRUCTURE_MAX_CONTROLS, "localHtml": _STRUCTURE_MAX_LOCAL_HTML},
            )
        except Exception as exc:  # noqa: BLE001 — 跨域/销毁中的 frame 可单独跳过
            detail = {"forms": [], "controls": [], "captcha_candidates": [], "local_html": "", "error": str(exc)[:300]}
        frames.append({"ref": frame_ref, "parent": parent_ref, "name": frame.name, "url": frame.url, **detail})
    remaining_html = _STRUCTURE_MAX_LOCAL_HTML
    for frame in frames:
        local_html = str(frame.get("local_html") or "")[:remaining_html]
        frame["local_html"] = local_html
        remaining_html = max(0, remaining_html - len(local_html))
    return {"version": 1, "frames": frames}


def _resource_ext(mime: str, kind: str = "") -> str:
    """按 MIME 推导资源文件扩展名；MIME 缺失时按资源种类兜底，最后才落 txt。

    回补路径拿到的 ``Content-Type`` 可能是通用的 ``application/octet-stream``，
    此时用枚举阶段已知的 script/stylesheet 种类仍能给出可用的扩展名。
    """
    key = (mime or "").split(";")[0].strip().lower()
    ext = _RESOURCE_EXT_BY_MIME.get(key)
    if ext:
        return ext
    if kind == "script":
        return "js"
    if kind == "stylesheet":
        return "css"
    return "txt"


def _url_scheme_variants(url: str) -> list[str]:
    """派生同一资源的 URL 形态：绝对 https/http 与协议相对 `//host/path`。

    DOM 属性里常见协议相对写法（`//s1.example.com/x.js`），而 CDP 资源树
    上报绝对 URL，逐形态替换才能把引用全部改写到本地文件。
    绝对形态在前、协议相对在后：先把长的替换掉，剩余的 `//` 前缀才是
    真正的协议相对用法（避免 `http://` 内含 `//` 被二次误替换）。
    """
    if url.startswith("https://"):
        rest = url[len("https://") :]
        return [url, f"http://{rest}", f"//{rest}"]
    if url.startswith("http://"):
        rest = url[len("http://") :]
        return [url, f"https://{rest}", f"//{rest}"]
    return [url]


def _rewrite_resource_urls(html: str, mapping: dict[str, str]) -> str:
    """把 HTML 中出现的资源 URL 改写为本地相对路径。

    URL 在 HTML 属性里可能是原样形态，也可能是 `&amp;` 转义形态，两种都替换；
    简单字符串替换可能误伤 JS 字符串中的同 URL 文本，对离线还原无实际影响。
    """
    for url, local in mapping.items():
        if not url:
            continue
        for variant in _url_scheme_variants(url):
            html = html.replace(variant, local)
            escaped = _html_escape(variant, quote=True)
            if escaped != variant:
                html = html.replace(escaped, local)
    return html


class _ResourceSink:
    """资源落盘收集器：CDP 快照与 HTTP 回补共用同一套上限与命名规则。

    文件名单调哈希命名（URL sha1 前 12 位 + 扩展名），两条路径写到同一目录也
    不会互相覆盖；总量/文件数上限在收集器内统一判定，避免某条路径绕过预算
    把捕获包撑爆。
    """

    def __init__(self, target_dir: Path) -> None:
        self.target_dir = target_dir
        self.saved: dict[str, str] = {}
        #: 资源在 HTML 里的原始书写形态 → 本地路径（改写引用时按文本替换）
        self.aliases: dict[str, str] = {}
        self.total_bytes = 0
        #: 触发上限时的说明；非 None 即表示后续资源不再收录
        self.note: str | None = None

    def admit(self) -> bool:
        """是否还能继续收录；触顶后统一返回 False（说明只在首次触顶时写入）。"""
        if self.note is not None:
            return False
        if len(self.saved) >= _RESOURCE_MAX_FILES:
            self.note = f"资源数超过 {_RESOURCE_MAX_FILES}，其余跳过"
            return False
        return True

    def alias(self, url: str, raw: str) -> None:
        """登记资源在 HTML 里的原始书写形态（内容可能已由 CDP 路径落盘）。

        枚举阶段必须无条件登记：CDP 已拿到正文的资源不会再走回补，其相对写法
        只能在这里补上，否则离线副本改不动引用。
        """
        local = self.saved.get(url)
        if local and raw and raw != url:
            self.aliases.setdefault(raw, local)

    def store(
        self,
        url: str,
        data: bytes,
        mime: str,
        kind: str = "",
        aliases: Iterable[str] = (),
    ) -> bool:
        """写入单个资源并建映射；空内容/超单文件/超总量返回 False。

        ``aliases`` 是同一资源在 HTML 文本里的其它书写形态（相对路径、
        协议相对等）。改写 HTML 时按文本替换，只有绝对 URL 是匹配不上的。
        """
        if not data or len(data) > _RESOURCE_MAX_BYTES:
            return False
        if self.total_bytes + len(data) > _RESOURCE_MAX_TOTAL_BYTES:
            if self.note is None:
                self.note = f"资源总量超 {_RESOURCE_MAX_TOTAL_BYTES // (1024 * 1024)}MiB，已截断"
            return False
        name = (
            f"{hashlib.sha1(url.encode('utf-8')).hexdigest()[:12]}"
            f".{_resource_ext(mime, kind)}"
        )
        self.target_dir.mkdir(parents=True, exist_ok=True)
        (self.target_dir / name).write_bytes(data)
        local = f"resources/{name}"
        self.saved[url] = local
        for alias in aliases:
            if alias and alias != url:
                self.aliases.setdefault(alias, local)
        self.total_bytes += len(data)
        return True


def _iter_cdp_frame_resources(frame_tree: Any) -> list[tuple[str, list]]:
    """递归收集 CDP frameTree 中各 frame 的 (frameId, resources) 对。

    资源树以主 frame 为根、``childFrames`` 挂子 frame；iframe 门户（登录表单
    全在 iframe 里）的 CSS/JS 都在子 frame 的 ``resources`` 里，只取根节点
    会让离线副本缺资源。畸形节点（非 dict / 缺 id）容错跳过。
    """
    out: list[tuple[str, list]] = []
    if not isinstance(frame_tree, dict):
        return out
    frame_id = str((frame_tree.get("frame") or {}).get("id") or "")
    entries = frame_tree.get("resources") or []
    if isinstance(entries, list):
        out.append((frame_id, entries))
    for child in frame_tree.get("childFrames") or []:
        out.extend(_iter_cdp_frame_resources(child))
    return out


async def _cdp_resource_snapshot(page: Any, sink: _ResourceSink) -> None:
    """经 CDP 抓取全部 frame（含 iframe）已加载的 Script/Stylesheet 资源并写入 sink。

    逐项容错：缓存已逐出/取回失败的单个资源跳过，不中断整体快照。CDP 内容与
    页面实际执行的版本一致，优先级高于 HTTP 回补，故先跑这条路径。
    """
    cdp = await page.context.new_cdp_session(page)
    try:
        # getResourceContent 要求本会话启用 Page 域（Playwright 的 CDP 会话
        # 不会自动启用，缺省时报 "Agent is not enabled"）
        await cdp.send("Page.enable")
        tree = await cdp.send("Page.getResourceTree")
        for frame_id, entries in _iter_cdp_frame_resources(tree.get("frameTree", {})):
            for res in entries:
                if not sink.admit():
                    return
                if not isinstance(res, dict):
                    continue
                rtype = (res.get("type") or "").lower()
                if rtype not in ("stylesheet", "script"):
                    continue
                url = res.get("url") or ""
                if not url.startswith(("http://", "https://")) or url in sink.saved:
                    continue
                try:
                    got = await cdp.send(
                        "Page.getResourceContent", {"frameId": frame_id, "url": url}
                    )
                except Exception:  # noqa: BLE001 — 缓存逐出等，逐项跳过
                    continue
                if got.get("base64Encoded"):
                    data = base64.b64decode(got.get("content") or "")
                else:
                    data = (got.get("content") or "").encode("utf-8")
                sink.store(url, data, res.get("mimeType") or "", rtype)
    finally:
        await cdp.detach()


#: 页面内枚举已加载的 script/stylesheet URL（引擎无关，不依赖 CDP）
#:
#: 两条来源互补：DOM 属性覆盖静态引用（含尚未进入缓存的），Performance 资源
#: 条目覆盖导航后动态插入/预加载的请求。返回 [绝对URL, 属性的原始书写形态, 种类]
#: 三元组：原始形态用于把 HTML 里的引用改成 resources/ 相对路径——HTML 文本里常见
#: 相对写法（`static/css/style.css`、`/css/a.css`），只有绝对 URL 是替换不掉的。
_PAGE_RESOURCE_PROBE_JS = r"""
() => {
  const out = [];
  const seen = new Set();
  const push = (url, raw, kind) => {
    if (!url || url.startsWith('data:') || seen.has(url)) return;
    seen.add(url);
    out.push([url, raw || '', kind]);
  };
  for (const el of document.querySelectorAll('script[src]')) {
    push(el.src, el.getAttribute('src'), 'script');
  }
  for (const el of document.querySelectorAll('link[href]')) {
    const rel = (el.getAttribute('rel') || '').toLowerCase();
    const as = (el.getAttribute('as') || '').toLowerCase();
    if (rel.includes('stylesheet') || (rel.includes('preload') && as === 'style')) {
      push(el.href, el.getAttribute('href'), 'stylesheet');
    }
  }
  try {
    for (const entry of performance.getEntriesByType('resource')) {
      const type = (entry.initiatorType || '').toLowerCase();
      if (type === 'script') push(entry.name, '', 'script');
      else if (type === 'link' || type === 'css' || type === 'style') push(entry.name, '', 'stylesheet');
    }
  } catch (_) {}
  return out;
}
"""


async def _http_resource_snapshot(page: Any, sink: _ResourceSink) -> str | None:
    """引擎无关兜底：逐 frame 枚举已加载的 script/stylesheet，按 URL 回补正文。

    CDP 不可用（firefox / webkit）时这是唯一能拿到 CSS/JS 正文的路径；Chromium
    下作为补充，捞 CDP 内存缓存已逐出或导航后才插入的资源。按 ``page.frames``
    逐 frame 执行枚举脚本并按 URL 去重汇总，iframe 门户的资源才能进离线副本；
    单个 frame（跨域/销毁中）枚举失败跳过，全部失败才报枚举不可用。
    ``context.request`` 走浏览器网络栈（带 context 的 cookie / UA），逐项容错，
    返回说明文本或 None。
    """
    # 按 URL 去重汇总各 frame 的枚举结果（首见优先，保留原始书写形态）
    entries_all: list[Any] = []
    seen: set[str] = set()
    first_error = ""
    frames = list(getattr(page, "frames", None) or [])
    if not frames:
        # 测试替身兼容：无 frames 属性时按单页处理
        frames = [page]
    for frame in frames:
        try:
            entries = await frame.evaluate(_PAGE_RESOURCE_PROBE_JS)
        except Exception as exc:  # noqa: BLE001 — 跨域/销毁中的 frame 单独跳过
            if not first_error:
                first_error = str(exc)
            continue
        for entry in entries or []:
            if not isinstance(entry, (list, tuple)) or not entry:
                continue
            url = str(entry[0] or "")
            if not url or url in seen:
                continue
            seen.add(url)
            entries_all.append(entry)
    if not entries_all and first_error:
        # 所有 frame 枚举都失败，本兜底整体不可用
        return f"页面资源枚举失败: {first_error}"
    fetched = 0
    failed = 0
    first_fetch_error = ""
    for entry in entries_all:
        if not sink.admit():
            break
        url = str(entry[0] or "")
        raw = str(entry[1] or "") if len(entry) > 1 else ""
        kind = str(entry[2] or "") if len(entry) > 2 else ""
        if not url.startswith(("http://", "https://")):
            continue
        if url in sink.saved:
            # 正文已有（CDP 路径或更早的条目）：只补登记 HTML 里的原始写法
            sink.alias(url, raw)
            continue
        try:
            resp = await page.context.request.get(url, timeout=_RESOURCE_FETCH_TIMEOUT_MS)
            if not resp.ok:
                failed += 1
                if not first_fetch_error:
                    first_fetch_error = f"HTTP {resp.status}"
                continue
            data = await resp.body()
            mime = resp.headers.get("content-type", "")
        except Exception as exc:  # noqa: BLE001 — 单个资源取回失败不中断整体
            failed += 1
            if not first_fetch_error:
                first_fetch_error = str(exc)[:120]
            continue
        if sink.store(url, data, mime, kind, aliases=[raw]):
            fetched += 1
    if failed:
        return f"{fetched} 个资源经 HTTP 回补成功，{failed} 个失败（首个原因: {first_fetch_error}）"
    return None


async def _capture_page_resources(
    page: Any, target_dir: Path, *, bs: dict
) -> tuple[dict[str, str], dict[str, str], str | None]:
    """抓取页面 CSS/JS 资源并落盘到 target_dir。

    返回 ``(saved, aliases, note)``：``saved`` 为 绝对URL → resources/<name>
    映射，``aliases`` 为资源在 HTML 里的原始书写形态 → 同一路径，两者合并后交给
    ``_rewrite_resource_urls`` 才能把 HTML 引用全部改到本地（相对路径写法只能靠
    aliases 命中）。``note`` 为降级/截断说明或 None。

    两条抓取路径：CDP（Chromium 系，内容与执行时一致）优先，页面枚举 + HTTP 回补
    （引擎无关）兜底补缺。非 Chromium 渠道 CDP 在协议层就不存在，此处跳过并按渠道
    给出可操作说明，而不是把 Playwright 的英文异常直接甩给用户。
    """
    sink = _ResourceSink(target_dir)
    notes: list[str] = []
    channel = str((bs or {}).get("browser_channel") or "playwright").strip().lower()
    if _channel_supports_cdp(bs):
        try:
            await _cdp_resource_snapshot(page, sink)
        except Exception as exc:  # noqa: BLE001 — 回退 HTTP 回补
            notes.append(f"CDP 资源快照不可用: {exc}")
    else:
        notes.append(
            f"当前浏览器渠道 {channel} 不支持 CDP：MHTML 完整布局快照不可用，"
            "CSS/JS 已改用页面枚举 + HTTP 回补抓取；需要完整快照请把浏览器渠道"
            "切换为 Chromium / Chrome / Edge 后重新捕获"
        )
    try:
        fallback_note = await _http_resource_snapshot(page, sink)
    except Exception as exc:  # noqa: BLE001 — 兜底整体失败不影响已落盘的 CDP 产物
        fallback_note = f"资源回补失败: {exc}"
    if fallback_note:
        notes.append(fallback_note)
    if sink.note:
        notes.append(sink.note)
    return sink.saved, sink.aliases, "；".join(notes) if notes else None


__all__ = [
    "_PAGE_RESOURCE_PROBE_JS",
    "_ResourceSink",
    "_STRUCTURE_MAX_CONTROLS",
    "_STRUCTURE_MAX_LOCAL_HTML",
    "_capture_page_resources",
    "_capture_page_structure",
    "_iter_cdp_frame_resources",
    "_resource_ext",
    "_rewrite_resource_urls",
    "_url_scheme_variants",
]
