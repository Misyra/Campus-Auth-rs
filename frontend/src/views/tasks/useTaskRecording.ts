/**
 * 「录制新任务」流程状态（单例）：选方案 → 配置会话录制（record=true）→ 完成录制
 * 取回动作 → 转换预览 → 保存为浏览器任务。
 *
 * 会话编排与方案页 PersistentSessionSection 同构（同一套 start/stop 端点与全局唯一
 * 会话约束，409 口径一致），但结束语义刻意不同：
 * - 「完成录制」走 stop + `apply=false`：仅结算会话并取回动作数组，**不写方案**——
 *   任务录制对所选方案零副作用（`recorded_actions` 不落盘、持久开关不被动翻转）；
 *   方案页「完成配置」才走缺省落盘采纳。
 * - 「放弃」走 discard=true（仅关浏览器，不落盘、不改方案）。
 *
 * 动作数组直接消费 stop 响应的 `recorded_actions`（后端无论 discard 与否都回显本次
 * 会话的动作）；不回读方案详情——apply=false 下方案本就没被写入，读到旧录制反而
 * 是错的。
 *
 * 409 口径与 PersistentSessionSection 一致：start 409 用固定文案「已有配置会话进行中」
 * （占用可能来自方案页/其他页面，统一行动指引比逐场景解释更有用）；stop 409 =
 * 会话已消失（浏览器被手动关闭/后端超时回收），透传后端 message 并退回选择阶段。
 */

import { ref } from "vue";
import { configSessionApi, tasksApi } from "@/api";
import { ApiError, extractApiError } from "@/api/client";
import { useToast } from "@/composables/useToast";
import { useConfirm } from "@/composables/useConfirm";
import { allTaskIds, useTaskDirectory } from "@/composables/useTaskDirectory";
import { frontendLogger } from "@/utils/logger";
import { convertRecordedActions, type RecordedTaskStep } from "./recordedTaskConverter";

/** 流程阶段：select 选方案；starting 拉起浏览器中；recording 录制进行中；
 *  finishing 正在结束会话；preview 预览并命名；saving 保存落盘中 */
export type TaskRecordingPhase =
  | "select"
  | "starting"
  | "recording"
  | "finishing"
  | "preview"
  | "saving";

/** 会话占用中（start 已发出到 stop 收尾之间）：关闭弹窗前需要确认放弃 */
const SESSION_ACTIVE_PHASES: ReadonlySet<TaskRecordingPhase> = new Set([
  "starting",
  "recording",
  "finishing",
]);

const open = ref(false);
const phase = ref<TaskRecordingPhase>("select");
/** 录制目标方案（配置会话按方案 ID 落盘，必须选已保存的方案） */
const profileId = ref("");
/** 就地错误（role=alert）；成功路径清空 */
const errorMsg = ref("");
/** 完成录制转换出的任务步骤（预览与保存的数据源） */
const steps = ref<RecordedTaskStep[]>([]);
/** 词表外被丢弃的动作数（预览里提示「N 个动作未识别」） */
const skippedUnknown = ref(0);
/** Worker 侧截断标记（动作数超出单次上限） */
const truncated = ref(false);

const { toastOnly } = useToast();
const { confirm } = useConfirm();

/** 打开弹窗：复位到选择阶段（会话进行中不会走到这里——requestClose 已先收尾） */
function openDialog(): void {
  phase.value = "select";
  profileId.value = "";
  errorMsg.value = "";
  steps.value = [];
  skippedUnknown.value = 0;
  truncated.value = false;
  open.value = true;
}

/**
 * 关闭弹窗。会话占用中先确认：录制会话全局唯一，直接关掉弹窗会留下一个看不见的
 * 占用（方案页也无法再发起会话），因此关闭 = 放弃录制（discard 结束）；取消则留在弹窗。
 */
async function requestClose(): Promise<void> {
  if (SESSION_ACTIVE_PHASES.has(phase.value)) {
    const ok = await confirm({
      title: "放弃录制",
      message: "录制会话进行中，关闭将结束会话并放弃本次录制。确定关闭吗？",
      confirmText: "放弃并关闭",
      danger: true,
    });
    if (!ok) return;
    await abandonRecording();
  }
  open.value = false;
}

/**
 * 发起录制会话（record 缺省 true）。409 用固定文案：占用可能来自方案页、其他页面
 * 或本页刷新丢失的观测状态，与 PersistentSessionSection 同一句话。
 */
async function startRecording(): Promise<void> {
  if (!profileId.value || phase.value !== "select") return;
  phase.value = "starting";
  errorMsg.value = "";
  try {
    await configSessionApi.start(profileId.value);
    phase.value = "recording";
  } catch (error) {
    phase.value = "select";
    if (error instanceof ApiError && error.status === 409) {
      errorMsg.value = "已有配置会话进行中";
    } else {
      errorMsg.value = extractApiError(error, "打开配置浏览器失败");
    }
  }
}

/** 放弃：discard 结束只关浏览器，后端不落盘录制、不改方案；409（浏览器已没了）目的已达成，静默收场 */
async function abandonRecording(): Promise<void> {
  if (!SESSION_ACTIVE_PHASES.has(phase.value)) return;
  phase.value = "finishing";
  try {
    await configSessionApi.stop(profileId.value, { discard: true });
    errorMsg.value = "";
  } catch (error) {
    errorMsg.value =
      error instanceof ApiError && error.status === 409 ? "" : extractApiError(error, "结束配置会话失败");
  } finally {
    phase.value = "select";
  }
}

/**
 * 完成录制：stop + `apply=false`（仅结算取回动作，不写方案——任务录制对所选方案
 * 零副作用），动作序列转成任务步骤进预览。
 *
 * 空录制（未录到任何动作）：不生成任务，提示后退回选择阶段——重新录制会覆盖上次
 * 结果，幂等；apply=false 下方案未被触碰，无残留。
 */
async function finishRecording(): Promise<void> {
  if (phase.value !== "recording") return;
  phase.value = "finishing";
  errorMsg.value = "";
  try {
    const result = await configSessionApi.stop(profileId.value, { apply: false });
    truncated.value = result.truncated;
    const conversion = convertRecordedActions(result.recorded_actions ?? []);
    if (!conversion.ok) {
      // 空录制：apply=false 不写方案，本次会话无任何残留，直接重试即可
      phase.value = "select";
      errorMsg.value = "未录制到任何动作，已放弃本次录制。请重新打开浏览器完成操作后再结束录制";
      return;
    }
    steps.value = conversion.steps;
    skippedUnknown.value = conversion.skippedUnknown;
    phase.value = "preview";
  } catch (error) {
    if (error instanceof ApiError && error.status === 409) {
      // 无活跃会话 = 浏览器已经没了（手动关闭/后端超时回收），退回选择阶段自愈
      phase.value = "select";
      errorMsg.value = extractApiError(error, "配置会话已结束");
    } else {
      // 其他失败（网络抖动、结算异常）会话可能仍在：回到录制态保留重试入口
      phase.value = "recording";
      errorMsg.value = extractApiError(error, "结束配置会话失败");
    }
  }
}

/**
 * 预览阶段删除单个步骤：录错/误触的动作在这里摘除后再保存（浏览器内悬浮面板只
 * 能删本页缓冲，跨页汇总后的可靠删除点在预览）。删空后保存按钮自然禁用。
 */
function removeStep(index: number): void {
  if (phase.value !== "preview") return;
  steps.value.splice(index, 1);
}

/**
 * 保存为浏览器任务：复用既有任务创建管线（PUT /api/tasks/{id}，与新建任务的自动
 * 保存同一落盘通道）。ID 取号与 useTasks.createTask 同口径：对三类任务唯一
 * （后端写盘会按 id 删掉别桶同名的文件）。
 *
 * url 固定 {{LOGIN_URL}}：录制会话本身从所选方案的门户地址开始，运行时由变量解析
 * 管线展开；页面内跳转由保留的 navigate 步骤承担。
 */
async function saveRecordedTask(name: string, description: string): Promise<void> {
  if (phase.value !== "preview" || !steps.value.length) return;
  phase.value = "saving";
  errorMsg.value = "";
  const finalName = name.trim() || "录制任务";
  try {
    let newId = "untitled_1";
    let counter = 2;
    while (allTaskIds().has(newId)) {
      newId = `untitled_${counter++}`;
    }
    await tasksApi.save(newId, {
      type: "browser",
      task_id: newId,
      name: finalName,
      description,
      url: "{{LOGIN_URL}}",
      steps: steps.value,
    });
    await useTaskDirectory().fetchDirectory(true);
    toastOnly(true, `已保存为任务「${finalName}」`);
    frontendLogger.info("tasks", `录制任务已保存: ${newId}`);
    open.value = false;
    phase.value = "select";
    steps.value = [];
    skippedUnknown.value = 0;
    truncated.value = false;
  } catch (error) {
    // 保存失败停在预览态：录制内容还在，可直接重试保存
    phase.value = "preview";
    errorMsg.value = extractApiError(error, "保存任务失败");
  }
}

export function useTaskRecording() {
  return {
    open,
    phase,
    profileId,
    errorMsg,
    steps,
    skippedUnknown,
    truncated,
    openDialog,
    requestClose,
    startRecording,
    abandonRecording,
    finishRecording,
    removeStep,
    saveRecordedTask,
  };
}
