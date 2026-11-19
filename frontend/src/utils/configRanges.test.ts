/**
 * `CONFIG_RANGES` 与设置页输入的双向一致性测试。
 *
 * 单一出处的价值全在"两侧不许漂移"上：数字输入框写 `min`/`max`、而 `saveConfig` 用
 * `CONFIG_RANGES` 校验，两边只要有一处改了就会静默错位（端口那项已经漂过一次：
 * 输入框 1024、校验放行 1）。这个测试把它变成 CI 能拦住的错误。
 *
 * 反向也查：表里留着一个界面上已经不存在的键，意味着那段校验是死代码。
 *
 * 阶段 4③ 后 UI 声明来自两处：
 * - schema 标准行（views/settings/schema.ts）：控件直接带 binding + min/max，
 *   渲染器按它产出输入框；
 * - 槽位组件模板（views/settings/**，含 panels/ 子目录）：v-model.number + min/max。
 */
import { readFileSync, readdirSync, statSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";
import { describe, it, expect } from "vitest";
import { parse, type TemplateChildNode } from "@vue/compiler-dom";
import { parse as parseSfc } from "@vue/compiler-sfc";
import { CONFIG_RANGES, validateRangeValues } from "./configRanges";
import { SETTINGS_SECTIONS, type SettingsPanelEntry } from "../views/settings/schema";

const here = dirname(fileURLToPath(import.meta.url));
const settingsDir = join(here, "..", "views", "settings");

interface Declared {
  path: string;
  min: number;
  max: number;
  where: string;
}

/** schema 标准行声明的数字区间（渲染器按 binding 直连草稿并产出 min/max） */
function collectSchemaRanges(): Declared[] {
  const out: Declared[] = [];
  for (const section of SETTINGS_SECTIONS) {
    for (const panel of section.panels) {
      for (const entry of (panel.entries ?? []) as readonly SettingsPanelEntry[]) {
        if (!("control" in entry) || entry.control.kind !== "number") continue;
        const { binding, min, max } = entry.control;
        if (min === undefined || max === undefined) continue;
        out.push({ path: binding, min, max, where: `schema:${section.id}/${entry.id}` });
      }
    }
  }
  return out;
}

/** 扫槽位组件模板（递归），收集 `v-model.number="config.config.<path>"` 且带 min/max 的输入框 */
function collectTemplateRanges(dir: string = settingsDir): Declared[] {
  const out: Declared[] = [];
  for (const file of readdirSync(dir)) {
    const full = join(dir, file);
    if (statSync(full).isDirectory()) {
      out.push(...collectTemplateRanges(full));
      continue;
    }
    if (!file.endsWith(".vue")) continue;
    const source = readFileSync(full, "utf8");
    const { descriptor } = parseSfc(source);
    if (!descriptor.template) continue;
    const visit = (node: TemplateChildNode): void => {
      if (node.type !== 1) return;
      const model = node.props.find(
        (prop) => prop.type === 7 && prop.name === "model" && prop.modifiers.some((m) => m.content === "number"),
      );
      const path = model?.type === 7 && model.exp?.type === 4
        ? /^config\.config\.([\w.]+)$/.exec(model.exp.content)?.[1]
        : undefined;
      const numberAttr = (name: string): number | undefined => {
        const attr = node.props.find((prop) => prop.type === 6 && prop.name === name);
        if (attr?.type !== 6 || !attr.value || !/^-?\d+$/.test(attr.value.content)) return undefined;
        return Number(attr.value.content);
      };
      const min = numberAttr("min");
      const max = numberAttr("max");
      if (path && min !== undefined && max !== undefined) {
        out.push({ path, min, max, where: `${file}:${node.loc.start.line}` });
      }
      node.children.forEach(visit);
    };
    parse(descriptor.template.content).children.forEach(visit);
  }
  return out;
}

/**
 * 界面声明了区间、但**有意**不进 saveConfig 校验的字段（维持既有校验面，本批
 * 不改数据流）：推送重试参数自设置页引入起从未收录 CONFIG_RANGES（后端自行钳制）。
 * 出现在这里必须有注释交代原因；新增声明若落不进 CONFIG_RANGES，先裁决再登记。
 */
const UNVALIDATED_DECLARED_PATHS = new Set([
  "notification.retry_max_attempts",
  "notification.retry_base_interval_seconds",
]);

describe("CONFIG_RANGES ↔ 设置页输入", () => {
  const declared = [...collectSchemaRanges(), ...collectTemplateRanges()];

  it("界面里确实扫到了数字区间声明（防止本测试静默失效）", () => {
    expect(declared.length).toBeGreaterThanOrEqual(15);
  });

  it("每个界面声明了区间的字段都在表里，且区间完全一致", () => {
    const mismatches: string[] = [];
    for (const d of declared) {
      if (UNVALIDATED_DECLARED_PATHS.has(d.path)) continue;
      const range = CONFIG_RANGES[d.path];
      if (!range) {
        mismatches.push(`${d.where} 的 ${d.path} 未收录进 CONFIG_RANGES`);
        continue;
      }
      if (range.min !== d.min || range.max !== d.max) {
        mismatches.push(
          `${d.where} 的 ${d.path}：界面 ${d.min}-${d.max}，表里 ${range.min}-${range.max}`,
        );
      }
    }
    expect(mismatches).toEqual([]);
  });

  it("表里每一项都有对应的界面输入框（没有死校验）", () => {
    const paths = new Set(declared.map((d) => d.path));
    const orphans = Object.keys(CONFIG_RANGES).filter((k) => !paths.has(k));
    expect(orphans).toEqual([]);
  });
});

describe("validateRangeValues", () => {
  it("区间内的值通过", () => {
    const r = validateRangeValues({ "app_settings.port": 50721, "retry.max_retries": 3 });
    expect(r.errors).toEqual([]);
    expect(r.warnings).toEqual([]);
  });

  it("NaN（清空输入框）被拦住——它参与比较恒为 false，是最容易漏的一种", () => {
    const r = validateRangeValues({ "browser.timeout": Number.NaN });
    expect(r.errors).toHaveLength(1);
    expect(r.errors[0]).toContain("整数");
  });

  it("非整数被拦住", () => {
    const r = validateRangeValues({ "monitor.check_interval_seconds": 20.5 });
    expect(r.errors).toEqual(["检测间隔（秒）必须是整数"]);
  });

  it("越出建议区间只警告、不阻断（后端是裸 u32 直收，硬拦会让旧配置存不回去）", () => {
    const r = validateRangeValues({ "retry.max_retries": 0 });
    expect(r.errors).toEqual([]);
    expect(r.warnings).toEqual(["最大重试次数建议在 1-5 之间，当前为 0"]);
  });

  it("未出现的字段不参与校验（缺失 ≠ 非法）", () => {
    const r = validateRangeValues({});
    expect(r.errors).toEqual([]);
    expect(r.warnings).toEqual([]);
  });

  it("端口越界属硬错误（超出必然起不来，与「建议区间」不同）", () => {
    expect(validateRangeValues({ "app_settings.port": 70000 }).errors).toHaveLength(1);
    expect(validateRangeValues({ "app_settings.port": 0 }).errors).toHaveLength(1);
    expect(validateRangeValues({ "app_settings.port": -1 }).errors).toHaveLength(1);
  });

  it("低端口放行但给出特权警告（不阻断既有配置）", () => {
    const r = validateRangeValues({ "app_settings.port": 80 });
    expect(r.errors).toEqual([]);
    expect(r.warnings).toHaveLength(1);
    expect(r.warnings[0]).toContain("管理员权限");
  });
});
