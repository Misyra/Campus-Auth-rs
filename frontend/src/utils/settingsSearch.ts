/**
 * 设置页搜索：纯前端过滤，不产生任何后端调用。
 *
 * **schema 数据驱动**（阶段 4③ 重写）：匹配范围不再是渲染后的 DOM textContent，
 * 而是 `views/settings/schema.ts` 声明的分区标题 / 面板标题与关键词 / 标准行的
 * label+hint+help+keywords——schema 是搜索文本的单一事实源，异型面板的检索词
 * 也由其槽位声明在 schema 里（面板 keywords / 槽位 text）。手写组件的措辞改动
 * 需要同步 schema 中的搜索文本（schema.test.ts 断言键集一致，措辞靠评审守约）。
 *
 * 流程两段式：
 * 1. `searchSettings`：纯数据匹配（无 DOM），返回分区显隐与行/槽位命中集合，
 *    供空态统计与测试断言；
 * 2. `applySettingsSearch`：把结果落到 DOM——无命中的分区加 .settings-search-hidden
 *    整体隐藏，命中的标准行（[data-settings-row]）与槽位（[data-settings-unit]）
 *    加 .settings-search-hit 高亮；每次调用先清掉上一轮标记，幂等。
 */

import type { SettingsSectionDef, SettingsPanelEntry } from "@/views/settings/schema";

export interface SettingsSearchResult {
  /** 页面分区总数 */
  totalSections: number;
  /** 命中关键词的分区数（0 时视图显示空态提示） */
  matchedSections: number;
  /** 行级高亮的设置项数（标准行 + 槽位单元） */
  matchedRows: number;
  /** 无命中（应隐藏）的分区 id 集合 */
  hiddenSectionIds: ReadonlySet<string>;
  /** 命中的行/槽位单元 id 集合（DOM 侧按 data-settings-row / data-settings-unit 挂高亮类） */
  hitUnitIds: ReadonlySet<string>;
}

export const SETTINGS_SEARCH_HIDDEN_CLASS = "settings-search-hidden";
export const SETTINGS_SEARCH_HIT_CLASS = "settings-search-hit";

const SECTION_ELEMENT_PREFIX = "settings-section-";
const ROW_DATA_ATTR = "data-settings-row";
const UNIT_DATA_ATTR = "data-settings-unit";

/** 归一化：全角空格折半角、去首尾空白、小写（中文原样保留）。 */
export function normalizeSettingsQuery(raw: string): string {
  return raw.replace(/\u3000/g, " ").trim().toLowerCase();
}

function matchesQuery(query: string, text: string | null | undefined): boolean {
  return text != null && normalizeSettingsQuery(text).includes(query);
}

/** 收集一个面板条目的搜索单元文本：标准行 = label+hint+help+keywords+绑定路径；槽位 = 声明的 text */
function entryText(entry: SettingsPanelEntry): string {
  if ("control" in entry) {
    return [entry.label, entry.hint, entry.help, entry.keywords, entry.control.binding]
      .filter((part) => typeof part === "string" && part.length > 0)
      .join(" ");
  }
  return entry.text;
}

/**
 * 对设置 schema 应用一次搜索匹配（纯数据，无 DOM）。空查询等价于完整列表：
 * 无分区隐藏、无单元命中。返回统计与显隐/命中集合供视图应用与测试断言。
 */
export function searchSettings(
  sections: readonly SettingsSectionDef[],
  rawQuery: string,
): SettingsSearchResult {
  const totalSections = sections.length;
  const query = normalizeSettingsQuery(rawQuery);
  if (!query) {
    return {
      totalSections,
      matchedSections: totalSections,
      matchedRows: 0,
      hiddenSectionIds: new Set(),
      hitUnitIds: new Set(),
    };
  }

  const hiddenSectionIds = new Set<string>();
  const hitUnitIds = new Set<string>();
  let matchedSections = 0;
  let matchedRows = 0;

  for (const section of sections) {
    // 分区标题本身可命中（如搜「网络检测」）：该分区保留，但不做行级高亮
    const sectionTitleHit = matchesQuery(query, section.label);
    const hits: string[] = [];
    let sectionHit = sectionTitleHit;
    for (const panel of section.panels) {
      // 面板标题与关键词命中只保证分区保留；条目仍继续扫描，命中的行/槽位照常高亮
      const panelText = [panel.title, panel.keywords].filter(Boolean).join(" ");
      if (matchesQuery(query, panelText)) sectionHit = true;
      for (const entry of panel.entries ?? []) {
        if (matchesQuery(query, entryText(entry))) hits.push(entry.id);
      }
    }
    if (sectionHit || hits.length > 0) {
      matchedSections++;
      matchedRows += hits.length;
      for (const id of hits) hitUnitIds.add(id);
    } else {
      hiddenSectionIds.add(section.id);
    }
  }

  return { totalSections, matchedSections, matchedRows, hiddenSectionIds, hitUnitIds };
}

/**
 * 把搜索结果落到设置表单 DOM：`root` 是 settings-form 元素（分区挂在它下面），
 * 传 null 安全跳过。幂等：先无条件清掉上一轮标记再标记本轮。
 */
export function applySettingsSearch(
  root: ParentNode | null,
  result: SettingsSearchResult,
): void {
  if (!root) return;
  const sections = Array.from(root.querySelectorAll<HTMLElement>("[id^='settings-section-']"));
  for (const section of sections) {
    const isHidden = result.hiddenSectionIds.has(section.id.slice(SECTION_ELEMENT_PREFIX.length));
    section.classList.toggle(SETTINGS_SEARCH_HIDDEN_CLASS, isHidden);
    for (const hit of section.querySelectorAll("." + SETTINGS_SEARCH_HIT_CLASS)) {
      hit.classList.remove(SETTINGS_SEARCH_HIT_CLASS);
    }
  }
  // 命中单元全部现查一遍（分区可能不在 root 直下，scoped 查询跨 chromeless 槽位）
  for (const id of result.hitUnitIds) {
    for (const el of root.querySelectorAll<HTMLElement>(`[${ROW_DATA_ATTR}="${id}"], [${UNIT_DATA_ATTR}="${id}"]`)) {
      el.classList.add(SETTINGS_SEARCH_HIT_CLASS);
    }
  }
}
