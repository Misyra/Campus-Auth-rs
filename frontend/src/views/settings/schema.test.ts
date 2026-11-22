// @vitest-environment happy-dom（导入链经 panels → router 的 createWebHistory
// 需要 window；本文件纯数据守卫、不渲染组件，无 img 资源解析路径）
/**
 * 设置页 schema 完整性守卫。
 *
 * schema 是分区/面板/标准行/搜索文本的单一事实源，键集或绑定路径写错只会表现为
 * 「渲染缺行」「搜索搜不到」这类静默缺陷，类型检查对此零感知。本文件静态断言：
 * - 分区数与锚点 id（tab 条与 URL hash 的契约）；
 * - 分区/面板/行（含槽位）id 全局唯一（搜索命中集合与 DOM data 属性靠它路由）；
 * - 槽位键都能在 panels.ts 注册表解析（渲染时 component :is 不会落空）；
 * - 标准行绑定路径都能在 DEFAULT_CONFIG 上解析（渲染器读写不落 undefined）；
 * - chromeless 面板必须声明 component、非 chromeless 面板必须声明 entries。
 */
import { describe, expect, it, vi } from "vitest";
import { SETTINGS_SECTIONS, type SettingsPanelEntry } from "./schema";
import { SETTINGS_PANEL_SLOTS } from "./panels";
import { DEFAULT_CONFIG } from "@/utils/constants";
import type { Config } from "@/api/types";

// happy-dom 在 Windows runner 上导入真实 BrowserIcon 模块即触发模板
// <img src="/icons/…"> 的文件系统解析（盘符路径非法抛 TypeError，CI 首跑
// 实锤，PR #33）——经 panels 槽位组件转导入亦然。本文件纯数据守卫、图标
// 与断言无关，桩掉即断掉资源解析路径（同 SetupWizard.focus.test.ts）
vi.mock("../../components/common/BrowserIcon.vue", () => ({
  default: { name: "BrowserIcon", render: () => null },
}));

/** 收集一个面板条目的 id（标准行与槽位共用 id 域：搜索命中单元全局唯一） */
function entryIds(entries: readonly SettingsPanelEntry[]): string[] {
  return entries.map((entry) => entry.id);
}

/** 点分路径在对象上逐段取值（与 SchemaRow 渲染器同口径） */
function resolvePath(path: string): unknown {
  let node: unknown = DEFAULT_CONFIG as unknown as Config;
  for (const key of path.split(".")) {
    if (node == null) return undefined;
    node = (node as Record<string, unknown>)[key];
  }
  return node;
}

describe("SETTINGS_SECTIONS 数据完整性", () => {
  it("8 个分区，锚点 id 唯一且与 URL hash 契约一致", () => {
    expect(SETTINGS_SECTIONS.map((s) => s.id)).toEqual([
      "network", "login", "browser", "tasks", "system", "update", "notification", "appearance",
    ]);
  });

  it("分区 / 面板 / 行（含槽位）id 在分区内各自唯一（proxy-note 槽位跨分区共享是唯一的全局重名）", () => {
    const sectionIds = SETTINGS_SECTIONS.map((s) => s.id);
    expect(new Set(sectionIds).size).toBe(sectionIds.length);
    for (const section of SETTINGS_SECTIONS) {
      const panelIds = section.panels.map((p) => p.id);
      const unitIds = section.panels.flatMap((p) => entryIds(p.entries ?? []));
      expect(new Set(panelIds).size, `分区 ${section.id} 面板 id 有重复`).toBe(panelIds.length);
      expect(new Set(unitIds).size, `分区 ${section.id} 行/槽位 id 有重复`).toBe(unitIds.length);
    }
    // 共享槽位：proxy-note 声明在两个分区（网络检测 / 浏览器），两处渲染各自挂 data 属性
    const proxyNoteSections = SETTINGS_SECTIONS.filter((s) =>
      s.panels.some((p) => (p.entries ?? []).some((e) => "slot" in e && e.slot === "proxy-note")),
    ).map((s) => s.id);
    expect(proxyNoteSections).toEqual(["network", "browser"]);
  });

  it("tab 元数据完整：label/title 非空、icon 可用（IconApp 的 key 在编译期校验，此处防运行时改坏）", () => {
    for (const section of SETTINGS_SECTIONS) {
      expect(section.label.length).toBeGreaterThan(0);
      expect(section.title.length).toBeGreaterThan(0);
      expect(section.icon.length).toBeGreaterThan(0);
      expect(section.panels.length).toBeGreaterThan(0);
    }
  });

  it("每个槽位键（slot / chromeless component）都能在注册表解析", () => {
    const keys: string[] = [];
    for (const section of SETTINGS_SECTIONS) {
      for (const panel of section.panels) {
        if (panel.component) keys.push(panel.component);
        for (const entry of panel.entries ?? []) {
          if ("slot" in entry) keys.push(entry.slot);
        }
      }
    }
    expect(keys.length).toBeGreaterThan(15);
    const unresolved = keys.filter((key) => !(key in SETTINGS_PANEL_SLOTS));
    expect(unresolved, `注册表缺少槽位组件：${unresolved.join(", ")}`).toEqual([]);
  });

  it("chromeless 面板必须声明 component，非 chromeless 面板必须声明非空 entries", () => {
    for (const section of SETTINGS_SECTIONS) {
      for (const panel of section.panels) {
        if (panel.chromeless) {
          expect(panel.component, `${panel.id} 缺少 component`).toBeTruthy();
        } else {
          expect(panel.entries?.length ?? 0, `${panel.id} 缺少 entries`).toBeGreaterThan(0);
        }
      }
    }
  });

  it("标准行绑定路径都能在 DEFAULT_CONFIG 上解析（渲染器读写不落空）", () => {
    const rows = SETTINGS_SECTIONS.flatMap((s) =>
      s.panels.flatMap((p) => (p.entries ?? []).filter((entry): entry is Extract<SettingsPanelEntry, { control: unknown }> => "control" in entry)),
    );
    expect(rows.length).toBeGreaterThan(15);
    const broken: string[] = [];
    for (const row of rows) {
      const value = resolvePath(row.control.binding);
      if (value === undefined) broken.push(`${row.id} → ${row.control.binding}`);
    }
    expect(broken, `绑定路径失效：${broken.join(", ")}`).toEqual([]);
  });

  it("搜索单元都携带非空文本（label/hint/keywords 或槽位 text）", () => {
    for (const section of SETTINGS_SECTIONS) {
      for (const panel of section.panels) {
        expect([panel.title, panel.keywords].filter(Boolean).join(" ").length).toBeGreaterThan(0);
        for (const entry of panel.entries ?? []) {
          if ("slot" in entry) {
            expect(entry.text.length, `${entry.id} 槽位缺搜索文本`).toBeGreaterThan(0);
          } else {
            expect(entry.label.length, `${entry.id} 行缺 label`).toBeGreaterThan(0);
          }
        }
      }
    }
  });
});
