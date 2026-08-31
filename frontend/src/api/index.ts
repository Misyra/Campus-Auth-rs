/**
 * API 端点函数集合。
 * 所有调用统一走此处，禁止在业务代码中直接 fetch。
 * 每个端点返回 client.ts 解包 .data 后的纯业务负载（成功时）。
 * 路径与后端 openapi.json 保持一致。
 */

import { http } from "./client";
import type { RequestOptions } from "./client";
import type {
  AutostartStatus,
  BackgroundUploadResult,
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
  OcrStatus,
  Profile,
  ProfileListResponse,
  RepoTask,
  SaveConfigPayload,
  ScheduledTask,
  ScheduledTaskHistoryItem,
  Script,
  TaskDetail,
  TaskItem,
  UpdateInfo,
} from "./types";

export { ApiError, extractApiError } from "./client";

/** 配置相关 */
export const configApi = {
  fetch: () => http.get<ConfigResponse>("/api/config"),
  // B4：原 PUT /api/config 全量保存方法已删除（全库零调用，且扁平整体替换
  // 的载荷形状容易在后端演变为清空配置的地雷）；保存统一走 patch 增量语义
  patch: (payload: SaveConfigPayload, opts?: RequestOptions) => http.patch<MutationResult>("/api/config", payload, opts),
  fetchDefaults: () => http.get<ConfigResponse>("/api/config/defaults"),
  fetchLogLevels: () => http.get<{ level: string }>("/api/config/log-levels"),
  setLogLevel: (level: string) => http.put<MutationResult>("/api/config/log-level", { level }),
  fetchStealthScript: () => http.get<{ script: string }>("/api/config/default-stealth-script"),
  reload: () => http.post<MutationResult>("/api/config/reload"),
};

/** 监控与登录操作 */
export const monitorApi = {
  fetchStatus: () => http.get<import("./types").StatusSnapshot>("/api/monitor/status"),
  start: () => http.post<MutationResult>("/api/monitor/start"),
  stop: () => http.post<MutationResult>("/api/monitor/stop"),
};

/** 一次性操作 */
export const actionsApi = {
  login: (timeoutMs: number) =>
    http.post<MutationResult>("/api/login", null, { timeout: timeoutMs }),
  cancelLogin: () => http.post<MutationResult>("/api/login/cancel"),
  testNetwork: () => http.post<MutationResult>("/api/monitor/test", null, { timeout: 5000 }),
};

/** 系统 */
export const systemApi = {
  health: () => http.get<HealthInfo>("/api/health"),
  initStatus: () => http.get<InitStatus>("/api/init-status"),
  checkUpdate: () => http.get<UpdateInfo>("/api/check-update"),
  agree: () => http.post<MutationResult>("/api/agree"),
  shutdown: () => http.post<MutationResult>("/api/system/shutdown"),
  update: () => http.post<MutationResult & { message?: string; version?: string }>("/api/system/update"),
  fetchLogs: (limit: number) => http.get<LogEntry[]>(`/api/logs?limit=${limit}`),
};

/** 环境初始化（uv sync + Chromium，POST /api/environment/bootstrap） */
export const environmentApi = {
  bootstrap: (opts?: RequestOptions) =>
    http.post<
      MutationResult & {
        capability_ready: boolean;
        uv_ready: boolean;
        python_ready: boolean;
        playwright_ready: boolean;
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
  get: (id: string) => http.get<{ settings: Profile }>(`/api/profiles/${id}`),
  // 新建方案：POST /api/profiles/{id}，body 必含 id/name/username/password（对齐后端 ProfileCreateBody 必填字段）
  create: (id: string, payload: { id: string; name: string; username: string; password: string }) =>
    http.post<MutationResult>(`/api/profiles/${id}`, payload),
  save: (id: string, payload: Profile) => http.put<MutationResult>(`/api/profiles/${id}`, payload),
  delete: (id: string) => http.delete<MutationResult>(`/api/profiles/${id}`),
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
};

/** 远程仓库 */
export const repoApi = {
  fetchIndex: (url: string) => http.get<RepoTask[]>(`/api/repo/fetch?url=${encodeURIComponent(url)}`),
  fetchTask: (url: string) => http.get<Record<string, unknown>>(`/api/repo/task?url=${encodeURIComponent(url)}`),
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
  remove: (filename: string) => http.delete<MutationResult>(`/api/background/${filename}`),
};

/** 脚本 */
export const scriptsApi = {
  list: () => http.get<Script[]>("/api/scripts"),
  get: (id: string) => http.get<Script>(`/api/scripts/${id}`),
  binaries: () => http.get<import("./types").BinaryInfo[]>("/api/scripts/binaries"),
  save: (id: string, payload: { name: string; description: string; content: string; binary_path: string }) =>
    http.put<MutationResult>(`/api/scripts/${id}`, payload),
  delete: (id: string) => http.delete<MutationResult>(`/api/scripts/${id}`),
  run: (id: string) => http.post<MutationResult>("/api/scripts/run", { task_id: id }),
};

/** 任务（浏览器任务） */
export const tasksApi = {
  list: () => http.get<TaskItem[]>("/api/tasks"),
  get: (id: string) => http.get<TaskDetail>(`/api/tasks/${id}`),
  active: () => http.get<{ task_id: string }>("/api/tasks/active"),
  save: (id: string, payload: Record<string, unknown>) => http.put<MutationResult>(`/api/tasks/${id}`, payload),
  delete: (id: string) => http.delete<MutationResult>(`/api/tasks/${id}`),
  setActive: (id: string) => http.post<MutationResult>(`/api/tasks/active/${id}`),
  execute: (id: string) => http.post<MutationResult>(`/api/tasks/${id}/execute`),
  order: (order: { all: string[]; scripts: string[] }) => http.post<MutationResult>("/api/tasks/order", order),
  import: (payload: unknown) => http.post<MutationResult & { imported?: number }>("/api/tasks/import", payload),
  export: (id: string) => http.get<Record<string, unknown>>(`/api/tasks/export/${id}`),
};

/** 定时任务 */
export const scheduledTasksApi = {
  list: () => http.get<ScheduledTask[]>("/api/scheduler/jobs"),
  get: (id: string) => http.get<ScheduledTask>(`/api/scheduler/jobs/${id}`),
  create: (payload: ScheduledTask) => http.post<MutationResult>("/api/scheduler/jobs", payload),
  update: (id: string, payload: ScheduledTask) => http.put<MutationResult>(`/api/scheduler/jobs/${id}`, payload),
  delete: (id: string) => http.delete<MutationResult>(`/api/scheduler/jobs/${id}`),
  toggle: (id: string) => http.post<MutationResult & { enabled: boolean }>(`/api/scheduler/jobs/${id}/toggle`),
  run: (id: string) => http.post<MutationResult & { run_id: string }>(`/api/scheduler/jobs/${id}/run`),
  history: (id: string) => http.get<{ runs: ScheduledTaskHistoryItem[] } | ScheduledTaskHistoryItem[]>(`/api/scheduler/jobs/${id}/history`),
};
