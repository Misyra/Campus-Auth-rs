/**
 * environmentStatus 单测：五项组件的判定口径（尤其认证核心的 manifest 复合条件、
 * 浏览器的系统浏览器回退、OCR 的可选语义）是三处页面共用的单一事实源，漂移即三处同时错。
 */
import { describe, expect, it } from "vitest";
import { environmentChecklist, missingRequiredComponents } from "./environmentStatus";
import type { EnvironmentStatus } from "../api/types";

/** 全部就绪的基线状态（capability_ready 为真的最小形态） */
function readyStatus(): EnvironmentStatus {
  return {
    uv_ready: true,
    python_ready: true,
    worker_ready: true,
    manifest_current: true,
    playwright_ready: true,
    system_browser_ready: false,
    ocr_enabled: false,
    ocr_ready: false,
    capability_ready: true,
    stage: "Done",
    progress: null,
    last_error: null,
  };
}

function byKey(s: EnvironmentStatus, key: string) {
  return environmentChecklist(s).find((c) => c.key === key)!;
}

describe("environmentChecklist", () => {
  it("全就绪时五项均为 ready，OCR 未启用标可选", () => {
    const list = environmentChecklist(readyStatus());
    expect(list.map((c) => c.name)).toEqual(["uv", "Python", "认证核心", "浏览器", "OCR"]);
    expect(list.filter((c) => c.ready).length).toBe(5);
    expect(byKey(readyStatus(), "ocr")).toMatchObject({ optional: true, value: "未启用（可选）" });
  });

  it("认证核心 = worker_ready 且 manifest_current，worker 在但清单过期报「需要同步」", () => {
    const s = readyStatus();
    s.worker_ready = true;
    s.manifest_current = false;
    expect(byKey(s, "worker")).toMatchObject({ ready: false, value: "需要同步" });
    s.worker_ready = false;
    expect(byKey(s, "worker")).toMatchObject({ ready: false, value: "未就绪" });
  });

  it("浏览器按 Playwright Chromium → 系统浏览器 回退展示", () => {
    const s = readyStatus();
    s.playwright_ready = false;
    s.system_browser_ready = true;
    expect(byKey(s, "browser")).toMatchObject({ ready: true, value: "系统浏览器可用" });
    s.system_browser_ready = false;
    expect(byKey(s, "browser")).toMatchObject({ ready: false, value: "未就绪" });
  });

  it("OCR 启用后才参与就绪判定，未启用时视为 ready（不算缺失）", () => {
    const s = readyStatus();
    s.ocr_enabled = true;
    s.ocr_ready = false;
    const ocr = byKey(s, "ocr");
    expect(ocr).toMatchObject({ optional: false, ready: false, value: "未就绪" });
    expect(missingRequiredComponents(s)).toContain("OCR");
    s.ocr_enabled = false;
    expect(missingRequiredComponents(s)).not.toContain("OCR");
  });
});

describe("missingRequiredComponents", () => {
  it("点名缺失的必需组件，全就绪时为空", () => {
    const s = readyStatus();
    expect(missingRequiredComponents(s)).toEqual([]);
    s.python_ready = false;
    s.playwright_ready = false;
    s.system_browser_ready = false;
    expect(missingRequiredComponents(s)).toEqual(["Python", "浏览器"]);
  });
});
