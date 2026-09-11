/**
 * API 端点函数集合。
 * 所有调用统一走此处，禁止在业务代码中直接 fetch。
 * 每个端点返回 client.ts 解包 .data 后的纯业务负载（成功时）。
 * 路径与后端 openapi.json 保持一致。
 */

import { ensureAuthToken, http } from "./client";
import type { RequestOptions } from "./client";
import type {
  AiCaptureResult,
  AiLlmConfig,
  AutostartStatus,
  BackgroundUploadResult,
  BinaryInfo,
  BrowserListResponse,
  ConfigResponse,
  DebugSession,
  EnvironmentStatus,
  HealthInfo,
  InitStatus,
  LoginHistoryItem,
  LogEntry,
  MutationResult,
  NetworkDetectResult,
  NetworkTestResult,
  OcrStatus,
  PortalDetectResult,
  Profile,
  ProfileListResponse,
  RepoTask,
  SaveConfigPayload,
  ScheduledTask,
  ScheduledTaskHistoryItem,
  Script,
  StatusSnapshot,
  TaskDetail,
  TaskItem,
  UninstallDetectItem,
  UninstallResponse,
  UpdateInfo,
  UpdatePin,
  UpdateState,
} from "./types";

/** 路径段编码：所有 id/filename 插值前必经此函数 */
const pathSegment = (s: string) => encodeURIComponent(s);

/** 带 60s 超时的 bundle 下载：导出走裸 fetch（返回 Blob，不经 http 封装），
 *  后端打 zip 卡住时需超时兜底报错而非永久挂起（F8）。 */
async function fetchBundleWithTimeout(url: string, init: RequestInit): Promise<Blob> {
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), 60000);
  try {
    const res = await fetch(url, { ...init, signal: controller.signal });
    if (!res.ok) {
      const text = await res.text().catch(() => "");
      let msg = `导出失败 (${res.status})`;
      try {
        const j = JSON.parse(text) as { error?: { message?: string } };
        if (j?.error?.message) msg = j.error.message;
      } catch {
        if (text) msg = text.slice(0, 200);
      }
      throw new Error(msg);
    }
    return await res.blob();
  } catch (e) {
    if (e instanceof Error && e.name === "AbortError") {
      throw new Error("导出超时（60s），请稍后重试");
    }
    throw e;
  } finally {
    clearTimeout(timer);
  }
}

export { ApiError, extractApiError } from "./client";

/** 配置相关 */
export const configApi = {
  fetch: () => http.get<ConfigResponse>("/api/config"),
  // B4：原 PUT /api/config 全量保存方法已删除（全库零调用，且扁平整体替换
  // 的载荷形状容易在后端演变为清空配置的地雷）；保存统一走 patch 增量语义
  patch: (payload: SaveConfigPayload, opts?: RequestOptions) => http.patch<MutationResult>("/api/config", payload, opts),
  fetchLogLevels: () => http.get<{ level: string }>("/api/config/log-levels"),
  setLogLevel: (level: string) => http.put<MutationResult>("/api/config/log-level", { level }),
  fetchStealthScript: () => http.get<{ script: string }>("/api/config/default-stealth-script"),
  reload: () => http.post<MutationResult>("/api/config/reload"),
};

/** 监控与登录操作 */
export const monitorApi = {
  fetchStatus: () => http.get<StatusSnapshot>("/api/monitor/status"),
  start: () => http.post<MutationResult>("/api/monitor/start"),
  stop: () => http.post<MutationResult>("/api/monitor/stop"),
  /** 认证门户检测：未认证时跟随 302 返回候选门户地址，需先退出登录 */
  detectPortal: () =>
    http.post<PortalDetectResult>("/api/monitor/detect-portal", null, { timeout: 60000 }),
};

/** 一次性操作 */
export const actionsApi = {
  login: (timeoutMs: number) =>
    http.post<MutationResult>("/api/login", null, { timeout: timeoutMs }),
  cancelLogin: () => http.post<MutationResult>("/api/login/cancel"),
  testNetwork: () => http.post<NetworkTestResult>("/api/monitor/test", null, { timeout: 30000 }),
};

/** 系统 */
export const systemApi = {
  health: () => http.get<HealthInfo>("/api/health"),
  initStatus: () => http.get<InitStatus>("/api/init-status"),
  checkUpdate: () => http.get<UpdateInfo>("/api/check-update"),
  // 上次检查状态（设置页"上次检查时间"数据源，只读不触发网络检查）
  updateState: () => http.get<UpdateState>("/api/update-state"),
  agree: () => http.post<MutationResult>("/api/agree"),
  shutdown: () => http.post<MutationResult>("/api/system/shutdown"),
  update: (pin?: UpdatePin) =>
    http.post<MutationResult & { message?: string; version?: string }>(
      "/api/system/update",
      pin ?? null,
    ),
  fetchLogs: (limit: number) => http.get<LogEntry[]>(`/api/logs?limit=${limit}`),
  /** 导出日志压缩包：后端打 zip（运行日志 + 登录历史 + 脱敏 meta），返回 Blob */
  async exportLogs(): Promise<Blob> {
    const token = await ensureAuthToken();
    return fetchBundleWithTimeout("/api/logs/export", {
      headers: token ? { "X-Auth-Token": token } : undefined,
    });
  },
};

/** 环境初始化（uv sync + Chromium，POST /api/environment/bootstrap） */
export const environmentApi = {
  bootstrap: (opts?: RequestOptions) =>
    http.post<
      MutationResult & {
        capability_ready: boolean;
        uv_ready: boolean;
        python_ready: boolean;
        worker_ready: boolean;
        manifest_current: boolean;
        playwright_ready: boolean;
        system_browser_ready: boolean;
        ocr_enabled: boolean;
        ocr_ready: boolean;
        stage: string;
        progress: { phase: string; percent: number; message: string } | null;
        last_error: string | null;
      }
    >("/api/environment/bootstrap", null, { timeout: 650000, ...opts }),
  fetchStatus: async (): Promise<EnvironmentStatus | null> => {
    const data = await http.get<InitStatus>("/api/init-status");
    return (data as InitStatus & { environment?: EnvironmentStatus }).environment ?? null;
  },
};

/** 配置方案 */
export const profilesApi = {
  list: () => http.get<ProfileListResponse>("/api/profiles"),
  get: (id: string) => http.get<{ settings: Profile }>(`/api/profiles/${pathSegment(id)}`),
  // 新建方案：POST /api/profiles/{id}，body 必含 id/name/username/password；
  // 可选设置字段（auth_url/trigger_url/isp/gateway_ip/wifi_ssid/active_task）与 PUT 同语义
  create: (
    id: string,
    payload: {
      id: string;
      name: string;
      username: string;
      password: string;
      auth_url?: string;
      trigger_url?: string;
      isp?: string;
      gateway_ip?: string;
      wifi_ssid?: string;
      active_task?: string;
    },
  ) => http.post<MutationResult>(`/api/profiles/${pathSegment(id)}`, payload),
  save: (id: string, payload: Profile) => http.put<MutationResult>(`/api/profiles/${pathSegment(id)}`, payload),
  delete: (id: string) => http.delete<MutationResult>(`/api/profiles/${pathSegment(id)}`),
  setActive: (id: string) => http.post<MutationResult>("/api/profiles/switch", { profile_id: id }),
  detect: () => http.post<NetworkDetectResult>("/api/profiles/detect"),
  toggleAutoSwitch: (enabled: boolean) =>
    http.post<MutationResult & { active_profile?: string }>("/api/profiles/auto-switch", { enabled }),
};

/** 开机自启动 */
export const autostartApi = {
  fetchStatus: () => http.get<AutostartStatus>("/api/autostart/status"),
  toggle: (enable: boolean) => http.post<MutationResult>(`/api/autostart/${enable ? "enable" : "disable"}`),
};

/** 卸载 */
export const uninstallApi = {
  detect: () => http.get<UninstallDetectItem[]>("/api/uninstall/detect"),
  // 删除 ms-playwright 浏览器缓存可能耗时较长（数百 MB），放宽客户端超时
  uninstall: () =>
    http.post<UninstallResponse>("/api/uninstall", null, { timeout: 300000 }),
};

/** OCR */
export const ocrApi = {
  fetchStatus: () => http.get<OcrStatus>("/api/ocr/status"),
  install: () => http.post<MutationResult>("/api/ocr/install"),
  uninstall: () => http.post<MutationResult>("/api/ocr/uninstall"),
  // 识别 base64 图片中的文本，返回 { text }
  // 首次构造 ddddocr 需加载 onnx 模型（可达 OCR_TIMEOUT_SECS=90s），
  // 给客户端一个略大于后端的超时，确保等待过长时前端能报错而非无限转圈
  recognize: (payload: { image_base64: string; old?: boolean }) =>
    http.post<{ text: string }>("/api/ocr/recognize", payload, { timeout: 120000 }),
};

/** AI 任务生成（LLM 配置 + 登录页捕获 + 生成任务 JSON） */
export const aiApi = {
  fetchLlmConfig: () => http.get<AiLlmConfig>("/api/ai/llm-config"),
  // api_key 缺省=保持不变；空串=清除；非空=更新
  saveLlmConfig: (payload: { base_url: string; model: string; api_key?: string }) =>
    http.put<AiLlmConfig>("/api/ai/llm-config", payload),
  // 捕获含导航 + networkidle 等待 + CDP 资源快照，放宽客户端超时
  capture: (url: string) =>
    http.post<AiCaptureResult>("/api/ai/capture", { url }, { timeout: 90000 }),
  // 截图经 <img> 直接引用（GET 免鉴权），不走 http 封装
  captureScreenshotUrl: () => `/api/ai/capture/screenshot?t=${Date.now()}`,
  /** 查询最近一次捕获产物是否可用（页面刷新后恢复状态用） */
  captureStatus: () =>
    http.get<{
      available: boolean;
      request_url?: string;
      final_url?: string;
      title?: string;
    }>("/api/ai/capture/status"),
  /** 保存页面文件：MHTML 完整布局 + HTML + CSS/JS 资源 + 截图（后端打 zip，返回 Blob） */
  async captureBundle(): Promise<Blob> {
    const token = await ensureAuthToken();
    return fetchBundleWithTimeout("/api/ai/capture/bundle", {
      method: "GET",
      headers: token ? { "X-Auth-Token": token } : undefined,
    });
  },
  /** 流式生成（SSE）：增量推送，空闲超时语义（有输出自动续命，仅连续无内容达阈值才超时，最大 10 分钟） */
  async generateStream(
    payload: { extra_prompt?: string },
    opts: {
      onEvent: (ev: Record<string, unknown>) => void;
      signal?: AbortSignal;
      /** 空闲超时 ms，默认 600_000（10 分钟），有内容即重置，仅无内容连续达阈值才超时 */
      idleTimeoutMs?: number;
    },
  ): Promise<void> {
    const idleMs = opts.idleTimeoutMs ?? 600_000;
    const token = await ensureAuthToken();
    const controller = new AbortController();
    const onCallerAbort = () => controller.abort((opts.signal as unknown as { reason?: unknown })?.reason);
    if (opts.signal) {
      if (opts.signal.aborted) onCallerAbort();
      else opts.signal.addEventListener("abort", onCallerAbort, { once: true });
    }
    let idleTimer: ReturnType<typeof setTimeout> | null = null;
    const resetIdle = () => {
      if (idleTimer) clearTimeout(idleTimer);
      idleTimer = setTimeout(() => controller.abort(new DOMException("空闲超时（连续无内容达阈值）", "AbortError")), idleMs);
    };
    const clearIdle = () => { if (idleTimer) clearTimeout(idleTimer); idleTimer = null; };
    resetIdle();
    try {
      const res = await fetch("/api/ai/generate/stream", {
        method: "POST",
        headers: {
          "Content-Type": "application/json",
          ...(token ? { "X-Auth-Token": token } : {}),
        },
        body: JSON.stringify(payload ?? {}),
        signal: controller.signal,
      });
      if (!res.ok) {
        const text = await res.text().catch(() => "");
        let msg = `生成失败 (${res.status})`;
        try { const j = JSON.parse(text) as { error?: { message?: string } }; if (j?.error?.message) msg = j.error.message; } catch { if (text) msg = text.slice(0, 400); }
        throw new Error(msg);
      }
      if (!res.body) throw new Error("浏览器不支持流式响应（ReadableStream 缺失）");
      const reader = res.body.getReader();
      const decoder = new TextDecoder();
      let buf = "";
      while (true) {
        const { done, value } = await reader.read();
        if (done) break;
        buf += decoder.decode(value, { stream: true });
        // 按 SSE 帧分割（\n\n）
        let idx: number;
        while ((idx = buf.indexOf("\n\n")) !== -1) {
          const frame = buf.slice(0, idx);
          buf = buf.slice(idx + 2);
          const lines = frame.split("\n");
          for (const raw of lines) {
            const line = raw.trim();
            if (!line || line.startsWith(":")) continue;
            if (!line.startsWith("data:")) continue;
            const data = line.slice(5).trim();
            if (!data) continue;
            try {
              const ev = JSON.parse(data) as Record<string, unknown>;
              // 仅真实数据帧续命空闲计时：后端 ": keepalive" 注释帧只证明连接存活，
              // 不代表有内容——否则任何 ≥ keepalive 间隔的空闲阈值都永远打不到
              resetIdle();
              opts.onEvent(ev);
            } catch { /* ignore non-json keepalive */ }
          }
        }
      }
      // 处理尾部残留
      const tail = buf.trim();
      if (tail.startsWith("data:")) {
        try { opts.onEvent(JSON.parse(tail.slice(5).trim()) as Record<string, unknown>); } catch {}
      }
    } catch (e) {
      if (e instanceof DOMException && e.name === "AbortError") {
        const reason = (e as DOMException).message || "";
        if (reason.includes("空闲超时")) throw new Error(`空闲超时（>${Math.round(idleMs/1000)}s 无输出），请检查网络或稍后重试`);
        // 调用方主动取消：静默向上抛 AbortError 语义
        throw e;
      }
      throw e;
    } finally {
      clearIdle();
      opts.signal?.removeEventListener("abort", onCallerAbort);
    }
  },
};

/** 登录历史 */
export const historyApi = {
  fetch: (limit: number) => http.get<LoginHistoryItem[]>(`/api/history?limit=${limit}`),
  clear: () => http.delete<MutationResult>("/api/history"),
};

/** 浏览器 */
export const browsersApi = {
  fetch: () => http.get<BrowserListResponse>("/api/browsers"),
  installPlaywright: (browser = "chromium", opts?: { signal?: AbortSignal; timeout?: number }) =>
    http.post<MutationResult & { browser?: string }>(
      `/api/install/playwright?browser=${encodeURIComponent(browser)}`,
      null,
      opts,
    ),
};

/** Worker（浏览器进程） */
export const workerApi = {
  stop: () => http.post<MutationResult>("/api/worker/stop"),
};

/** 调试 */
export const debugApi = {
  start: (taskId: string) => http.post<DebugSession>("/api/debug/start", { task_id: taskId }),
  next: () => http.post<DebugSession>("/api/debug/step"),
  runAll: () => http.post<DebugSession>("/api/debug/run-all"),
  stop: () => http.post<MutationResult>("/api/debug/stop"),
  status: () =>
    http.get<{ active: boolean; screenshot_url?: string; session?: DebugSession }>(
      "/api/debug/status",
    ),
  /** 导出问题报告：日志+活动任务+页面 MHTML/截图（后端打 zip，返回 Blob） */
  async feedbackBundle(): Promise<Blob> {
    const token = await ensureAuthToken();
    return fetchBundleWithTimeout("/api/debug/feedback-bundle", {
      method: "POST",
      headers: token ? { "X-Auth-Token": token } : undefined,
    });
  },
};

/** 远程仓库 */
export const repoApi = {
  fetchIndex: (url: string) => http.get<RepoTask[]>(`/api/repo/fetch?url=${encodeURIComponent(url)}`),
  fetchTask: (url: string) => http.get<Record<string, unknown>>(`/api/repo/task?url=${encodeURIComponent(url)}`),
  // 仓库任务截图经 <img> 直接引用（GET 免鉴权），不走 http 封装；
  // 后端代理复用更新器代理配置并限死任务站 raw 域（防开放 SSRF 出口）
  screenshotUrl: (rawUrl: string) => `/api/repo/image?url=${encodeURIComponent(rawUrl)}`,
};

/** 纯模式 */
export const pureModeApi = {
  fetch: () => http.get<{ enabled: boolean }>("/api/pure-mode"),
  toggle: () => http.post<{ enabled: boolean; message?: string }>("/api/pure-mode"),
};

/** 外观/背景 */
export const backgroundApi = {
  upload: (file: File) => {
    const form = new FormData();
    form.append("file", file);
    return http.post<BackgroundUploadResult>("/api/background/upload", form);
  },
  fetchUrl: (url: string) =>
    http.post<BackgroundUploadResult>("/api/background/fetch-url", { url }),
  remove: (filename: string) => http.delete<MutationResult>(`/api/background/${pathSegment(filename)}`),
};

/** 脚本 */
export const scriptsApi = {
  get: (id: string) => http.get<Script>(`/api/scripts/${pathSegment(id)}`),
  binaries: () => http.get<BinaryInfo[]>("/api/scripts/binaries"),
  save: (id: string, payload: { name: string; description: string; content: string; binary_path: string }) =>
    http.put<MutationResult>(`/api/scripts/${pathSegment(id)}`, payload),
  delete: (id: string) => http.delete<MutationResult>(`/api/scripts/${pathSegment(id)}`),
  run: (id: string) => http.post<MutationResult>("/api/scripts/run", { task_id: id }),
};

/** 任务（浏览器任务） */
export const tasksApi = {
  list: () => http.get<TaskItem[]>("/api/tasks"),
  get: (id: string) => http.get<TaskDetail>(`/api/tasks/${pathSegment(id)}`),
  active: () => http.get<{ task_id: string }>("/api/tasks/active"),
  save: (id: string, payload: Record<string, unknown>) => http.put<MutationResult>(`/api/tasks/${pathSegment(id)}`, payload),
  delete: (id: string) => http.delete<MutationResult>(`/api/tasks/${pathSegment(id)}`),
  setActive: (id: string) => http.post<MutationResult>(`/api/tasks/active/${pathSegment(id)}`),
  execute: (id: string) => http.post<MutationResult>(`/api/tasks/${pathSegment(id)}/execute`),
  order: (order: { all: string[]; scripts: string[] }) => http.post<MutationResult>("/api/tasks/order", order),
  import: (payload: unknown) => http.post<MutationResult & { imported?: number }>("/api/tasks/import", payload),
  export: (id: string) => http.get<Record<string, unknown>>(`/api/tasks/export/${pathSegment(id)}`),
};

/** 定时任务 */
export const scheduledTasksApi = {
  list: () => http.get<ScheduledTask[]>("/api/scheduler/jobs"),
  create: (payload: ScheduledTask) => http.post<MutationResult>("/api/scheduler/jobs", payload),
  update: (id: string, payload: ScheduledTask) => http.put<MutationResult>(`/api/scheduler/jobs/${id}`, payload),
  delete: (id: string) => http.delete<MutationResult>(`/api/scheduler/jobs/${id}`),
  toggle: (id: string) => http.post<MutationResult & { enabled: boolean }>(`/api/scheduler/jobs/${id}/toggle`),
  run: (id: string) => http.post<MutationResult & { run_id: string }>(`/api/scheduler/jobs/${id}/run`),
  history: (id: string) => http.get<{ runs: ScheduledTaskHistoryItem[] } | ScheduledTaskHistoryItem[]>(`/api/scheduler/jobs/${id}/history`),
};
