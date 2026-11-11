/**
 * 设置页搜索：纯前端过滤，不产生任何后端调用。
 *
 * 匹配范围 = 所属分区标题 + 设置项标签 + 描述（含 sr-only 文案）。设置项分散在
 * 五个子组件里，从 SettingsView 逐个下发查询词需要改五层 props；这里统一取渲染后
 * 的 textContent 做包含匹配，视图结构变化时匹配范围自动跟随，无需维护索引。
 *
 * 粒度：
 * - 无命中的分区（.settings-category）整体隐藏（.settings-search-hidden）；
 * - 命中的分区保留，其中「最深层的命中单元」加 .settings-search-hit 高亮——
 *   设置行命中时不再重复标记它的父分组（如 .settings-subsection）；
 * - 清空关键词即恢复完整列表：applySettingsSearch 每次调用先清掉上一轮标记，
 *   空关键词等价于一次纯还原。
 */

export interface SettingsSearchResult {
  /** 页面分区总数 */
  totalSections: number;
  /** 命中关键词的分区数（0 时视图显示空态提示） */
  matchedSections: number;
  /** 行级高亮的设置项数 */
  matchedRows: number;
}

export const SETTINGS_SEARCH_HIDDEN_CLASS = "settings-search-hidden";
export const SETTINGS_SEARCH_HIT_CLASS = "settings-search-hit";

const SECTION_SELECTOR = ".settings-category";

/** 行级高亮单元：各设置子页里「一个设置项 / 一小组」的粒度。尽力覆盖——
 * 某类结构漏选时该处只是不高亮，分区显隐仍按整段 textContent 判定，不影响正确性。 */
const ROW_UNIT_SELECTOR = [
  ".setting-row",
  ".settings-subsection",
  ".run-mode-option",
  ".browser-choice",
  ".env-lead",
  ".env-status-row",
  ".env-check-item",
  ".ocr-pick-row",
  ".manual-update-tip",
  ".appearance-slider-item",
  ".appearance-presets",
  ".appearance-bg-thumb-group",
].join(", ");

/** 归一化：全角空格折半角、去首尾空白、小写（中文原样保留）。 */
export function normalizeSettingsQuery(raw: string): string {
  return raw.replace(/\u3000/g, " ").trim().toLowerCase();
}

function matchesQuery(query: string, text: string | null | undefined): boolean {
  return text != null && normalizeSettingsQuery(text).includes(query);
}

/**
 * 对设置表单应用一次搜索过滤。`root` 是 settings-form 元素（分区挂在它下面），
 * 传 null 或空关键词等于恢复完整列表。返回统计供空态提示与测试断言使用。
 */
export function applySettingsSearch(root: ParentNode | null, rawQuery: string): SettingsSearchResult {
  const sections = root ? Array.from(root.querySelectorAll<HTMLElement>(SECTION_SELECTOR)) : [];
  const query = normalizeSettingsQuery(rawQuery);
  // 先无条件清掉上一轮结果，保证重复调用幂等、空关键词必然还原完整列表
  for (const section of sections) {
    section.classList.remove(SETTINGS_SEARCH_HIDDEN_CLASS);
    for (const hit of section.querySelectorAll("." + SETTINGS_SEARCH_HIT_CLASS)) {
      hit.classList.remove(SETTINGS_SEARCH_HIT_CLASS);
    }
  }
  if (!query) return { totalSections: sections.length, matchedSections: sections.length, matchedRows: 0 };

  let matchedSections = 0;
  let matchedRows = 0;
  for (const section of sections) {
    if (!matchesQuery(query, section.textContent)) {
      section.classList.add(SETTINGS_SEARCH_HIDDEN_CLASS);
      continue;
    }
    matchedSections++;
    const units = Array.from(section.querySelectorAll<HTMLElement>(ROW_UNIT_SELECTOR));
    const hits = units.filter((unit) => matchesQuery(query, unit.textContent));
    for (const unit of hits) {
      // 最深命中单元：子项已命中时跳过其父分组，高亮只落在用户要找的那一行
      if (hits.some((other) => other !== unit && unit.contains(other))) continue;
      unit.classList.add(SETTINGS_SEARCH_HIT_CLASS);
      matchedRows++;
    }
  }
  return { totalSections: sections.length, matchedSections, matchedRows };
}
