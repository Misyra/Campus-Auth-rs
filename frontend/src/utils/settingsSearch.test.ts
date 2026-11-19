// @vitest-environment happy-dom
/**
 * 设置页搜索（utils/settingsSearch.ts，schema 数据驱动机制）。
 *
 * 覆盖三层：
 * 1. 查询词归一化（trim / 小写 / 全角空格）；
 * 2. `searchSettings` 纯数据匹配：分区标题 / 面板标题与 keywords / 标准行
 *    label+hint+keywords+绑定路径 / 槽位声明 text 各自可命中，无命中的分区进
 *    hiddenSectionIds，空查询等价完整列表；
 * 3. `applySettingsSearch` DOM 应用：无命中分区加隐藏类、命中行/槽位加高亮类、
 *    重复应用幂等（先清后标）。
 */
import { describe, expect, it } from "vitest";
import {
  applySettingsSearch,
  normalizeSettingsQuery,
  searchSettings,
  SETTINGS_SEARCH_HIDDEN_CLASS,
  SETTINGS_SEARCH_HIT_CLASS,
  type SettingsSearchResult,
} from "./settingsSearch";
import type { SettingsSectionDef } from "@/views/settings/schema";

/** 双分区 fixture：网络检测（标准行 + 槽位）/ 系统（面板 keywords 命中形态） */
const FIXTURE_SECTIONS: readonly SettingsSectionDef[] = [
  {
    id: "network",
    label: "网络检测",
    title: "检测频率、检测目标与诊断辅助",
    icon: "wifi",
    panels: [
      {
        id: "detect-frequency",
        title: "检测频率",
        entries: [
          {
            id: "monitor-interval",
            label: "检测间隔（秒）",
            hint: "较短间隔能更快发现断线",
            help: "两次网络检测之间的间隔。",
            keywords: "check_interval_seconds",
            control: { kind: "number", binding: "monitor.check_interval_seconds", inputId: "settings-interval" },
          },
        ],
      },
      {
        id: "probe-checks",
        title: "网络状态检测",
        keywords: "204 门户检测 generate_204",
        entries: [
          { slot: "monitor-probe", id: "monitor-probe-rows", text: "204 门户检测 通过轻量请求识别公网连接和认证门户" },
        ],
      },
    ],
  },
  {
    id: "system",
    label: "系统",
    title: "配置预设、启动行为、控制台与访问安全",
    icon: "settings",
    panels: [
      {
        id: "run-mode",
        title: "配置预设",
        chromeless: true,
        component: "run-mode",
        keywords: "配置预设 运行模式 标准模式",
      },
    ],
  },
];

describe("normalizeSettingsQuery", () => {
  it("去首尾空白、小写、全角空格折半角（中文原样保留）", () => {
    expect(normalizeSettingsQuery("  HTTP 代理 ")).toBe("http 代理");
    expect(normalizeSettingsQuery("　代理　")).toBe("代理");
    expect(normalizeSettingsQuery("TRACE")).toBe("trace");
  });
});

describe("searchSettings 纯数据匹配", () => {
  it("标准行的 hint 命中：分区保留且行进 hitUnitIds，其余分区隐藏", () => {
    const stats = searchSettings(FIXTURE_SECTIONS, "断线");
    expect(stats).toEqual<SettingsSearchResult>({
      totalSections: 2,
      matchedSections: 1,
      matchedRows: 1,
      hiddenSectionIds: new Set(["system"]),
      hitUnitIds: new Set(["monitor-interval"]),
    });
  });

  it("绑定路径（keywords）可命中：搜英文字段名也能定位行", () => {
    const stats = searchSettings(FIXTURE_SECTIONS, "check_interval");
    expect(stats.matchedSections).toBe(1);
    expect(stats.hitUnitIds.has("monitor-interval")).toBe(true);
  });

  it("槽位声明的 text 命中：异型面板内容可检索且槽位单元高亮", () => {
    const stats = searchSettings(FIXTURE_SECTIONS, "公网连接");
    expect(stats.hiddenSectionIds.has("system")).toBe(true);
    expect(stats.hitUnitIds).toEqual(new Set(["monitor-probe-rows"]));
  });

  it("面板 keywords 命中：分区保留（标题形态不落行级高亮）", () => {
    const stats = searchSettings(FIXTURE_SECTIONS, "运行模式");
    expect(stats.matchedSections).toBe(1);
    expect(stats.matchedRows).toBe(0);
    expect(stats.hiddenSectionIds.has("network")).toBe(true);
  });

  it("分区标题命中：分区保留，无行级高亮（「系统」不在 network 分区任何单元文本中）", () => {
    const stats = searchSettings(FIXTURE_SECTIONS, "系统");
    expect(stats.matchedSections).toBe(1);
    expect(stats.matchedRows).toBe(0);
    expect(stats.hiddenSectionIds.has("network")).toBe(true);
  });

  it("全部无命中：matchedSections 为 0（视图据此显示空态），分区全部隐藏", () => {
    const stats = searchSettings(FIXTURE_SECTIONS, "不存在的关键词");
    expect(stats.matchedSections).toBe(0);
    expect(stats.hiddenSectionIds).toEqual(new Set(["network", "system"]));
  });

  it("空查询等价完整列表：无隐藏、无命中", () => {
    const stats = searchSettings(FIXTURE_SECTIONS, "   ");
    expect(stats).toEqual<SettingsSearchResult>({
      totalSections: 2,
      matchedSections: 2,
      matchedRows: 0,
      hiddenSectionIds: new Set(),
      hitUnitIds: new Set(),
    });
  });
});

describe("applySettingsSearch DOM 应用", () => {
  function mountFixture(): { form: HTMLElement; sections: HTMLElement[]; rows: Record<string, HTMLElement> } {
    document.body.innerHTML = "";
    const host = document.createElement("div");
    // 与 SettingsView 的产出结构一致：分区 id + 标准行 data-settings-row + 槽位 data-settings-unit
    host.innerHTML = `
      <form class="settings-form">
        <section class="settings-category" id="settings-section-network">
          <h2 class="settings-category-heading">网络检测</h2>
          <div class="settings-list-page">
            <section class="settings-panel"><div class="card-body">
              <div class="setting-row" data-settings-row="monitor-interval">检测间隔</div>
              <div data-settings-unit="monitor-probe-rows">204 门户检测</div>
            </div></section>
          </div>
        </section>
        <section class="settings-category" id="settings-section-system">
          <h2 class="settings-category-heading">系统</h2>
          <div class="settings-list-page">
            <section class="settings-panel"><div class="card-body">配置预设</div></section>
          </div>
        </section>
      </form>`;
    document.body.appendChild(host);
    const form = host.querySelector("form")!;
    return {
      form,
      sections: Array.from(form.querySelectorAll<HTMLElement>(".settings-category")),
      rows: {
        "monitor-interval": form.querySelector("[data-settings-row='monitor-interval']")!,
        "monitor-probe-rows": form.querySelector("[data-settings-unit='monitor-probe-rows']")!,
      },
    };
  }

  it("无命中分区加隐藏类，命中行与槽位加高亮类", () => {
    const { form, sections, rows } = mountFixture();
    applySettingsSearch(form, searchSettings(FIXTURE_SECTIONS, "断线"));
    expect(sections[0]!.classList.contains(SETTINGS_SEARCH_HIDDEN_CLASS)).toBe(false);
    expect(sections[1]!.classList.contains(SETTINGS_SEARCH_HIDDEN_CLASS)).toBe(true);
    expect(rows["monitor-interval"]!.classList.contains(SETTINGS_SEARCH_HIT_CLASS)).toBe(true);
    expect(rows["monitor-probe-rows"]!.classList.contains(SETTINGS_SEARCH_HIT_CLASS)).toBe(false);
  });

  it("槽位单元命中时加高亮类；清空关键词恢复完整列表且重复应用幂等", () => {
    const { form, sections, rows } = mountFixture();
    applySettingsSearch(form, searchSettings(FIXTURE_SECTIONS, "公网连接"));
    expect(rows["monitor-probe-rows"]!.classList.contains(SETTINGS_SEARCH_HIT_CLASS)).toBe(true);
    expect(sections[1]!.classList.contains(SETTINGS_SEARCH_HIDDEN_CLASS)).toBe(true);

    // 空查询 = 纯还原：隐藏与高亮全部清除
    applySettingsSearch(form, searchSettings(FIXTURE_SECTIONS, ""));
    expect(sections[0]!.classList.contains(SETTINGS_SEARCH_HIDDEN_CLASS)).toBe(false);
    expect(sections[1]!.classList.contains(SETTINGS_SEARCH_HIDDEN_CLASS)).toBe(false);
    expect(form.querySelectorAll("." + SETTINGS_SEARCH_HIT_CLASS)).toHaveLength(0);

    // 命中 → 再清空，仍完整还原（上一轮标记被无条件清除）
    applySettingsSearch(form, searchSettings(FIXTURE_SECTIONS, "配置预设"));
    applySettingsSearch(form, searchSettings(FIXTURE_SECTIONS, "   "));
    expect(form.querySelectorAll("." + SETTINGS_SEARCH_HIT_CLASS)).toHaveLength(0);
    expect(sections[0]!.classList.contains(SETTINGS_SEARCH_HIDDEN_CLASS)).toBe(false);
  });

  it("root 为 null 时安全跳过（设置表单尚未挂载）", () => {
    expect(() => applySettingsSearch(null, searchSettings(FIXTURE_SECTIONS, "代理"))).not.toThrow();
  });
});
