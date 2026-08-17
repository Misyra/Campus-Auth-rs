/**
 * 定时任务状态与操作（单例）。
 * 替代原 scheduledTasksData + scheduledTasksMethods。
 */

import { ref, reactive } from "vue";
import type { ScheduledTask, ScheduledTaskHistoryItem } from "../api/types";
import { scheduledTasksApi } from "../api";
import { extractApiError } from "../api/client";
import { frontendLogger } from "../utils/logger";
import { formatScheduleTime, formatTimeValue } from "../utils/formatters";
import { useToast } from "./useToast";
import { useConfirm } from "./useConfirm";

interface ScheduledTaskForm {
  id: string;
  name: string;
  description: string;
  task_type: string;
  target_id: string;
  enabled: boolean;
  schedule: { hour: number; minute: number };
  timeout: number;
}

/** 从 5 字段 cron 表达式解析 hour 和 minute */
function parseCronToSchedule(cron: string): { hour: number; minute: number } {
  const parts = cron.trim().split(/\s+/);
  // 标准 5 字段: minute hour day month weekday
  if (parts.length >= 2) {
    const minute = parseInt(parts[0], 10);
    const hour = parseInt(parts[1], 10);
    if (!isNaN(minute) && !isNaN(hour)) {
      return { hour, minute };
    }
  }
  return { hour: 8, minute: 0 };
}

/** 从 {hour, minute} 生成 5 字段 cron 表达式 */
function scheduleToCron(hour: number, minute: number): string {
  return `${minute} ${hour} * * *`;
}

const scheduledTasks = ref<ScheduledTask[]>([]);
const scheduledTaskForm = ref<ScheduledTaskForm>({
  id: "",
  name: "",
  description: "",
  task_type: "browser",
  target_id: "",
  enabled: true,
  schedule: { hour: 8, minute: 0 },
  timeout: 60,
});
const scheduledTaskHistory = ref<ScheduledTaskHistoryItem[]>([]);
const showScheduledTaskModal = ref(false);
const editingScheduledTask = ref<string | null>(null);
const scheduledTaskFormLoading = ref(false);
const scheduledTaskHistoryLoading = ref(false);
const selectedScheduledTaskId = ref<string | null>(null);

// A11：手动运行 busy 守卫（响应式 Set），防止连点重复提交
const runningIds = reactive(new Set<string>());

const { toastOnly } = useToast();
const { confirm } = useConfirm();

// P15：5 秒内已成功拉取则跳过（useUi.init 已拉全部数据，View mount / 路由往返
// 不再重复请求）。lastFetchAt 仅成功后更新（失败不更新以便重试）；
// force: true 供变更后刷新 / 重连回调等显式刷新场景绕过守卫。
let lastFetchAt = 0;

async function loadScheduledTasks(force = false): Promise<void> {
  if (!force && Date.now() - lastFetchAt < 5000) return;
  try {
    const data = await scheduledTasksApi.list();
    if (Array.isArray(data)) {
      scheduledTasks.value.splice(0, scheduledTasks.value.length, ...data);
    }
    lastFetchAt = Date.now();
  } catch (e) {
    frontendLogger.error("scheduled_tasks", "加载定时任务失败", e);
  }
}

function openCreateScheduledTask(): void {
  editingScheduledTask.value = null;
  Object.assign(scheduledTaskForm.value, {
    name: "",
    description: "",
    task_type: "browser",
    target_id: "",
    enabled: true,
    schedule: { hour: 8, minute: 0 },
    timeout: 60,
  });
  showScheduledTaskModal.value = true;
}

function openEditScheduledTask(task: ScheduledTask): void {
  editingScheduledTask.value = task.id;
  const schedule = parseCronToSchedule(task.cron || "");
  Object.assign(scheduledTaskForm.value, {
    name: task.name || "",
    description: task.description || "",
    task_type: task.task_type || "browser",
    target_id: task.target_id || "",
    enabled: task.enabled !== false,
    schedule,
    timeout: task.timeout || 60,
  });
  showScheduledTaskModal.value = true;
}

function closeScheduledTaskModal(): void {
  showScheduledTaskModal.value = false;
  editingScheduledTask.value = null;
}

async function saveScheduledTask(): Promise<void> {
  const form = scheduledTaskForm.value;
  if (!form.name.trim()) {
    toastOnly(false, "请输入任务名称");
    return;
  }
  if (!form.target_id) {
    toastOnly(false, "请选择目标任务");
    return;
  }
  scheduledTaskFormLoading.value = true;
  const cron = scheduleToCron(form.schedule.hour, form.schedule.minute);
  try {
    if (editingScheduledTask.value) {
      // PUT /api/scheduler/jobs/{id} — 发送完整表单数据（类型由后端从 target 推导，不再上传）
      const payload = {
        name: form.name,
        description: form.description,
        target_id: form.target_id,
        cron,
        enabled: form.enabled,
        timeout: form.timeout,
      };
      const data = await scheduledTasksApi.update(editingScheduledTask.value, payload);
      toastOnly(true, data?.message || "保存成功");
    } else {
      // POST /api/scheduler/jobs — 需要 id, name, target_id, cron, enabled
      const id = `sched_${Date.now().toString(36)}_${Math.random().toString(36).slice(2, 6)}`;
      const payload = {
        id,
        name: form.name,
        target_id: form.target_id,
        cron,
        enabled: form.enabled,
      };
      const data = await scheduledTasksApi.create(payload);
      toastOnly(true, data?.message || "保存成功");
    }
    closeScheduledTaskModal();
    await loadScheduledTasks(true);
  } catch (e) {
    toastOnly(false, extractApiError(e, "保存失败"));
  } finally {
    scheduledTaskFormLoading.value = false;
  }
}

async function deleteScheduledTask(taskId: string): Promise<void> {
  const ok = await confirm({ title: "删除定时任务", message: "确定要删除这个定时任务吗？", danger: true });
  if (!ok) return;
  try {
    const data = await scheduledTasksApi.delete(taskId);
    toastOnly(true, data?.message || "删除成功");
    await loadScheduledTasks(true);
  } catch (e) {
    toastOnly(false, extractApiError(e, "删除失败"));
  }
}

async function toggleScheduledTask(taskId: string): Promise<void> {
  try {
    const data = await scheduledTasksApi.toggle(taskId);
    toastOnly(true, data?.message || "操作成功");
    await loadScheduledTasks(true);
  } catch (e) {
    toastOnly(false, extractApiError(e, "操作失败"));
  }
}

async function runScheduledTask(taskId: string): Promise<void> {
  // A11：busy 守卫，运行中连点直接忽略，避免重复触发定时任务
  if (runningIds.has(taskId)) return;
  runningIds.add(taskId);
  try {
    const data = await scheduledTasksApi.run(taskId);
    toastOnly(true, data?.message || "执行成功");
    await loadScheduledTasks(true);
  } catch (e) {
    toastOnly(false, extractApiError(e, "执行失败"));
  } finally {
    runningIds.delete(taskId);
  }
}

async function loadScheduledTaskHistory(taskId: string): Promise<void> {
  selectedScheduledTaskId.value = taskId;
  scheduledTaskHistoryLoading.value = true;
  try {
    const data = await scheduledTasksApi.history(taskId);
    // 后端返回 { runs: [...] } 包装结构
    const runs = Array.isArray(data) ? data : (data as { runs?: ScheduledTaskHistoryItem[] }).runs || [];
    scheduledTaskHistory.value.splice(0, scheduledTaskHistory.value.length, ...runs);
  } catch (e) {
    frontendLogger.error("scheduled_tasks", "加载执行历史失败", e);
    scheduledTaskHistory.value.splice(0, scheduledTaskHistory.value.length);
  } finally {
    scheduledTaskHistoryLoading.value = false;
  }
}

function closeScheduledTaskHistory(): void {
  selectedScheduledTaskId.value = null;
  scheduledTaskHistory.value.splice(0, scheduledTaskHistory.value.length);
}

function formatTaskType(type: string): string {
  const types: Record<string, string> = { script: "自定义脚本", browser: "浏览器任务", shell: "Shell 命令" };
  return types[type] || type;
}

function onTimeChange(event: Event): void {
  const value = (event.target as HTMLInputElement).value;
  if (value) {
    const [hour, minute] = value.split(":").map(Number);
    scheduledTaskForm.value.schedule.hour = hour;
    scheduledTaskForm.value.schedule.minute = minute;
  }
}

export function useScheduledTasks() {
  return {
    scheduledTasks,
    scheduledTaskForm,
    scheduledTaskHistory,
    showScheduledTaskModal,
    editingScheduledTask,
    scheduledTaskFormLoading,
    scheduledTaskHistoryLoading,
    selectedScheduledTaskId,
    runningIds,
    loadScheduledTasks,
    openCreateScheduledTask,
    openEditScheduledTask,
    closeScheduledTaskModal,
    saveScheduledTask,
    deleteScheduledTask,
    toggleScheduledTask,
    runScheduledTask,
    loadScheduledTaskHistory,
    closeScheduledTaskHistory,
    formatScheduleTime,
    formatTimeValue,
    formatTaskType,
    onTimeChange,
  };
}
