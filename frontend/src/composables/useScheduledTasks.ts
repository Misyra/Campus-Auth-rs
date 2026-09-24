/**
 * 定时任务状态与操作（单例）——自动保存模式。
 *
 * 编辑模型与 `useScripts` / `useHttpTasks` 对齐：**没有弹窗、没有保存按钮**。字段变更
 * debounce 静默落盘（首次 POST 建任务、之后 PUT），头部状态字 idle→saving→saved/error；
 * 缺口（见 `utils/scheduledDraft`）会拦住落盘并把状态字改成「有 N 处待补全，改动暂未保存」。
 *
 * 此前是「表格 + 新建/编辑弹窗」：同一个任务页里，另外三个子页是"点行进二级页、
 * 改动自动保存"，定时任务却是"弹窗 + 取消/保存"，交互与版式都不是一套东西。
 *
 * 两处与弹窗时代不同的口径（有意）：
 * - **新建不再预落盘**：弹窗时代点「新建」只是打开表单（不落盘，这点本来就对），现在
 *   同理——ID 由程序生成（`newScheduledTaskId`）但只有名称与目标都补齐后第一次自动保存
 *   才创建文件，中途放弃不留垃圾任务。
 * - **启停开关仍留在列表**（连点守卫不变），编辑页侧栏另有一个同样的开关，改的是同一份
 *   草稿、走自动保存。
 */

import { computed, ref, watch } from "vue";
import type { ScheduledTask, ScheduledTaskHistoryItem } from "../api/types";
import { scheduledTasksApi } from "../api";
import { extractApiError, isConflictError } from "../api/client";
import { frontendLogger } from "../utils/logger";
import { createFetchGuard, createFirstFailNotifier, useBusyIds } from "../utils/guards";
import { createAutosaveController, gapBlocker } from "../utils/autosave";
import {
  emptyScheduledDraft,
  scheduledDraftFingerprint,
  scheduledDraftFromServer,
  scheduledDraftGaps,
  scheduledDraftPayload,
  type ScheduledGapContext,
  type ScheduledTaskDraft,
} from "../utils/scheduledDraft";
import { useTaskDirectory } from "./useTaskDirectory";
import { initialReconcileState, reconcileDraft } from "../utils/draftReconcile";
import { useToast } from "./useToast";
import { useConfirm } from "./useConfirm";

export type { ScheduledTaskDraft };

// 目标下拉与缺口校验都要用任务目录（浏览器任务 / 脚本的混合列表，模块级单例）
const { browserTasks, scripts: taskScripts, loaded: directoryLoaded } = useTaskDirectory();

const scheduledTasks = ref<ScheduledTask[]>([]);
/** 列表是否已成功拉取过一次：`?task=<id>` 未就绪时不判定"任务不存在"（见 useTaskEditorQuery 的 ready） */
const scheduledLoaded = ref(false);
/** 正在编辑的定时任务草稿（null = 列表态） */
const scheduledTaskDraft = ref<ScheduledTaskDraft | null>(null);

const scheduledTaskHistory = ref<ScheduledTaskHistoryItem[]>([]);
const scheduledTaskHistoryLoading = ref(false);
const selectedScheduledTaskId = ref<string | null>(null);

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

/**
 * 拉取世代号：只有**最新**那一发允许写列表。
 *
 * 与 `useTaskDirectory.fetchDirectory` 同一口径（那里解释了为什么必要）：落盘后刷新与
 * 守卫内的自然刷新可以重叠，较旧的快照后到会让"刚编辑的这个任务"从列表里消失，
 * 对账逻辑据此判定它已被删除、关掉编辑器，未落盘的改动随之丢失。
 */
let loadEpoch = 0;

async function loadScheduledTasks(force = false): Promise<void> {
  if (!fetchGuard.shouldFetch(force)) return;
  const mine = ++loadEpoch;
  try {
    const data = await scheduledTasksApi.list();
    if (mine !== loadEpoch) return; // 过期快照：丢弃
    if (Array.isArray(data)) {
      scheduledTasks.value.splice(0, scheduledTasks.value.length, ...data);
      scheduledLoaded.value = true;
    }
    fetchGuard.markSuccess();
    loadFail.trackRecovery();
  } catch (e) {
    if (mine !== loadEpoch) return;
    frontendLogger.error("scheduler", "加载定时任务失败", e);
    if (loadFail.trackFailure()) {
      toastOnly(false, extractApiError(e, "加载定时任务失败"));
    }
  }
}

/**
 * 列表刷新后对账（四个任务面板同一口径，见 `utils/draftReconcile`）。
 *
 * 正在编辑的任务若在别处被删掉（手改磁盘 / 另一个实例 / 刷新后它已不在列表里），
 * 编辑器必须自己退出：否则用户每改一处都会 PUT 一个不存在的 id、拿到 404 与失败
 * 提示，草稿却留在页面上，看起来像"保存不了"。
 *
 * 这里用 `clearScheduledDraft` 而**不是** `closeScheduledTaskEditor`：后者的语义是
 * "退出即落盘"（flush），对一个已被删除的任务再发一次 PUT 只会再吃一次 404；而且
 * 那份草稿已经没有任何可以落盘的去处。
 *
 * 挂在模块作用域而**不是** `loadScheduledTasks` 里面：后者每成功拉取一次就跑一遍
 * （自动保存落盘后也会拉），放在里面会让 watcher 与 `reconcileState` 按拉取次数累加，
 * 同一份删除被反复判定。
 */
let reconcileState = initialReconcileState(scheduledTasks.value.map((t) => t.id));
watch(
  () => scheduledTasks.value.map((t) => t.id).join("\u0000"),
  () => {
    const listIds = scheduledTasks.value.map((t) => t.id);
    const result = reconcileDraft({
      state: reconcileState,
      draftId: scheduledTaskDraft.value?.id,
      listIds,
    });
    reconcileState = result.state;
    if (!result.gone) return;
    clearScheduledDraft();
    toastOnly(false, "正在编辑的任务已不存在，已退出编辑");
  },
);

// ---- 草稿 / 缺口 ----

/** 缺口校验的上下文：当前草稿的目标类型决定候选池 */
function gapContextFor(draft: ScheduledTaskDraft): ScheduledGapContext {
  const pool = draft.task_type === "script" ? taskScripts.value : browserTasks.value;
  return {
    validTargetIds: pool.map((t) => t.id),
    targetsLoaded: directoryLoaded.value,
  };
}

/** 当前草稿的缺口（渲染用）：自动保存被缺口拦住时状态字与缺口条据此改口 */
const draftGapsNow = computed<string[]>(() =>
  scheduledTaskDraft.value ? scheduledDraftGaps(scheduledTaskDraft.value, gapContextFor(scheduledTaskDraft.value)) : [],
);

/** 当前编辑的是"还没落盘的定时任务"（面板据此把「删除」改成「放弃」、落盘后补 `?task=`） */
const isNewScheduledDraft = computed(() => scheduledTaskDraft.value?._isNew === true);

// ---- 自动保存 ----

/**
 * 自动保存：状态机、在途请求序号与 debounce 都交给共享控制器（见 `utils/autosave`），
 * 本面板只提供三件口径——**什么算有改动**（载荷指纹）、**什么算发不出去**（缺口）、
 * **往哪儿落盘**。
 *
 * 落盘动作带着本面板唯一的特殊分支：**首次落盘走 POST 建任务，之后同一 id 走 PUT**。
 */
const autosave = createAutosaveController<ScheduledTaskDraft>({
  draft: scheduledTaskDraft,
  idOf: (draft) => draft.id,
  fingerprintOf: scheduledDraftFingerprint,
  blockReasonOf: gapBlocker((draft) => scheduledDraftGaps(draft, gapContextFor(draft))),
  persist: async (draft) => {
    const payload = scheduledDraftPayload(draft);
    if (draft._isNew) {
      try {
        await scheduledTasksApi.create({ id: draft.id, ...payload });
      } catch (e) {
        // 首发与"换编辑对象 / 关闭编辑器时补发的那一发"重叠时，两边都以为自己是第一次；
        // 而 `POST /api/scheduler/jobs` 对已存在的 id 明确返回 409（PUT 是幂等合并）。
        // 此时 409 的真实含义是"另一发已经把它建好了"，降级成 PUT 继续即可——否则用户
        // 看到一句"定时任务 X 已存在"的红字，而任务其实建成功了。
        if (!isConflictError(e)) throw e;
        await scheduledTasksApi.update(draft.id, payload);
      }
    } else {
      await scheduledTasksApi.update(draft.id, payload);
    }
    // 列表里的调度摘要（下次执行时间 / 今日成功次数）由后端回填，落盘后刷新才准
    await loadScheduledTasks(true);
  },
  onSaved: (draft) => {
    draft._isNew = false;
  },
  toast: toastOnly,
  logScope: "scheduler",
  detachedSubject: "上一份定时任务",
});

/** 清空编辑器状态（删除 / 放弃新建等无需再保存的场景）；草稿由控制器一并关掉 */
function clearScheduledDraft(): void {
  autosave.clear();
}

/** 关闭编辑器：在途 debounce 立即落盘（「退出即生效」承诺） */
async function closeScheduledTaskEditor(): Promise<void> {
  await autosave.flush("close");
  clearScheduledDraft();
}

/**
 * 新建草稿（**不落盘**）。
 *
 * ID 虽然由程序生成，但只有名称与目标补齐后第一次自动保存才创建文件——否则"点开看一眼
 * 又退出"会在磁盘上留一个没填过的任务。
 */
function createScheduledDraft(): void {
  const draft = emptyScheduledDraft();
  scheduledTaskDraft.value = draft;
  // 名称与目标都空着，本来就是缺口（发不出去）：登记基线只是省掉一发注定被拦的定时器
  autosave.markBaseline(draft);
}

/**
 * 打开定时任务编辑器：无参 = 新建草稿，带参 = 由列表里的任务构造草稿。
 *
 * 任务字段**全部取自列表响应**（后端没有单任务 GET）：故深链要在列表就绪后再解析
 * （见 useTaskEditorQuery 的 `ready`），此处找不到即视为"已不存在"。
 */
async function showScheduledTaskEditor(taskId?: string): Promise<void> {
  // 换编辑对象：上一份草稿在途的改动先补发（不 await——打开必须立刻发生）
  void autosave.flush("switch");
  if (!taskId) {
    createScheduledDraft();
    return;
  }
  const task = scheduledTasks.value.find((t) => t.id === taskId);
  if (!task) {
    toastOnly(false, `找不到定时任务「${taskId}」，它可能已被删除`);
    return;
  }
  const draft = scheduledDraftFromServer(task);
  scheduledTaskDraft.value = draft;
  // 刚载入的草稿就是磁盘现状：基线对上了，用户不动它就不会发请求
  autosave.markBaseline(draft);
  if (draft._originalCronInvalid) {
    // 非每日表达式在表单里表达不了：不提示就等于静默改写调度语义。
    // 措辞与面板里那条 hint 同口径——只改名称 / 目标不动调度，改「执行时间」才会改写。
    toastOnly(
      false,
      `该任务使用非每日时间表达式（${draft._originalCron}）：不动「执行时间」就保持原样，改了才会按上方时间改为每日执行`,
    );
  }
}

// ---- 列表行操作 ----

async function deleteScheduledTask(taskId: string): Promise<void> {
  // 新建草稿磁盘上还没有它：这个动作是"放弃"而不是"删除"，照旧走后端只会 404
  const draft = scheduledTaskDraft.value;
  const discard = !!draft && draft._isNew && draft.id === taskId;
  const ok = await confirm({
    title: discard ? "放弃新建定时任务" : "删除定时任务",
    message: discard
      ? "这个定时任务还没有保存过（名称与目标补齐后才会创建），放弃后当前内容会丢掉。"
      : `确定要删除定时任务「${taskId}」吗？`,
    danger: true,
  });
  if (!ok) return;
  if (discard) {
    clearScheduledDraft();
    return;
  }
  try {
    // 被删的是当前打开的那条时先关编辑器：自动保存可能正要写回一个已删除的 id
    if (draft && !draft._isNew && draft.id === taskId) {
      clearScheduledDraft();
    }
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
    // 后端只表示"已排入执行"（spawn 手动运行后立刻回包），成败要等执行历史。
    // 原来读 `data?.message` 恒为 undefined，于是无论任务成败都弹绿色的"执行成功"。
    await scheduledTasksApi.run(taskId);
    toastOnly(true, "已触发执行，结果见「执行历史」");
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

export function useScheduledTasks() {
  return {
    scheduledTasks,
    scheduledLoaded,
    scheduledTaskDraft,
    isNewScheduledDraft,
    autosaveState: autosave.autosaveState,
    draftGapsNow,
    scheduledTaskHistory,
    scheduledTaskHistoryLoading,
    selectedScheduledTaskId,
    runningIds,
    togglingIds,
    loadScheduledTasks,
    showScheduledTaskEditor,
    createScheduledDraft,
    closeScheduledTaskEditor,
    clearScheduledDraft,
    deleteScheduledTask,
    toggleScheduledTask,
    runScheduledTask,
    loadScheduledTaskHistory,
    closeScheduledTaskHistory,
    formatTaskType,
  };
}
