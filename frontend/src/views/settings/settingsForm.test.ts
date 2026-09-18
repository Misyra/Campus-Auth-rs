import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";

/**
 * 设置页表单提交守卫。
 *
 * 背景（实测定位）：`SettingsView.vue` 用 `<form autocomplete="on">` 包裹路由内容，
 * 只为拿到浏览器自动填充。HTML 规范中 `<button>` 缺 `type` 时其 IDL `type` 默认为
 * `"submit"`，所以表单内任何漏写 `type="button"` 的按钮被点击都会触发**原生表单提交**
 * → 浏览器导航到当前 URL → 整个 SPA 重载。
 *
 * 实测症状与证据（Playwright，见 docs/reports/verify-submit-bug.py）：
 * - submit 事件确实触发，`event.submitter` 就是该按钮；
 * - 表单 method=get 且 action 为当前 URL，提交后 **token 查询串被覆盖掉**
 *   （`?token=...` → `?`），随之丢失鉴权上下文。
 *
 * 为何「立即检查」等同样漏写 type 的按钮当时没暴雷：其 click 处理器**首行**同步置位
 * busy 标志 → Vue 微任务刷 DOM → 按钮变成 disabled → 浏览器在默认动作阶段发现
 * submitter 已禁用而取消提交。这是**偶然**保护：一旦置位挪到 await 之后（如新按钮
 * 「选择安装包」首行是 `await pickFile(...)`）就立即失效。故不能依赖"先置 busy"，
 * 三类守卫缺一不可：
 *   1. 按钮显式 `type="button"`；
 *   2. 表单 `@submit.prevent`（漏写 type 时的兜底）；
 *   3. 本测试静态锁定前两者。
 */

const settingsView = readFileSync(
  new URL("../SettingsView.vue", import.meta.url),
  "utf8",
);

/** 设置页子组件（表单内容来源；按钮都写在 views/settings/ 下） */
const settingsChildFiles = [
  "MonitorSettings.vue",
  "BrowserSettings.vue",
  "TaskEnvironmentSettings.vue",
  "SystemSettings.vue",
  "NetworkSettings.vue",
];

describe("设置页表单提交守卫", () => {
  it("settings-form 带 @submit.prevent，漏写 type 的按钮不会导致整页重载", () => {
    // 匹配 <form ... class="settings-form" ...> 开标签（属性可跨行）
    const formTag = settingsView.match(/<form\b[^>]*class="settings-form"[^>]*>/);
    expect(formTag, "SettingsView.vue 未找到 form.settings-form").not.toBeNull();
    expect(
      /@submit\.prevent/.test(formTag![0]),
      "form.settings-form 缺少 @submit.prevent：任何漏写 type=\"button\" 的按钮都会触发原生提交并重载页面",
    ).toBe(true);
  });

  it("设置页内所有 <button> 都显式声明 type", () => {
    const offenders: string[] = [];
    for (const file of settingsChildFiles) {
      const src = readFileSync(new URL(`./${file}`, import.meta.url), "utf8");
      // 逐个取出开标签，检查其中是否含 type= 属性
      const tags = src.match(/<button\b[^>]*>/g) ?? [];
      for (const tag of tags) {
        if (!/\btype\s*=/.test(tag)) {
          offenders.push(`${file}: ${tag.replace(/\s+/g, " ").slice(0, 80)}`);
        }
      }
    }
    // AppearanceView 在 views/ 而非 views/settings/，单独读
    const appearance = readFileSync(new URL("../AppearanceView.vue", import.meta.url), "utf8");
    for (const tag of appearance.match(/<button\b[^>]*>/g) ?? []) {
      if (!/\btype\s*=/.test(tag)) {
        offenders.push(`AppearanceView.vue: ${tag.replace(/\s+/g, " ").slice(0, 80)}`);
      }
    }
    expect(
      offenders,
      "以下按钮未写 type（HTML 默认 type=submit，位于设置页表单内时会提交并重载页面）：\n" +
        offenders.join("\n"),
    ).toEqual([]);
  });
});
