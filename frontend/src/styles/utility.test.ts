import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";

/**
 * 全局 utility 类守卫。
 *
 * .sr-only 曾被实现为两条容器限定规则（.setting-row .sr-only /
 * .appearance-field .sr-only），22 处使用全靠「恰好都在这两个容器内」才成立，
 * 任何一处挪出父级，原生 color input 就会原形毕露。现提为 base.css 全局
 * utility，容器限定版本必须保持删除。
 */

const baseCss = readFileSync(new URL("./base.css", import.meta.url), "utf8");
const listCss = readFileSync(new URL("./pages/settings/list.css", import.meta.url), "utf8");
const appearanceCss = readFileSync(new URL("./pages/appearance.css", import.meta.url), "utf8");

describe("sr-only 全局 utility", () => {
  it("base.css 提供全局 .sr-only 规则（含 white-space: nowrap 的完整版本）", () => {
    const rule = baseCss.match(/\.sr-only\s*\{([^}]*)\}/);
    expect(rule, "base.css 缺少全局 .sr-only 规则").not.toBeNull();
    for (const decl of [
      "position: absolute",
      "width: 1px",
      "height: 1px",
      "overflow: hidden",
      "clip-path: inset(50%)",
      "white-space: nowrap",
      "border: 0",
    ]) {
      expect(rule![1]).toContain(decl);
    }
  });

  it("容器限定版本已删除，无竞争声明", () => {
    expect(listCss).not.toContain(".sr-only");
    expect(appearanceCss).not.toContain(".sr-only");
  });
});

describe("仪表盘日志搜索框可访问名", () => {
  it("log-search 输入框带 aria-label（不能只靠 placeholder）", () => {
    const view = readFileSync(new URL("../views/DashboardView.vue", import.meta.url), "utf8");
    const input = view.match(/<input[^>]*class="log-search"[^>]*>/);
    expect(input, "DashboardView.vue 缺少 log-search 输入框").not.toBeNull();
    expect(input![0]).toContain('aria-label="搜索日志"');
  });
});
