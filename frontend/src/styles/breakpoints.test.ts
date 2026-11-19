import { readFileSync, readdirSync, statSync } from "node:fs";
import { relative } from "node:path";
import { describe, expect, it } from "vitest";

/**
 * 断点约定守卫。
 *
 * 全仓 @media 宽度条件的权威出处是 index.css 文件头的「断点约定清单」：主轴
 * 640 / 768 / 900 / 1100 四档（base.css 的 --bp-sm/md/lg/xl 同值）+ 360 / 1800
 * 两个特例档。媒体查询条件不能引用 var()，只能写字面值——字面值一旦手滑写偏
 * （850、700、600 这类「看着差不多」的值），同一语义档的降级时机就会在不同页面
 * 悄悄错开，类型检查与构建对此零感知，只有静态测试能拦。
 */

const SRC = new URL("../", import.meta.url);

/** 允许出现的宽度断点值（与 index.css 断点约定清单一一对应，新增须先改清单） */
const ALLOWED_BP = new Set([360, 640, 768, 900, 1100, 1800]);
/** 特例档只允许出现在约定清单点名的文件（防止清单 4 档 + 暗地散值） */
const EXCEPTIONAL_BP_FILES: Record<number, Set<string>> = {
  360: new Set(["styles/responsive.css"]),
  1800: new Set(["components/common/HttpFlowFields.vue"]),
};

/** 递归收集目录下指定后缀的文件（与 tokens.test.ts 同一实现口径） */
function walk(dir: URL, ext: string, out: URL[] = []): URL[] {
  for (const name of readdirSync(dir)) {
    const p = new URL(name + (statSync(new URL(name, dir)).isDirectory() ? "/" : ""), dir);
    const s = statSync(p);
    if (s.isDirectory()) walk(p, ext, out);
    else if (p.pathname.endsWith(ext)) out.push(p);
  }
  return out;
}

/** 去掉 CSS / HTML 注释：文件头清单、迁移历史里的示例数值不应参与扫描 */
function stripComments(text: string): string {
  return text.replace(/\/\*[\s\S]*?\*\//g, "").replace(/<!--[\s\S]*?-->/g, "");
}

interface BpUsage {
  file: string;
  /** 条件形态：媒体查询（media）或 JS matchMedia（js） */
  kind: "media" | "js";
  minmax: "min" | "max";
  value: number;
}

/** 从一段 CSS 里取 @media 前导符中的全部 (min|max)-width: Npx 条件 */
function scanMediaWidths(css: string, file: string, out: BpUsage[]): void {
  for (const m of css.matchAll(/@media[^{]*/g)) {
    for (const w of m[0].matchAll(/\((min|max)-width:\s*(\d+)px\)/g)) {
      out.push({ file, kind: "media", minmax: w[1] as "min" | "max", value: Number(w[2]) });
    }
  }
}

const cssUrls = walk(new URL("./styles/", SRC), ".css");
const vueUrls = walk(SRC, ".vue");
const tsUrls = walk(SRC, ".ts").filter((u) => !u.pathname.endsWith(".d.ts"));

const usages: BpUsage[] = [];
for (const url of [...cssUrls, ...vueUrls]) {
  const file = relative(SRC.pathname, url.pathname);
  let text = stripComments(readFileSync(url, "utf8"));
  if (file.endsWith(".vue")) {
    // 只扫 <style> 块做媒体查询；模板/脚本里的 @media 是普通字符串，不构成查询
    const styles = [...text.matchAll(/<style[^>]*>([\s\S]*?)<\/style>/g)].map((m) => m[1]);
    text = styles.join("\n");
  }
  scanMediaWidths(text, file, usages);
}
// JS 视口判断（matchMedia）与 CSS 同一套数值口径
for (const url of [...tsUrls, ...vueUrls]) {
  const file = relative(SRC.pathname, url.pathname);
  const text = stripComments(readFileSync(url, "utf8"));
  for (const m of text.matchAll(/matchMedia\(\s*["'`]\((min|max)-width:\s*(\d+)px\)/g)) {
    usages.push({ file, kind: "js", minmax: m[1] as "min" | "max", value: Number(m[2]) });
  }
}

describe("断点约定（index.css 清单守卫）", () => {
  it("扫描面非空（收集逻辑失效时此处先炸）", () => {
    expect(usages.length).toBeGreaterThan(20);
  });

  it("全部宽度断点落在约定清单取值内", () => {
    const offList = usages.filter((u) => !ALLOWED_BP.has(u.value));
    expect(
      offList.map((u) => `${u.file} @media(${u.minmax}-width: ${u.value}px)`),
      "出现清单外断点值：先改 index.css 断点约定清单，再改这里",
    ).toEqual([]);
  });

  it("特例档（360 / 1800）只出现在约定清单点名的文件", () => {
    const misplaced = usages.filter(
      (u) => EXCEPTIONAL_BP_FILES[u.value] && !EXCEPTIONAL_BP_FILES[u.value].has(u.file),
    );
    expect(
      misplaced.map((u) => `${u.file} 用了特例档 ${u.value}px`),
      "特例档不得扩散：要么归并到主轴四档，要么走清单评审并更新 EXCEPTIONAL_BP_FILES",
    ).toEqual([]);
  });

  it("base.css 的 --bp-* 四档与约定数值同值", () => {
    const baseCss = readFileSync(new URL("./styles/base.css", SRC), "utf8");
    for (const [name, value] of [
      ["sm", 640],
      ["md", 768],
      ["lg", 900],
      ["xl", 1100],
    ] as const) {
      const m = baseCss.match(new RegExp(`--bp-${name}:\\s*(\\d+)px`));
      expect(m, `base.css 缺少 --bp-${name} token`).not.toBeNull();
      expect(Number(m![1]), `--bp-${name} 应为约定值 ${value}px`).toBe(value);
    }
  });

  it("JS matchMedia 与 CSS 断点同值（RepoImportModals 768 档）", () => {
    const jsUsages = usages.filter((u) => u.kind === "js");
    expect(jsUsages.length).toBeGreaterThan(0);
    for (const u of jsUsages) {
      expect(ALLOWED_BP.has(u.value), `${u.file} matchMedia(${u.value}px) 脱离约定清单`).toBe(true);
    }
  });
});
