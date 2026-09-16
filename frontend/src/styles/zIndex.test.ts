import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";

/**
 * z-index 层级阶梯守卫。
 *
 * 层级是纯声明式的，类型检查与构建都不会发现被压平：曾出现「关于」页卸载弹窗
 * 压住其确认框——两者同为 --z-top，同层级按 DOM 顺序决胜，常驻挂载的
 * ConfirmDialog 锚点先入 body，必输给路由进入后才挂载的 Modal，导致确认框
 * 不可见而其按钮仍持焦点（Enter 即执行不可恢复的清理）。
 */

const baseCss = readFileSync(new URL("./base.css", import.meta.url), "utf8");
const modalCss = readFileSync(new URL("./components/modal.css", import.meta.url), "utf8");

/** 取出 base.css 中某个 --z-* token 的数值 */
function zToken(name: string): number {
  const m = baseCss.match(new RegExp(`--z-${name}:\\s*(\\d+)\\s*;`));
  if (!m) throw new Error(`base.css 缺少 --z-${name} token`);
  return Number(m[1]);
}

describe("z-index 层级阶梯", () => {
  it("token 严格递增，不存在同层级决胜", () => {
    const ladder = [
      "base",
      "dropdown",
      "sticky",
      "sidebar",
      "overlay",
      "modal",
      "toast",
      "top",
      "confirm",
      "max",
    ].map(zToken);
    for (let i = 1; i < ladder.length; i++) {
      expect(ladder[i], `第 ${i} 级不高于第 ${i - 1} 级`).toBeGreaterThan(ladder[i - 1]);
    }
  });

  it("确认框高于普通弹窗，普通弹窗高于 Toast", () => {
    expect(zToken("confirm")).toBeGreaterThan(zToken("top"));
    expect(zToken("top")).toBeGreaterThan(zToken("toast"));
  });

  it("modal.css 把确认框遮罩修饰类映射到 --z-confirm", () => {
    const rule = modalCss.match(/\.modal-overlay--confirm\s*\{([^}]*)\}/);
    expect(rule, "modal.css 缺少 .modal-overlay--confirm 规则").not.toBeNull();
    expect(rule![1]).toContain("var(--z-confirm)");
  });

  it("ConfirmDialog 挂载确认框遮罩修饰类", () => {
    const dialog = readFileSync(
      new URL("../components/common/ConfirmDialog.vue", import.meta.url),
      "utf8",
    );
    expect(dialog).toContain('class="modal-overlay modal-overlay--confirm"');
  });
});
