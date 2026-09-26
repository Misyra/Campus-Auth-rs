/**
 * 环境组件状态的统一口径：组件名、就绪判定与展示文案的单一事实源。
 * 环境页清单、仪表盘横幅与关于页系统信息共用，避免同一状态在各页各说各话
 * （此前仪表盘笼统说「Python 环境未就绪」、关于页只看 python_ready，与环境页
 * 「缺认证核心」的结论互相矛盾）。
 */
import type { EnvironmentStatus } from "../api/types";

/** 单个环境组件的检查结果 */
export interface EnvComponentCheck {
  /** 稳定标识（key 渲染与测试定位用），与后端字段一一对应 */
  key: "uv" | "python" | "worker" | "browser" | "ocr";
  /** 展示名：与首次启动向导、文档用词一致 */
  name: string;
  /** 可选组件（OCR 未启用时不算缺失，不参与整体就绪判定） */
  optional: boolean;
  /** 该组件是否就绪 */
  ready: boolean;
  /** 单组件状态文案（「可用」「已验证」「Chromium 可用」等） */
  value: string;
}

/** 从 EnvironmentStatus 派生五项组件检查，判定口径与后端 derive_capability_ready 一致 */
export function environmentChecklist(s: EnvironmentStatus): EnvComponentCheck[] {
  const workerReady = s.worker_ready && s.manifest_current;
  const browserReady = s.playwright_ready || s.system_browser_ready;
  return [
    { key: "uv", name: "uv", optional: false, ready: s.uv_ready, value: s.uv_ready ? "可用" : "未就绪" },
    { key: "python", name: "Python", optional: false, ready: s.python_ready, value: s.python_ready ? "可运行" : "未就绪" },
    { key: "worker", name: "认证核心", optional: false, ready: workerReady, value: workerReady ? "已验证" : s.worker_ready ? "需要同步" : "未就绪" },
    { key: "browser", name: "浏览器", optional: false, ready: browserReady, value: s.playwright_ready ? "Chromium 可用" : s.system_browser_ready ? "系统浏览器可用" : "未就绪" },
    { key: "ocr", name: "OCR", optional: !s.ocr_enabled, ready: !s.ocr_enabled || s.ocr_ready, value: !s.ocr_enabled ? "未启用（可选）" : s.ocr_ready ? "已就绪" : "未就绪" },
  ];
}

/** 缺失的必需组件名（供摘要文案「缺 Python、认证核心」）；OCR 未启用不算缺失 */
export function missingRequiredComponents(s: EnvironmentStatus): string[] {
  return environmentChecklist(s)
    .filter((c) => !c.optional && !c.ready)
    .map((c) => c.name);
}
