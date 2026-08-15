/**
 * 浏览器任务状态与操作（单例）。
 * 替代原 taskData + tasks/core.js + tasks/editor.js（浏览器任务部分）+ tasks/debug.js（部分）。
 */

import { ref, computed } from "vue";
import type { TaskItem, DangerStep, RepoTask, TaskConfig } from "../api/types";
import { tasksApi, repoApi, pureModeApi } from "../api";
import { extractApiError } from "../api/client";
import { frontendLogger } from "../utils/logger";
import { downloadBlob, pickFile } from "../utils/file";
import { useToast } from "./useToast";
import { useConfirm } from "./useConfirm";

export interface BrowserTaskDraft {
  id: string;
  name: string;
  description: string;
  url: string;
  json: string;
  _isNew: boolean;
}

// 危险步骤类型：涵盖后端兼容的多个别名。StepEditor 实际产出 `evaluate`（执行 JS），
// eval / custom_js 为历史别名，三者均需视为危险（历史遗留：前端危险步骤检测失效）
const DANGEROUS_STEP_TYPES = new Set(["eval", "custom_js", "evaluate"]);

const tasks = ref<TaskItem[]>([]);
const activeTaskId = ref("default");
const editingTask = ref<BrowserTaskDraft | null>(null);
const editingTaskType = ref<"browser" | "script">("browser");
const jsonError = ref("");

const pureMode = ref(true);
const pureModeLoading = ref(false);

// ==================== 仓库导入 ====================
const repoImport = ref({
  visible: false,
  url: "https://raw.githubusercontent.com/Misyra/campus-auth-tasks/master/index.json",
  source: "github" as "github" | "gitee" | "custom",
  loading: false,
  error: "",
  tasks: [] as RepoTask[],
  searchQuery: "",
  disclaimer: null as RepoTask | null,
});

const filteredRepoTasks = computed(() => {
  const q = repoImport.value.searchQuery.trim().toLowerCase();
  if (!q) return repoImport.value.tasks;
  return repoImport.value.tasks.filter((t) => {
    const searchable = [t.name, t.description, t.author, ...(t.tags || [])].filter(Boolean).join(" ").toLowerCase();
    return searchable.includes(q);
  });
});

const { toastOnly } = useToast();
const { confirm } = useConfirm();

async function fetchTasks(): Promise<void> {
  try {
    const data = await tasksApi.list();
    if (Array.isArray(data)) {
      // GET /api/tasks 与 /api/scripts 返回同一混合列表，此处仅保留浏览器任务（browser）
      const browserTasks = data.filter((t) => {
        const tt = (t.task_type as string) || (t.type as string) || "";
        return tt === "" || tt === "browser";
      });
      tasks.value.splice(0, tasks.value.length, ...browserTasks);
    }
  } catch (error) {
    frontendLogger.error("tasks", "获取任务列表失败", error);
  }
}

async function fetchActiveTask(): Promise<void> {
  try {
    const data = await tasksApi.active();
    activeTaskId.value = data.task_id;
  } catch (error) {
    frontendLogger.error("tasks", "获取活动任务失败", error);
  }
}

/** 仅同步本地活动任务 id（不调 API），供已自行完成服务端切换的调用方复用。 */
async function setActiveTask(taskId: string): Promise<void> {
  try {
    frontendLogger.info("tasks", `设置活动任务: ${taskId}`);
    await tasksApi.setActive(taskId);
    activeTaskId.value = taskId;
    frontendLogger.info("tasks", `活动任务已设置: ${taskId}`);
  } catch (error) {
    frontendLogger.error("tasks", "设置活动任务异常", error);
    toastOnly(false, "设置活动任务失败");
  }
}

/** 立即执行任务（通用语义：浏览器打卡/脚本/Shell，不注入账号密码） */
async function executeTask(taskId: string): Promise<void> {
  try {
    frontendLogger.info("tasks", `执行任务: ${taskId}`);
    const data = await tasksApi.execute(taskId);
    toastOnly(true, extractApiError(data, "执行完成"));
  } catch (error) {
    frontendLogger.error("tasks", "执行任务异常", error);
    toastOnly(false, extractApiError(error, "执行失败"));
  }
}

function detectDangerousSteps(config: { steps?: Array<Record<string, unknown>> }): DangerStep[] {
  const steps = config.steps || [];
  const warnings: DangerStep[] = [];
  for (let i = 0; i < steps.length; i++) {
    const step = steps[i];
    const type = (step.type as string) || "";
    if (DANGEROUS_STEP_TYPES.has(type)) {
      const desc = (step.description as string) || (step.id as string) || `步骤 ${i + 1}`;
      const code = String((step.script as string) || (step.extra as { script?: string })?.script || "");
      warnings.push({ stepIndex: i + 1, stepType: type, description: desc, code: code.slice(0, 2000) });
    }
  }
  return warnings;
}

async function saveTask(): Promise<void> {
  if (!editingTask.value || !editingTask.value.id) {
    toastOnly(false, "请输入任务ID");
    return;
  }
  if (!/^[a-zA-Z][a-zA-Z0-9_]*$/.test(editingTask.value.id)) {
    toastOnly(false, "任务ID必须以字母开头，且只能包含字母、数字和下划线");
    return;
  }
  let config: Record<string, unknown>;
  try {
    config = JSON.parse(editingTask.value.json);
  } catch (e) {
    jsonError.value = (e as Error).message;
    toastOnly(false, "JSON 格式错误: " + (e as Error).message);
    return;
  }
  const payload = { ...config };
  payload.name = editingTask.value.name || (config.name as string);
  payload.description = editingTask.value.description || (config.description as string);
  payload.url = editingTask.value.url || (config.url as string) || "{{LOGIN_URL}}";
  // 确保 type 字段存在（后端 TaskKind 反序列化需要）
  if (!payload.type) {
    payload.type = editingTaskType.value || "browser";
  }
  delete payload.version;
  delete payload.source;

  const dangers = detectDangerousSteps(payload as { steps?: Array<Record<string, unknown>> });
  if (dangers.length > 0) {
    const ok = await confirm({
      title: "检测到危险步骤",
      message: `任务包含 ${dangers.length} 个危险步骤（eval/custom_js），确定要继续保存吗？`,
      danger: true,
    });
    if (!ok) return;
  }

  try {
    const data = await tasksApi.save(editingTask.value.id, payload);
    frontendLogger.info("tasks", data?.message || "任务保存成功");
    editingTask.value = null;
    jsonError.value = "";
    await fetchTasks();
  } catch (error) {
    frontendLogger.error("tasks", "保存任务失败", error);
    toastOnly(false, extractApiError(error, "保存失败"));
  }
}

async function deleteTask(taskId: string): Promise<void> {
  const ok = await confirm({
    title: "删除任务",
    message: "确定要删除这个任务吗？",
    danger: true,
  });
  if (!ok) return;
  try {
    await tasksApi.delete(taskId);
    frontendLogger.info("tasks", "任务删除成功: " + taskId);
    toastOnly(true, "任务已删除");
    await fetchTasks();
    if (activeTaskId.value === taskId) {
      activeTaskId.value = "default";
      await setActiveTask("default");
    }
  } catch (error) {
    frontendLogger.error("tasks", "删除任务异常", error);
    toastOnly(false, "删除任务失败");
  }
}

async function showTaskEditor(taskId?: string): Promise<void> {
  if (taskId) {
    try {
      const data = await tasksApi.get(taskId);
      // 后端返回 TaskDetail: { summary: { id, name, description, task_type }, config: {...} }
      const summary = data.summary;
      const taskConfig: TaskConfig = data.config ?? {};
      const taskType = summary?.task_type || taskConfig.type;

      if (taskType === "script") {
        // 脚本任务交给脚本编辑器处理
        await (await import("./useScripts")).useScripts().showScriptEditor(taskId);
        return;
      }
      editingTask.value = {
        id: taskId,
        name: summary?.name || taskConfig.name || "",
        description: summary?.description || taskConfig.description || "",
        url: taskConfig.url || "",
        json: JSON.stringify(taskConfig, null, 2),
        _isNew: false,
      };
      jsonError.value = "";
    } catch (error) {
      frontendLogger.error("tasks", "加载任务失败: " + taskId, error);
      toastOnly(false, "加载任务失败");
    }
  } else {
    editingTask.value = { id: "", name: "", description: "", url: "", json: "", _isNew: true };
    jsonError.value = "";
  }
}

function syncMetaToJson(): void {
  if (!editingTask.value) return;
  try {
    const parsed = JSON.parse(editingTask.value.json);
    parsed.name = editingTask.value.name;
    parsed.description = editingTask.value.description;
    editingTask.value.json = JSON.stringify(parsed, null, 2);
    jsonError.value = "";
  } catch {
    /* JSON 无效时不同步 */
  }
}

function syncJsonToMeta(): void {
  if (!editingTask.value) return;
  try {
    const parsed = JSON.parse(editingTask.value.json);
    if ("name" in parsed) editingTask.value.name = (parsed.name as string) || "";
    if ("description" in parsed) editingTask.value.description = (parsed.description as string) || "";
    jsonError.value = "";
  } catch {
    /* JSON 无效时不同步 */
  }
}

async function loadTemplate(templateId: string): Promise<void> {
  if (!editingTask.value) return;
  try {
    const data = await tasksApi.get(templateId);
    const summary = data.summary;
    const taskConfig: TaskConfig = data.config ?? {};
    editingTask.value.json = JSON.stringify(taskConfig, null, 2);
    const name = summary?.name || taskConfig.name || "";
    if (name) editingTask.value.name = name;
    const desc = summary?.description || taskConfig.description || "";
    if (desc) editingTask.value.description = desc;
    jsonError.value = "";
  } catch (error) {
    frontendLogger.error("tasks", "加载模板失败: " + templateId, error);
    toastOnly(false, "加载模板失败");
  }
}

function validateJson(): void {
  if (!editingTask.value || !editingTask.value.json.trim()) {
    jsonError.value = "";
    return;
  }
  try {
    JSON.parse(editingTask.value.json);
    jsonError.value = "";
  } catch (e) {
    jsonError.value = (e as Error).message;
  }
}

function formatJson(): void {
  if (!editingTask.value) return;
  try {
    const parsed = JSON.parse(editingTask.value.json);
    editingTask.value.json = JSON.stringify(parsed, null, 2);
    jsonError.value = "";
  } catch (e) {
    frontendLogger.warn("tasks", "JSON 格式化失败: " + (e as Error).message);
    toastOnly(false, "JSON 格式错误，无法格式化");
  }
}

async function duplicateTask(taskId: string): Promise<void> {
  try {
    const data = await tasksApi.get(taskId);
    // 解包 TaskDetail 嵌套结构
    const summary = data.summary;
    const taskConfig: TaskConfig = data.config ?? {};

    const baseId = taskId.replace(/_copy(_\d+)?$/, "");
    const existingIds = new Set((tasks.value || []).map((t) => t.id));
    const baseName = (summary?.name || taskConfig.name || "").replace(/\s*\(副本\)(\s*\d+)?$/, "");
    let newId = baseId + "_copy";
    let suffix = " (副本)";
    let counter = 2;
    while (existingIds.has(newId)) {
      newId = baseId + "_copy_" + counter;
      suffix = ` (副本${counter})`;
      counter++;
    }
    editingTask.value = {
      id: newId,
      name: baseName + suffix,
      description: summary?.description || taskConfig.description || "",
      url: taskConfig.url || "",
      json: JSON.stringify(taskConfig, null, 2),
      _isNew: true,
    };
    jsonError.value = "";
  } catch (error) {
    frontendLogger.error("tasks", "复制任务失败: " + taskId, error);
    toastOnly(false, "复制任务失败");
  }
}

async function exportTask(taskId: string): Promise<void> {
  try {
    // 经后端导出端点获取完整任务配置（与 /api/tasks/import 格式对应）
    const data = await tasksApi.export(taskId);
    downloadBlob(JSON.stringify(data, null, 2), `${taskId}.json`, "application/json");
    frontendLogger.info("tasks", "任务已导出");
  } catch (error) {
    frontendLogger.error("tasks", "导出任务失败", error);
    toastOnly(false, extractApiError(error, "导出失败"));
  }
}

async function importTask(): Promise<void> {
  const file = await pickFile(".json");
  if (!file) return;
  try {
    const text = await file.text();
    const data = JSON.parse(text);
    const payload = Array.isArray(data) ? data : [data];
    const result = await tasksApi.import(payload);
    toastOnly(true, `已导入 ${result?.imported ?? payload.length} 个任务`);
    await fetchTasks();
  } catch (e) {
    frontendLogger.warn("tasks", "导入失败: " + (e as Error).message);
    toastOnly(false, "导入失败：" + extractApiError(e, "文件不是有效的任务 JSON"));
  }
}

async function fetchPureMode(): Promise<void> {
  try {
    const data = await pureModeApi.fetch();
    pureMode.value = data.enabled;
  } catch {
    /* 保持默认值 */
  }
}

async function togglePureMode(): Promise<void> {
  if (pureModeLoading.value) return;
  pureModeLoading.value = true;
  try {
    const data = await pureModeApi.toggle();
    const enabled = data?.enabled ?? false;
    pureMode.value = enabled;
    frontendLogger.info("tasks", `纯净模式已${enabled ? "开启" : "关闭"}`);
    toastOnly(true, `纯净模式已${enabled ? "开启" : "关闭"}`);
  } catch (error) {
    pureMode.value = !pureMode.value;
    frontendLogger.error("tasks", "切换纯净模式失败", error);
    toastOnly(false, "切换纯净模式失败");
  } finally {
    pureModeLoading.value = false;
  }
}

// ==================== 仓库导入 ====================

function selectRepoSource(source: "github" | "gitee" | "custom") {
  repoImport.value.source = source;
  if (source === "github") {
    repoImport.value.url = "https://raw.githubusercontent.com/Misyra/campus-auth-tasks/master/index.json";
  } else if (source === "gitee") {
    repoImport.value.url = "https://raw.giteeusercontent.com/Misyra/campus-auth-tasks/raw/master/index.gitee.json";
  }
}

function showRepoImport() {
  repoImport.value.visible = true;
  repoImport.value.error = "";
  repoImport.value.tasks = [];
  repoImport.value.searchQuery = "";
  repoImport.value.loading = false;
  repoImport.value.disclaimer = null;
}

function closeRepoImport() {
  repoImport.value.visible = false;
}

async function fetchRepoIndex() {
  const url = repoImport.value.url.trim();
  if (!url) {
    repoImport.value.error = "请输入索引地址";
    return;
  }
  repoImport.value.loading = true;
  repoImport.value.error = "";
  repoImport.value.tasks = [];
  repoImport.value.searchQuery = "";
  try {
    const data = await repoApi.fetchIndex(url);
    if (!Array.isArray(data) || data.length === 0) {
      repoImport.value.error = "索引为空或格式不正确";
      return;
    }
    repoImport.value.tasks = data;
  } catch (e) {
    const msg = extractApiError(e, "加载失败，请检查地址是否正确");
    repoImport.value.error = msg;
    toastOnly(false, `获取远程索引失败: ${msg}`);
  } finally {
    repoImport.value.loading = false;
  }
}

function confirmRepoImport(task: RepoTask) {
  repoImport.value.disclaimer = task;
}

function cancelRepoDisclaimer() {
  repoImport.value.disclaimer = null;
}

async function acceptRepoDisclaimer() {
  const task = repoImport.value.disclaimer;
  repoImport.value.disclaimer = null;
  if (!task) return;

  try {
    const data = await repoApi.fetchTask(task.url);
    let id = (task.id || (data.name as string) || "imported").replace(/[^A-Za-z0-9_]/g, "_");
    if (/^[0-9]/.test(id)) {
      id = "task_" + id;
    }
    editingTask.value = {
      id,
      name: (data.name as string) || task.name || "",
      description: (data.description as string) || task.description || "",
      url: (data.url as string) || "",
      json: JSON.stringify(data, null, 2),
      _isNew: true,
    };
    editingTaskType.value = "browser";
    jsonError.value = "";
    closeRepoImport();
    frontendLogger.info("tasks", `已从仓库导入: ${task.name}`);
    toastOnly(true, `已导入「${task.name}」，请在右侧编辑器内确认后保存`);
  } catch (e) {
    const msg = extractApiError(e, "下载任务失败");
    frontendLogger.error("tasks", "远程任务下载失败", msg);
    toastOnly(false, `远程任务下载失败: ${msg}`);
  }
}

export function useTasks() {
  return {
    tasks,
    activeTaskId,
    editingTask,
    editingTaskType,
    jsonError,
    pureMode,
    pureModeLoading,
    fetchTasks,
    fetchActiveTask,
    setActiveTask,
    executeTask,
    saveTask,
    deleteTask,
    showTaskEditor,
    syncMetaToJson,
    syncJsonToMeta,
    loadTemplate,
    validateJson,
    formatJson,
    duplicateTask,
    exportTask,
    importTask,
    fetchPureMode,
    togglePureMode,
    repoImport,
    filteredRepoTasks,
    selectRepoSource,
    showRepoImport,
    closeRepoImport,
    fetchRepoIndex,
    confirmRepoImport,
    cancelRepoDisclaimer,
    acceptRepoDisclaimer,
  };
}
