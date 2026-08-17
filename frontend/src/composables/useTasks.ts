/**
 * 浏览器任务状态与操作（单例）。
 * 替代原 taskData + tasks/core.js + tasks/editor.js（浏览器任务部分）+ tasks/debug.js（部分）。
 */

import { ref, reactive } from "vue";
import type { TaskItem, DangerStep, TaskConfig } from "../api/types";
import { tasksApi } from "../api";
import { extractApiError } from "../api/client";
import { frontendLogger } from "../utils/logger";
import { downloadBlob, pickFile } from "../utils/file";
import { useToast } from "./useToast";
import { useConfirm } from "./useConfirm";
import { useScripts } from "./useScripts";

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

// A11：执行类操作 busy 守卫（响应式 Set），防止连点重复提交
const executingIds = reactive(new Set<string>()); // executeTask 执行中
const duplicatingIds = reactive(new Set<string>()); // duplicateTask 复制中
const exportingIds = reactive(new Set<string>()); // exportTask 导出中

const { toastOnly } = useToast();
const { confirm } = useConfirm();

// F3：首次失败 toast 通知（参照 useStatus.fetchStatus 的首败 notify 模式）
const fetchTasksFailCount = ref(0);

// P15：5 秒内已成功拉取则跳过（useUi.init 已拉全部数据，View mount / 路由往返
// 不再重复请求）。lastFetchAt 仅成功后更新（失败不更新以便重试）；
// force: true 供变更后刷新 / 重连回调等显式刷新场景绕过守卫。
let lastFetchAt = 0;

async function fetchTasks(force = false): Promise<void> {
  if (!force && Date.now() - lastFetchAt < 5000) return;
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
    lastFetchAt = Date.now();
    if (fetchTasksFailCount.value > 0) fetchTasksFailCount.value = 0;
  } catch (error) {
    fetchTasksFailCount.value++;
    frontendLogger.error("tasks", "获取任务列表失败", error);
    // F3：首次失败 toast 通知，后续失败保持静默（log-only）
    if (fetchTasksFailCount.value === 1) {
      toastOnly(false, "加载任务列表失败");
    }
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
  // A11：busy 守卫，执行中连点直接忽略，避免重复提交
  if (executingIds.has(taskId)) return;
  executingIds.add(taskId);
  try {
    frontendLogger.info("tasks", `执行任务: ${taskId}`);
    const data = await tasksApi.execute(taskId);
    toastOnly(true, extractApiError(data, "执行完成"));
  } catch (error) {
    frontendLogger.error("tasks", "执行任务异常", error);
    toastOnly(false, extractApiError(error, "执行失败"));
  } finally {
    executingIds.delete(taskId);
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
    await fetchTasks(true);
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
    await fetchTasks(true);
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
        await useScripts().showScriptEditor(taskId);
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
  // A11：busy 守卫，避免连点生成 _copy 与 _copy_2 等重复草稿
  if (duplicatingIds.has(taskId)) return;
  duplicatingIds.add(taskId);
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
  } finally {
    duplicatingIds.delete(taskId);
  }
}

async function exportTask(taskId: string): Promise<void> {
  // A11：busy 守卫，避免连点重复下载导出文件
  if (exportingIds.has(taskId)) return;
  exportingIds.add(taskId);
  try {
    // 经后端导出端点获取完整任务配置（与 /api/tasks/import 格式对应）
    const data = await tasksApi.export(taskId);
    downloadBlob(JSON.stringify(data, null, 2), `${taskId}.json`, "application/json");
    frontendLogger.info("tasks", "任务已导出");
  } catch (error) {
    frontendLogger.error("tasks", "导出任务失败", error);
    toastOnly(false, extractApiError(error, "导出失败"));
  } finally {
    exportingIds.delete(taskId);
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
    await fetchTasks(true);
  } catch (e) {
    frontendLogger.warn("tasks", "导入失败: " + (e as Error).message);
    toastOnly(false, "导入失败：" + extractApiError(e, "文件不是有效的任务 JSON"));
  }
}

export function useTasks() {
  return {
    tasks,
    activeTaskId,
    editingTask,
    editingTaskType,
    jsonError,
    executingIds,
    duplicatingIds,
    exportingIds,
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
  };
}
