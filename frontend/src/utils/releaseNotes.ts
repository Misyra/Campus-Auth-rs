/**
 * GitHub Release 正文（Markdown）→ 安全 HTML 的最小渲染器。
 *
 * 只覆盖发布说明实际用到的语法（标题 / 无序与有序列表 / 引用 / 分隔线 / 围栏代码块 /
 * 加粗 / 斜体 / 行内代码 / 链接与裸链接）。正文来自远端仓库，为此引入
 * `marked` + `DOMPurify` 两条依赖只为渲染一页发布日志并不划算。
 *
 * **安全**：所有文本在拼接进标签前一律经 [`escapeHtml`] 转义，正文里的原生 HTML
 * 标签会作为纯文本显示；链接只接受 `http/https/mailto`，其余（含 `javascript:`）
 * 一律降级为纯文本。因此调用方可以安全地对返回值使用 `v-html`。
 *
 * 已知边界（发布说明里不会出现，故不实现）：行内语法不支持互相嵌套
 * （`**加粗里的 \`代码\`**` 会按字面输出）、列表不支持嵌套层级（深缩进项按同级渲染）。
 */

/** HTML 转义：先于任何标签拼接调用，杜绝正文里的原生标签穿透 */
export function escapeHtml(text: string): string {
  return text
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;")
    .replace(/'/g, "&#39;");
}

/** 链接白名单：只放行 http/https/mailto，其余返回 null（调用方按纯文本输出） */
export function safeHref(raw: string): string | null {
  const url = raw.trim();
  return /^(?:https?:\/\/|mailto:)/i.test(url) ? url : null;
}

/** 渲染一个链接；协议不合法时退化为纯文本，避免 href 注入 */
function renderLink(label: string, href: string): string {
  const safe = safeHref(href);
  if (!safe) return escapeHtml(label);
  return `<a href="${escapeHtml(safe)}" target="_blank" rel="noopener noreferrer">${escapeHtml(label)}</a>`;
}

/**
 * 行内语法匹配（顺序即优先级）：行内代码 / 链接 / 加粗 / 斜体 / 裸链接。
 *
 * 用 `exec` 逐段推进而非 `replace`：命中之间的普通文本必须各自独立转义，
 * `replace` 拿不到这些片段，容易漏转义。
 */
const INLINE_PATTERN =
  /(`[^`\n]+`)|(\[[^\]\n]+\]\([^\s)]+\))|(\*\*[^*\n]+\*\*)|(__[^_\n]+__)|(\*[^*\n]+\*)|(_[^_\n]+_)|(https?:\/\/[^\s<>()]+)/g;

/** 单行文本 → 行内 HTML（普通片段逐个转义，语法片段按类型生成标签） */
export function renderInline(text: string): string {
  let out = "";
  let cursor = 0;
  INLINE_PATTERN.lastIndex = 0;
  for (let match = INLINE_PATTERN.exec(text); match; match = INLINE_PATTERN.exec(text)) {
    out += escapeHtml(text.slice(cursor, match.index));
    const token = match[0];
    if (token.startsWith("`")) {
      out += `<code>${escapeHtml(token.slice(1, -1))}</code>`;
    } else if (token.startsWith("[")) {
      const split = token.indexOf("](");
      out += renderLink(token.slice(1, split), token.slice(split + 2, -1));
    } else if (token.startsWith("**") || token.startsWith("__")) {
      out += `<strong>${escapeHtml(token.slice(2, -2))}</strong>`;
    } else if (token.startsWith("http")) {
      out += renderLink(token, token);
    } else {
      out += `<em>${escapeHtml(token.slice(1, -1))}</em>`;
    }
    cursor = match.index + token.length;
  }
  return out + escapeHtml(text.slice(cursor));
}

const HEADING = /^(#{1,6})\s+(.*)$/;
const BULLET = /^[-*+]\s+(.*)$/;
const ORDERED = /^\d+[.)]\s+(.*)$/;
const QUOTE = /^>\s?(.*)$/;
const RULE = /^(?:-{3,}|\*{3,}|_{3,})$/;
const FENCE = /^```/;

/** 发布流程（`release.yml`）写在正文开头的模板化标题 */
const TEMPLATE_TITLE = /^\s*##\s+更新日志\s*\n/;

/**
 * 去掉发布说明开头的模板标题「## 更新日志」。
 *
 * 弹窗上方已经标了「更新日志」，正文再出现一次标题纯属重复；该标题由发布流程
 * 固定生成，去掉不会误伤用户自己写的正文。
 */
export function stripTemplateTitle(markdown: string | null | undefined): string {
  return String(markdown ?? "").replace(TEMPLATE_TITLE, "");
}

/** Release 正文 → HTML 片段（块级解析：逐行归块，块内走 [`renderInline`]） */
export function renderReleaseNotes(markdown: string | null | undefined): string {
  const lines = String(markdown ?? "").replace(/\r\n?/g, "\n").split("\n");
  const blocks: string[] = [];
  let paragraph: string[] = [];
  let quote: string[] = [];
  let list: { ordered: boolean; items: string[] } | null = null;

  const flushParagraph = () => {
    if (paragraph.length) {
      // 段内单个换行按可见换行处理：发布说明常按行断句，压成一行反而难读
      blocks.push(`<p>${paragraph.map(renderInline).join("<br>")}</p>`);
      paragraph = [];
    }
  };
  const flushQuote = () => {
    if (quote.length) {
      blocks.push(`<blockquote>${quote.map(renderInline).join("<br>")}</blockquote>`);
      quote = [];
    }
  };
  const flushList = () => {
    if (list) {
      const tag = list.ordered ? "ol" : "ul";
      blocks.push(`<${tag}>${list.items.map((item) => `<li>${renderInline(item)}</li>`).join("")}</${tag}>`);
      list = null;
    }
  };
  const flushAll = () => {
    flushParagraph();
    flushQuote();
    flushList();
  };

  for (let i = 0; i < lines.length; i += 1) {
    const trimmed = lines[i].trim();

    if (FENCE.test(trimmed)) {
      flushAll();
      const code: string[] = [];
      i += 1;
      while (i < lines.length && !FENCE.test(lines[i].trim())) {
        code.push(lines[i]);
        i += 1;
      }
      blocks.push(`<pre><code>${escapeHtml(code.join("\n"))}</code></pre>`);
      continue;
    }
    if (!trimmed) {
      flushAll();
      continue;
    }
    if (RULE.test(trimmed)) {
      flushAll();
      blocks.push("<hr>");
      continue;
    }

    const heading = HEADING.exec(trimmed);
    if (heading) {
      flushAll();
      // 标题整体下沉三级：弹窗自身已有标题层级，正文从 h4 起避免与对话框标题争层级
      const level = Math.min(6, heading[1].length + 3);
      blocks.push(`<h${level}>${renderInline(heading[2].trim())}</h${level}>`);
      continue;
    }

    const bullet = BULLET.exec(trimmed);
    if (bullet) {
      flushParagraph();
      flushQuote();
      if (list?.ordered) flushList();
      if (!list) list = { ordered: false, items: [] };
      list.items.push(bullet[1].trim());
      continue;
    }

    const ordered = ORDERED.exec(trimmed);
    if (ordered) {
      flushParagraph();
      flushQuote();
      if (list && !list.ordered) flushList();
      if (!list) list = { ordered: true, items: [] };
      list.items.push(ordered[1].trim());
      continue;
    }

    const quoted = QUOTE.exec(trimmed);
    if (quoted) {
      flushParagraph();
      flushList();
      quote.push(quoted[1].trim());
      continue;
    }

    flushQuote();
    flushList();
    paragraph.push(trimmed);
  }

  flushAll();
  return blocks.join("");
}
