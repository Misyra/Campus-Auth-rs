// @vitest-environment happy-dom
/**
 * 设置页搜索（utils/settingsSearch.ts）。
 *
 * 覆盖三层：
 * 1. 查询词归一化（trim / 小写 / 全角空格）；
 * 2. 过滤逻辑：无命中分区整体隐藏、命中分区保留并高亮最深命中单元、
 *    清空关键词恢复完整列表（幂等还原）；
 * 3. 基本交互：输入 → v-model → watch → applySettingsSearch 的完整链路
 *    （与 SettingsView.vue 的接线方式一致，用 createApp 直接挂载，不引入
 *    @vue/test-utils），含清空按钮恢复与输入框可访问名。
 */
import { describe, expect, it } from "vitest";
import { createApp, defineComponent, h, nextTick, ref, watch } from "vue";
import {
  applySettingsSearch,
  normalizeSettingsQuery,
  SETTINGS_SEARCH_HIDDEN_CLASS,
  SETTINGS_SEARCH_HIT_CLASS,
  type SettingsSearchResult,
} from "./settingsSearch";

const FIXTURE_HTML = `
<form>
  <section class="settings-category" id="settings-section-monitor">
    <h2 class="settings-category-heading">网络检测</h2>
    <div class="setting-row"><div class="setting-row-title"><label>自动重连</label></div>
      <div class="setting-row-description"><p>断线后自动重试登录</p></div></div>
    <div class="setting-row"><div class="setting-row-title"><label>暂停时段</label></div></div>
  </section>
  <section class="settings-category" id="settings-section-system">
    <h2 class="settings-category-heading">系统与更新</h2>
    <div class="settings-subsection">
      <h3>下载与任务仓库代理</h3>
      <div class="setting-row"><div class="setting-row-title"><label>使用代理</label></div>
        <div class="setting-row-description"><p>仅影响版本更新检查</p></div></div>
    </div>
    <div class="setting-row"><div class="setting-row-title"><label>任务通知</label></div></div>
  </section>
</form>`;

function mountFixture(): { host: HTMLElement; form: HTMLFormElement; sections: HTMLElement[] } {
  // 每次挂载前清空 body：避免前一个用例的分区残留进 document 级查询
  document.body.innerHTML = "";
  const host = document.createElement("div");
  host.innerHTML = FIXTURE_HTML;
  document.body.appendChild(host);
  const form = host.querySelector("form")!;
  return { host, form, sections: Array.from(form.querySelectorAll<HTMLElement>(".settings-category")) };
}

describe("normalizeSettingsQuery", () => {
  it("去首尾空白、小写、全角空格折半角（中文原样保留）", () => {
    expect(normalizeSettingsQuery("  HTTP 代理 ")).toBe("http 代理");
    expect(normalizeSettingsQuery("　代理　")).toBe("代理");
    expect(normalizeSettingsQuery("TRACE")).toBe("trace");
  });
});

describe("applySettingsSearch 过滤逻辑", () => {
  it("按设置项描述命中：命中分区保留并高亮命中行，无命中的分区整体隐藏", () => {
    const { form, sections } = mountFixture();
    const stats = applySettingsSearch(form, "断线");
    expect(stats).toEqual<SettingsSearchResult>({ totalSections: 2, matchedSections: 1, matchedRows: 1 });
    expect(sections[0]!.classList.contains(SETTINGS_SEARCH_HIDDEN_CLASS)).toBe(false);
    expect(sections[1]!.classList.contains(SETTINGS_SEARCH_HIDDEN_CLASS)).toBe(true);
    const hits = form.querySelectorAll("." + SETTINGS_SEARCH_HIT_CLASS);
    expect(hits).toHaveLength(1);
    expect(hits[0]!.textContent).toContain("断线后自动重试登录");
  });

  it("按分区标题命中：该分区保留（无行级命中则不高亮），其余隐藏", () => {
    const { form, sections } = mountFixture();
    const stats = applySettingsSearch(form, "系统与更新");
    expect(stats.matchedSections).toBe(1);
    expect(sections[0]!.classList.contains(SETTINGS_SEARCH_HIDDEN_CLASS)).toBe(true);
    expect(sections[1]!.classList.contains(SETTINGS_SEARCH_HIDDEN_CLASS)).toBe(false);
    expect(form.querySelectorAll("." + SETTINGS_SEARCH_HIT_CLASS)).toHaveLength(0);
  });

  it("最深命中单元：子设置行命中时不重复标记父分组", () => {
    const { form } = mountFixture();
    applySettingsSearch(form, "使用代理");
    const hits = Array.from(form.querySelectorAll<HTMLElement>("." + SETTINGS_SEARCH_HIT_CLASS));
    expect(hits).toHaveLength(1);
    expect(hits[0]!.classList.contains("setting-row")).toBe(true);
    expect(hits[0]!.querySelector(".settings-subsection")).toBeNull();
  });

  it("清空关键词恢复完整列表，且重复应用幂等", () => {
    const { form, sections } = mountFixture();
    applySettingsSearch(form, "断线");
    expect(sections[1]!.classList.contains(SETTINGS_SEARCH_HIDDEN_CLASS)).toBe(true);

    const restored = applySettingsSearch(form, "");
    expect(restored.matchedSections).toBe(2);
    for (const section of sections) {
      expect(section.classList.contains(SETTINGS_SEARCH_HIDDEN_CLASS)).toBe(false);
    }
    expect(form.querySelectorAll("." + SETTINGS_SEARCH_HIT_CLASS)).toHaveLength(0);

    // 再来一轮命中后再次清空，仍完整还原（上一轮标记被无条件清除）
    applySettingsSearch(form, "任务通知");
    expect(sections[0]!.classList.contains(SETTINGS_SEARCH_HIDDEN_CLASS)).toBe(true);
    applySettingsSearch(form, "   ");
    expect(sections[0]!.classList.contains(SETTINGS_SEARCH_HIDDEN_CLASS)).toBe(false);
    expect(sections[1]!.classList.contains(SETTINGS_SEARCH_HIDDEN_CLASS)).toBe(false);
    expect(form.querySelectorAll("." + SETTINGS_SEARCH_HIT_CLASS)).toHaveLength(0);
  });

  it("全部无命中：matchedSections 为 0（视图据此显示空态），分区全部隐藏", () => {
    const { form, sections } = mountFixture();
    const stats = applySettingsSearch(form, "不存在的关键词");
    expect(stats.matchedSections).toBe(0);
    for (const section of sections) {
      expect(section.classList.contains(SETTINGS_SEARCH_HIDDEN_CLASS)).toBe(true);
    }
  });

  it("root 为 null 时安全返回零统计（设置表单尚未挂载）", () => {
    expect(applySettingsSearch(null, "代理")).toEqual<SettingsSearchResult>({
      totalSections: 0, matchedSections: 0, matchedRows: 0,
    });
  });
});

describe("设置搜索基本交互（输入 → 过滤 → 清空恢复）", () => {
  interface Harness {
    host: HTMLElement;
    input: HTMLInputElement;
    clearButton: HTMLButtonElement;
    stats: () => SettingsSearchResult;
  }

  /** 与 SettingsView.vue 相同的接线：查询词 ref + watch 后 nextTick 应用过滤 */
  function mountHarness(): Harness {
    document.body.innerHTML = "";
    const host = document.createElement("div");
    document.body.appendChild(host);
    const query = ref("");
    let formEl: HTMLElement | null = null;
    let latest: SettingsSearchResult = { totalSections: 0, matchedSections: 0, matchedRows: 0 };
    const app = createApp(defineComponent({
      setup() {
        watch(query, () => { void nextTick(() => { latest = applySettingsSearch(formEl, query.value); }); });
        return () =>
          h("div", [
            h("input", {
              "aria-label": "搜索设置",
              placeholder: "搜索设置项、说明或分区标题",
              value: query.value,
              onInput: (e: Event) => { query.value = (e.target as HTMLInputElement).value; },
            }),
            h("button", {
              type: "button",
              class: "settings-search-clear",
              onClick: () => { query.value = ""; },
            }, "清空"),
            h("form", {
              ref: (el: unknown) => { formEl = el as HTMLElement; },
              innerHTML: FIXTURE_HTML,
            }),
          ]);
      },
    }));
    app.mount(host);
    return {
      host,
      input: host.querySelector('input[aria-label="搜索设置"]')!,
      clearButton: host.querySelector<HTMLButtonElement>(".settings-search-clear")!,
      stats: () => latest,
    };
  }

  it("输入关键词后分区被过滤，点清空按钮恢复完整列表", async () => {
    const { host, input, clearButton, stats } = mountHarness();
    // 输入关键词（模拟用户键入触发 v-model）
    input.value = "断线";
    input.dispatchEvent(new Event("input", { bubbles: true }));
    await nextTick();
    await nextTick();
    const sections = host.querySelectorAll(".settings-category");
    expect(sections[0]!.classList.contains(SETTINGS_SEARCH_HIDDEN_CLASS)).toBe(false);
    expect(sections[1]!.classList.contains(SETTINGS_SEARCH_HIDDEN_CLASS)).toBe(true);
    expect(stats().matchedSections).toBe(1);

    // 点击清空 → 查询词归零 → 完整列表恢复
    clearButton.click();
    await nextTick();
    await nextTick();
    expect(sections[0]!.classList.contains(SETTINGS_SEARCH_HIDDEN_CLASS)).toBe(false);
    expect(sections[1]!.classList.contains(SETTINGS_SEARCH_HIDDEN_CLASS)).toBe(false);
    expect(stats().matchedSections).toBe(2);
  });

  it("搜索输入框带可访问名（对齐 DashboardView log-search 的做法）", () => {
    const { input } = mountHarness();
    expect(input.getAttribute("aria-label")).toBe("搜索设置");
    expect(input.getAttribute("placeholder")).toBeTruthy();
  });
});
