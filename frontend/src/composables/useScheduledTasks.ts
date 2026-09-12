/**
 * 定时任务状态与操作（单例）。
 * 替代原 scheduledTasksData + scheduledTasksMethods。
 */

import { ref } from "vue";
import type { ScheduledTask, ScheduledTaskHistoryItem, ScheduledTaskTrigger } from "../api/types";
import { scheduledTasksApi } from "../api";
import { extractApiError } from "../api/client";
import { frontendLogger } from "../utils/logger";
import { createFetchGuard, createFirstFailNotifier, useBusyIds } from "../utils/guards";
import { formatScheduleTime } from "../utils/formatters";
import { useToast } from "./useToast";
import { useConfirm } from "./useConfirm";

interface ScheduledTaskForm {
  id: string;
  name: string;
  description: string;
  task_type: string;
  target_id: string;
  enabled: boolean;
  trigger: ScheduledTaskTrigger;
  schedule: { hour: number; minute: number };
  timeout: number;
  /** 启动触发：每日成功次数上限 */
  max_runs_per_day: number;
  /** 启动触发：失败重试次数 */
  max_retries: number;
  /** 启动触发：延迟执行秒数 */
  startup_delay_secs: number;
}

/** 启动触发字段的合法区间（与后端钳制口径一致） */
export const STARTUP_FORM_LIMITS = {
  maxRunsPerDay: { min: 1, max: 99, fallback: 1 },
  maxRetries: { min: 0, max: 10, fallback: 2 },
  startupDelaySecs: { min: 0, max: 86400, fallback: 30 },
} as const;

/** 钳制启动触发表单值：NaN/越界回退缺省或区间边界（纯函数，供测试） */
export function clampStartupForm(input: {
  max_runs_per_day: number;
  max_retries: number;
  startup_delay_secs: number;
}): { max_runs_per_day: number; max_retries: number; startup_delay_secs: number } {
  const { maxRunsPerDay, maxRetries, startupDelaySecs } = STARTUP_FORM_LIMITS;
  const clamp = (raw: number, min: number, max: number, fallback: number): number => {
    const n = Math.round(Number(raw));
    if (!Number.isFinite(n)) return fallback;
    return Math.min(Math.max(n, min), max);
  };
  return {
    max_runs_per_day: clamp(input.max_runs_per_day, maxRunsPerDay.min, maxRunsPerDay.max, maxRunsPerDay.fallback),
    max_retries: clamp(input.max_retries, maxRetries.min, maxRetries.max, maxRetries.fallback),
    startup_delay_secs: clamp(
      input.startup_delay_secs,
      startupDelaySecs.min,
      startupDelaySecs.max,
      startupDelaySecs.fallback,
    ),
  };
}

/** 从 5 字段 cron 表达式解析 hour 和 minute；分/时字段含非纯数字内容（步进、区间、列表等）时返回 valid:false */
export function parseCronToSchedule(cron: string): { hour: number; minute: number; valid: boolean } {
  const parts = cron.trim().split(/\s+/);
  // 标准 5 字段: minute hour day month weekday
  // 必须整字段纯数字：parseInt("8-18") 会宽松解析为 8，把区间表达式
  // "半解析成功"，调度语义已经变了却检测不到
  if (parts.length >= 2 && /^\d+$/.test(parts[0]) && /^\d+$/.test(parts[1])) {
    return { hour: parseInt(parts[1], 10), minute: parseInt(parts[0], 10), valid: true };
  }
  // 非每日时间表达式：回退 08:00 仅作表单展示初值，调用方必须提示覆盖后果
  //（保存固定生成每日表达式，此前无提示导致调度语义被静默改写）
  return { hour: 8, minute: 0, valid: false };
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
  trigger: "cron",
  schedule: { hour: 8, minute: 0 },
  timeout: 60,
  max_runs_per_day: 1,
  max_retries: 2,
  startup_delay_secs: 30,
});
const scheduledTaskHistory = ref<ScheduledTaskHistoryItem[]>([]);
const showScheduledTaskModal = ref(false);
const editingScheduledTask = ref<string | null>(null);
const scheduledTaskFormLoading = ref(false);
const scheduledTaskHistoryLoading = ref(false);
const selectedScheduledTaskId = ref<string | null>(null);
/** 编辑中的任务原始 cron 表达式（非每日格式时在弹窗内明示覆盖后果） */
const originalCron = ref("");
const originalCronInvalid = ref(false);

// A11：手动运行 busy 守卫（响应式 Set），防止连点重复提交
const runningIds = useBusyIds();
// 启停开关 busy 守卫：toggle 无 in-flight 防护时快速双击会发出两次请求，终态取决于响应顺序
const togglingIds = useBusyIds();

const { toastOnly } = useToast();
const { confirm } = useConfirm();

// P15：5 秒内已成功拉取则跳过（useUi.init 已拉全部数据，View mount / 路由往返
// 不再重复请求）。失败不记录时间戳以便重试；force: true 供变更后刷新 /
// 重连回调等显式刷新场景绕过守卫。
const fetchGuard = createFetchGuard(5000);
// 首败提示：加载失败时不再静默显示"暂无定时任务"空态误导用户
const loadFail = createFirstFailNotifier();

async function loadScheduledTasks(force = false): Promise<void> {
  if (!fetchGuard.shouldFetch(force)) return;
  try {
    const data = await scheduledTasksApi.list();
    if (Array.isArray(data)) {
      scheduledTasks.value.splice(0, scheduledTasks.value.length, ...data);
    }
    fetchGuard.markSuccess();
    loadFail.trackRecovery();
  } catch (e) {
    frontendLogger.error("scheduler", "加载定时任务失败", e);
    if (loadFail.trackFailure()) {
      toastOnly(false, extractApiError(e, "加载定时任务失败"));
    }
  }
}

function openCreateScheduledTask(): void {
  editingScheduledTask.value = null;
  originalCron.value = "";
  originalCronInvalid.value = false;
  Object.assign(scheduledTaskForm.value, {
    name: "",
    description: "",
    task_type: "browser",
    target_id: "",
    enabled: true,
    trigger: "cron",
    schedule: { hour: 8, minute: 0 },
    timeout: 60,
    max_runs_per_day: 1,
    max_retries: 2,
    startup_delay_secs: 30,
  });
  showScheduledTaskModal.value = true;
}

function openEditScheduledTask(task: ScheduledTask): void {
  editingScheduledTask.value = task.id;
  const isStartup = task.trigger === "startup";
  const cron = isStartup ? "" : task.cron || "";
  const schedule = parseCronToSchedule(cron);
  originalCron.value = cron;
  originalCronInvalid.value = !isStartup && !schedule.valid;
  if (!schedule.valid && !isStartup) {
    toastOnly(
      false,
      `该任务使用非每日时间表达式（${cron}），保存后将按表单时间改为每日执行`,
    );
  }
  Object.assign(scheduledTaskForm.value, {
    name: task.name || "",
    description: task.description || "",
    // 表单类型仅用于展示/切换目标下拉；保存不上传类型，后端始终从 target 推导
    task_type: task.task_type === "script" ? "script" : "browser",
    target_id: task.target_id || "",
    enabled: task.enabled !== false,
    trigger: isStartup ? "startup" : "cron",
    schedule,
    timeout: task.timeout || 60,
    max_runs_per_day: task.max_runs_per_day || 1,
    max_retries: task.max_retries ?? 2,
    startup_delay_secs: task.startup_delay_secs ?? 30,
  });
  showScheduledTaskModal.value = true;
}

function closeScheduledTaskModal(): void {
  showScheduledTaskModal.value = false;
  editingScheduledTask.value = null;
  originalCron.value = "";
  originalCronInvalid.value = false;
}

async function saveScheduledTask(validTargetIds?: string[]): Promise<void> {
  const form = scheduledTaskForm.value;
  if (!form.name.trim()) {
    toastOnly(false, "请输入任务名称");
    return;
  }
  if (!form.target_id) {
    toastOnly(false, "请选择目标任务");
    return;
  }
  // 死引用校验：目标任务被删除后下拉显示为空但 target_id 残留，
  // 保存成功也要到运行期才报"加载目标任务失败"，这里前置拦截
  if (validTargetIds && !validTargetIds.includes(form.target_id)) {
    toastOnly(false, "目标任务不存在或已删除，请重新选择");
    return;
  }
  scheduledTaskFormLoading.value = true;
  const isStartup = form.trigger === "startup";
  // 启动触发不依赖 cron（后端落盘空串）；定时触发按表单时间生成每日表达式
  const cron = isStartup ? "" : scheduleToCron(form.schedule.hour, form.schedule.minute);
  // 超时按输入框 min/max 钳制：NaN/越界值不发后端（后端缺省 60s）
  const timeout = Math.min(Math.max(Number(form.timeout) || 60, 5), 3600);
  const startupFields = clampStartupForm({
    max_runs_per_day: form.max_runs_per_day,
    max_retries: form.max_retries,
    startup_delay_secs: form.startup_delay_secs,
  });
  try {
    if (editingScheduledTask.value) {
      // PUT /api/scheduler/jobs/{id} — 发送完整表单数据（类型由后端从 target 推导，不再上传）
      const payload = {
        name: form.name,
        description: form.description,
        target_id: form.target_id,
        cron,
        enabled: form.enabled,
        timeout,
        trigger: form.trigger,
        ...(isStartup ? startupFields : {}),
      };
      const data = await scheduledTasksApi.update(editingScheduledTask.value, payload);
      toastOnly(true, data?.message || "保存成功");
    } else {
      // POST /api/scheduler/jobs — 创建同样带上描述与超时
      //（此前只发 5 字段，弹窗里的描述/超时被静默丢弃）
      const id = `sched_${Date.now().toString(36)}_${Math.random().toString(36).slice(2, 6)}`;
      const payload = {
        id,
        name: form.name,
        description: form.description,
        target_id: form.target_id,
        cron,
        enabled: form.enabled,
        timeout,
        trigger: form.trigger,
        ...(isStartup ? startupFields : {}),
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
  // busy 守卫：开关连点只发一次请求，视觉状态等刷新后如实翻转
  if (togglingIds.has(taskId)) return;
  togglingIds.add(taskId);
  try {
    const data = await scheduledTasksApi.toggle(taskId);
    toastOnly(true, data?.message || "操作成功");
    await loadScheduledTasks(true);
  } catch (e) {
    toastOnly(false, extractApiError(e, "操作失败"));
  } finally {
    togglingIds.delete(taskId);
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
  // G21：以发起时的 taskId 为准（参考 useStatus 的 epoch 设计）：快速切换 A→B 时，
  // 慢的 A 响应后到则丢弃，避免覆盖 B 的历史。关闭面板（置 null）后迟到的响应同样丢弃。
  const requestTaskId = taskId;
  try {
    const data = await scheduledTasksApi.history(taskId);
    if (selectedScheduledTaskId.value !== requestTaskId) return;
    // 后端返回 { runs: [...] } 包装结构
    const runs = Array.isArray(data) ? data : (data as { runs?: ScheduledTaskHistoryItem[] }).runs || [];
    scheduledTaskHistory.value.splice(0, scheduledTaskHistory.value.length, ...runs);
  } catch (e) {
    if (selectedScheduledTaskId.value !== requestTaskId) return;
    frontendLogger.error("scheduler", "加载执行历史失败", e);
    scheduledTaskHistory.value.splice(0, scheduledTaskHistory.value.length);
  } finally {
    // 仅当仍是当前选中任务时才复位 loading（后发起的请求负责自己的状态）
    if (selectedScheduledTaskId.value === requestTaskId) {
      scheduledTaskHistoryLoading.value = false;
    }
  }
}

function closeScheduledTaskHistory(): void {
  selectedScheduledTaskId.value = null;
  scheduledTaskHistory.value.splice(0, scheduledTaskHistory.value.length);
}

function formatTaskType(type: string): string {
  const types: Record<string, string> = { script: "自定义脚本", browser: "浏览器任务" };
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
    originalCron,
    originalCronInvalid,
    runningIds,
    togglingIds,
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
    // formatTimeValue 不再经此转发：无视图消费，测试直接引用 utils/formatters
    formatScheduleTime,
    formatTaskType,
    onTimeChange,
  };
}
