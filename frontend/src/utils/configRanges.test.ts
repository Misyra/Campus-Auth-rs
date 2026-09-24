/**
 * `CONFIG_RANGES` 与设置页模板的双向一致性测试。
 *
 * 单一出处的价值全在"两侧不许漂移"上：数字输入框写 `min`/`max`、而 `saveConfig` 用
 * `CONFIG_RANGES` 校验，两边只要有一处改了就会静默错位（端口那项已经漂过一次：
 * 输入框 1024、校验放行 1）。这个测试把它变成 CI 能拦住的错误。
 *
 * 反向也查：表里留着一个模板上已经不存在的键，意味着那段校验是死代码。
 */
import { readFileSync, readdirSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";
import { describe, it, expect } from "vitest";
import { CONFIG_RANGES, validateRangeValues } from "./configRanges";

const here = dirname(fileURLToPath(import.meta.url));
const settingsDir = join(here, "..", "views", "settings");

interface Declared {
  path: string;
  min: number;
  max: number;
  where: string;
}

/** 扫设置页模板，收集 `v-model.number="config.config.<path>"` 且带 min/max 的输入框 */
function collectDeclaredRanges(): Declared[] {
  const out: Declared[] = [];
  for (const file of readdirSync(settingsDir)) {
    if (!file.endsWith(".vue")) continue;
    const lines = readFileSync(join(settingsDir, file), "utf8").split("\n");
    lines.forEach((line, i) => {
      const model = /v-model\.number="config\.config\.([\w.]+)"/.exec(line);
      if (!model) return;
      const min = /min="(-?\d+)"/.exec(line);
      const max = /max="(-?\d+)"/.exec(line);
      if (!min || !max) return;
      out.push({
        path: model[1],
        min: Number(min[1]),
        max: Number(max[1]),
        where: `${file}:${i + 1}`,
      });
    });
  }
  return out;
}

describe("CONFIG_RANGES ↔ 设置页输入框", () => {
  const declared = collectDeclaredRanges();

  it("模板里确实扫到了数字区间声明（防止本测试静默失效）", () => {
    expect(declared.length).toBeGreaterThanOrEqual(10);
  });

  it("每个界面声明了区间的字段都在表里，且区间完全一致", () => {
    const mismatches: string[] = [];
    for (const d of declared) {
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
