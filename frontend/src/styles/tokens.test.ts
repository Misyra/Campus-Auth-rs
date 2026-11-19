import { readFileSync, readdirSync, statSync } from "node:fs";
import { relative } from "node:path";
import { describe, expect, it } from "vitest";

/**
 * CSS 自定义属性（token）引用完整性守卫。
 *
 * P0-1 事故：HttpTestResult.vue 引用了全前端从未定义的 `--surface-subtle` 并带
 * 浅色兜底——深色主题下兜底浅灰永不跟随主题，chip 实测对比度 1.13:1，近乎不可读。
 * 类型检查与构建对 CSS 变量拼写零感知，只有把「引用 ⊆ 定义」钉成静态测试才能防回归。
 *
 * 定义集合（引用必须落在其中之一）：
 * ① styles/ 分层 CSS 的 `--x:` 声明（token 权威源）；
 * ② useAppearance.ts 的运行时主题注入（字符串字面量里的 token 名）；
 * ③ 各 .vue `<style>` 块内的本地声明（如面板按 AGENTS.md 范式声明 `--tsk-col-name-w`）；
 * ④ 显式 allowlist：第三方运行时注入（Reka UI 定位变量），逐项注明出处。
 */

const SRC = new URL("../", import.meta.url);

/** 递归收集目录下指定后缀的文件 */
function walk(dir: URL, ext: string, out: URL[] = []): URL[] {
  for (const name of readdirSync(dir)) {
    const p = new URL(name + (statSync(new URL(name, dir)).isDirectory() ? "/" : ""), dir);
    const s = statSync(p);
    if (s.isDirectory()) walk(p, ext, out);
    else if (p.pathname.endsWith(ext)) out.push(p);
  }
  return out;
}

/** 去掉 CSS 注释与 HTML 注释，避免注释里的示例代码污染声明/引用统计 */
function stripComments(text: string): string {
  return text.replace(/\/\*[\s\S]*?\*\//g, "").replace(/<!--[\s\S]*?-->/g, "");
}

/** 声明匹配：`--x:`，但排除 `::-webkit-scrollbar` 这类伪元素（`--` 前不能是冒号或词字符） */
const DECL_RE = /(^|[^:\w-])(--[A-Za-z0-9-]+)\s*:/g;
/** 引用匹配：`var(--x`（含带兜底的写法） */
const REF_RE = /var\(\s*(--[A-Za-z0-9-]+)/g;

function allMatches(text: string, re: RegExp): string[] {
  return [...text.matchAll(re)].map((m) => m[2] ?? m[1]);
}

const styleCssUrls = walk(new URL("./", SRC), ".css");
const vueUrls = walk(SRC, ".vue");

/** ① styles/ 分层 CSS 的 token 声明 */
const styleTokens = new Set<string>();
for (const url of styleCssUrls) {
  for (const name of allMatches(stripComments(readFileSync(url, "utf8")), DECL_RE)) styleTokens.add(name);
}

/** ② useAppearance.ts 运行时注入的 token（setProperty("----x", ...) 的字符串字面量） */
const runtimeTokens = new Set<string>();
const appearanceSrc = readFileSync(new URL("./composables/useAppearance.ts", SRC), "utf8");
for (const m of appearanceSrc.matchAll(/(['"`])(--[A-Za-z0-9-]+)\1/g)) runtimeTokens.add(m[2]);

/** ③ .vue `<style>` 块内的本地声明（scoped 覆盖通道，如 --tsk-col-name-w） */
const vueLocalTokens = new Set<string>();
for (const url of vueUrls) {
  const text = readFileSync(url, "utf8");
  for (const m of text.matchAll(/<style[^>]*>([\s\S]*?)<\/style>/g)) {
    for (const name of allMatches(stripComments(m[1]), DECL_RE)) vueLocalTokens.add(name);
  }
}

/** ④ 第三方运行时注入的 token：Reka UI 定位变量按碰撞实测写入（form.css 注释有说明） */
const THIRD_PARTY_TOKENS = new Set(["--reka-tooltip-content-available-width"]);

const knownTokens = new Set([...styleTokens, ...runtimeTokens, ...vueLocalTokens, ...THIRD_PARTY_TOKENS]);

/** 收集某文件里的全部 var(--x) 引用（style 块 + 模板，注释剔除） */
function collectVarRefs(url: URL): { file: string; token: string }[] {
  const text = stripComments(readFileSync(url, "utf8"));
  return allMatches(text, REF_RE).map((token) => ({ file: relative(SRC.pathname, url.pathname), token }));
}

describe("CSS token 引用完整性（P0-1 守卫）", () => {
  it("定义集合非空且规模合理（收集逻辑失效时此处先炸）", () => {
    expect(styleTokens.size).toBeGreaterThan(100);
    expect(runtimeTokens.size).toBeGreaterThan(10);
  });

  it("styles/ 分层 CSS 的 var(--x) 引用全部可解析", () => {
    const dangling: string[] = [];
    for (const url of styleCssUrls) {
      for (const { file, token } of collectVarRefs(url)) {
        if (!knownTokens.has(token)) dangling.push(`${file} 引用 ${token}`);
      }
    }
    expect(dangling, "存在未定义的 CSS 变量引用（补定义或进 allowlist 并注明出处）").toEqual([]);
  });

  it("全部 .vue（scoped 样式与模板）的 var(--x) 引用全部可解析", () => {
    const dangling: string[] = [];
    for (const url of vueUrls) {
      for (const { file, token } of collectVarRefs(url)) {
        if (!knownTokens.has(token)) dangling.push(`${file} 引用 ${token}`);
      }
    }
    expect(dangling, "存在未定义的 CSS 变量引用（补定义或进 allowlist 并注明出处）").toEqual([]);
  });

  it("P0-1 回归钉：--surface-subtle 不得在任何文件出现（浅色兜底不随主题，深色 1.13:1）", () => {
    for (const url of [...styleCssUrls, ...vueUrls]) {
      const text = stripComments(readFileSync(url, "utf8"));
      expect(text, relative(SRC.pathname, url.pathname)).not.toContain("--surface-subtle");
    }
  });

  it("HttpTestResult 变量徽标走主题安全写法（rgba(--slate-rgb) + 显式文字色）", () => {
    const src = readFileSync(new URL("./components/common/HttpTestResult.vue", SRC), "utf8");
    const rule = src.match(/\.http-test-produced-vars code\s*\{([^}]*)\}/);
    expect(rule, "缺少 .http-test-produced-vars code 规则").not.toBeNull();
    expect(rule![1]).toContain("rgba(var(--slate-rgb)");
    expect(rule![1]).toContain("color: var(--text-primary)");
  });
});
