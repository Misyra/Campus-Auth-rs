/**
 * 直连任务（`type: "http"`）状态与操作（单例）——自动保存模式。
 *
 * 结构与 `useTasks` 同构：列表来自 `useTaskDirectory` 单次拉取的 httpTasks 视图，
 * 编辑是平铺的 `HttpTaskDraft`（草稿 ⇄ 落盘载荷的互转与缺口校验见 `utils/httpTask`），
 * 增删改查复用 `tasksApi`（后端一套 CRUD 按 `type` 分派）。
 *
 * 编辑模型（方案 G 终稿）：**没有草稿态与显式保存**。字段变更 debounce 静默 PUT；
 * 缺口校验（id 形态 / 请求地址）只影响"是否值得发请求"，不再阻断编辑。
 * 「放弃未保存的修改？」确认链随草稿态一并退役。
 *
 * 没有「执行」入口：直连任务不经 Python Worker，验证路径是发一次测试请求
 * （见 `useHttpTaskTest`，凭据由宿主传入，本模块不关心）。
 */

import { computed, ref, watch } from "vue";
import type { HttpTaskConfig, TaskItem } from "../api/types";
import { tasksApi } from "../api";
import { extractApiError } from "../api/client";
import { frontendLogger } from "../utils/logger";
import { downloadBlob, pickFile } from "../utils/file";
import { useBusyIds } from "../utils/guards";
import {
  emptyHttpTaskDraft,
  httpTaskDraftFromConfig,
  httpTaskDraftGaps,
  httpTaskPayload,
  type HttpTaskDraft,
} from "../utils/httpTask";
import { useTaskDirectory } from "./useTaskDirectory";
import { createAutosaveController, gapBlocker } from "../utils/autosave";
import { initialReconcileState, reconcileDraft } from "../utils/draftReconcile";
import { useToast } from "./useToast";
import { useConfirm } from "./useConfirm";

export type { HttpTaskDraft };

/**
 * 新建直连任务种子的请求地址占位符。
 *
 * 后端保存闸口拒绝空地址（"存得下但必然失败"的配置不落盘），新建即落盘的
 * 种子因此需要一个非空但明确表达"待填"的值。执行时该占位符无对应变量、
 * 原样保留在 URL 里，请求必然失败——不会误登录到任何真实地址。
 * 面板的测试按钮据此提示用户先替换（见 HttpTasksPanel 的 sendTestRequest）。
 */
export const NEW_TASK_PLACEHOLDER_URL = "{gateway_host}";

/**
 * 任务详情里的 config 在接口层是宽类型 `TaskConfig`（三类任务共用信封字段）。
 * 直连任务的实际形状由后端 `TaskKind::Http` 保证，此处按 `HttpTaskConfig` 读取，
 * 缺字段由 `httpTaskDraftFromConfig` 逐项兜底，故转换是安全的。
 */
type TaskConfigAsHttp = HttpTaskConfig & { type?: string };

// 列表复用任务目录的混合拉取（含 5 秒守卫与首败通知），只取其中的直连任务视图
const { httpTasks: tasks, fetchDirectory, allTaskIds } = useTaskDirectory();
const httpTaskDraft = ref<HttpTaskDraft | null>(null);

/**
 * 列表行的「请求方法 / 请求地址」直接取任务摘要自带字段。
 *
 * 后端 `TaskSummary` 现在带 `url` 与 `http_method`（列表读取本来就把整个 JSON 解析
 * 出来了，顺手取字段是免费的），故面板不再需要对每条任务补发详情请求。
 */
// A11：执行类操作 busy 守卫（响应式 Set），防止连点重复提交
const duplicatingIds = useBusyIds(); // duplicateHttpTask 复制中
const exportingIds = useBusyIds(); // exportHttpTask 导出中

const { toastOnly } = useToast();
const { confirm } = useConfirm();

/** 当前编辑的是否为"还没落盘的新建草稿"（面板据此禁测试、改「删除」为「放弃」） */
const isNewDraft = computed(() => httpTaskDraft.value?._isNew === true);

// 拉取统一委托任务目录（force 语义与其他 fetch 一致）
function fetchHttpTasks(force = false): Promise<void> {
  return fetchDirectory(force);
}

/**
 * 列表刷新后对账（四个任务面板同一口径，见 `utils/draftReconcile`）。
 * 正在编辑的任务被别处删掉时，编辑器必须自己退出——否则每改一处都 PUT 一个不存在的
 * id、吃 404 与失败提示，草稿却留在页面上。
 * 走 `clearHttpTaskDraft` 而不是 `closeHttpTaskEditor`：后者"退出即落盘"，对已删除的
 * 任务再发一次 PUT 只会再吃一次 404。
 */
let reconcileState = initialReconcileState(tasks.value.map((t) => t.id));
watch(
  () => tasks.value.map((t) => t.id).join("\u0000"),
  () => {
    const listIds = tasks.value.map((t) => t.id);
    const result = reconcileDraft({
      state: reconcileState,
      draftId: httpTaskDraft.value?.id,
      listIds,
    });
    reconcileState = result.state;
    if (!result.gone) return;
    clearHttpTaskDraft();
    toastOnly(false, "正在编辑的任务已不存在，已退出编辑");
  },
);

// ---- 自动保存 ----

/** 落盘载荷（与 `persist` 实际发出去的那份完全一致，含 task_id） */
function draftPayload(draft: HttpTaskDraft): Record<string, unknown> {
  return { ...httpTaskPayload(draft), task_id: draft.id.trim() };
}

/** 落盘载荷指纹（`_isNew` 这类界面态不进载荷，故可作磁盘状态代表） */
function fingerprint(draft: HttpTaskDraft): string {
  return JSON.stringify(draftPayload(draft));
}

/**
 * 缺口清单：阻断自动保存（缺了必然登不上，发了也是坏配置）。
 *
 * 口径与「仓库导入 / 方案编辑器保存 / 后端 validate_task」同源，直接复用
 * `utils/httpTask` 的 `httpTaskDraftGaps`——此处曾另写一份（少了 `_isNew` 分支），
 * 两份判定各自演进时会出现"面板说能存、后端说不能"。
 */
function draftGaps(draft: HttpTaskDraft): string[] {
  return httpTaskDraftGaps(draft);
}

/**
 * 当前草稿的缺口（渲染用）。
 *
 * 自动保存被缺口跳过时**必须让用户看见**：状态字写着「改动自动保存」，编辑却一直
 * 没落盘（请求地址空着、前置请求填了一半…），用户会以为存过了，关掉编辑器才发现丢。
 */
const draftGapsNow = computed<string[]>(() =>
  httpTaskDraft.value ? httpTaskDraftGaps(httpTaskDraft.value) : [],
);

/**
 * 自动保存：状态机、在途请求序号与 debounce 都交给共享控制器（见 `utils/autosave`），
 * 本面板只提供三件口径——**什么算有改动**（载荷指纹）、**什么算发不出去**（缺口）、
 * **往哪儿落盘**。
 */
const autosave = createAutosaveController<HttpTaskDraft>({
  draft: httpTaskDraft,
  idOf: (draft) => draft.id,
  fingerprintOf: fingerprint,
  blockReasonOf: gapBlocker(draftGaps),
  persist: async (draft) => {
    await tasksApi.save(draft.id.trim(), draftPayload(draft));
    await fetchHttpTasks(true);
  },
  onSaved: (draft) => {
    // 首次落盘完成：它已经是磁盘上的任务了，不再是"新建草稿"
    draft._isNew = false;
  },
  toast: toastOnly,
  logScope: "http-task",
  detachedSubject: "上一份直连任务",
});

/** 关闭编辑器：在途 debounce 立即落盘（「退出即生效」承诺） */
async function closeHttpTaskEditor(): Promise<void> {
  await autosave.flush("close");
  clearHttpTaskDraft();
}

/** 清空编辑器状态（删除 / 放弃新建等场景）；草稿由控制器一并关掉 */
function clearHttpTaskDraft(): void {
  autosave.clear();
}

/** 打开指定直连任务的编辑器（先取详情再转草稿）。 */
async function showHttpTaskEditor(taskId: string): Promise<void> {
  // 换编辑对象：上一份草稿在途的改动先补发（不 await——打开必须立刻发生；那一发
  // 以 detached 方式落盘，不会回头改这份新草稿的指纹）
  void autosave.flush("switch");
  try {
    const data = await tasksApi.get(taskId);
    const config = (data?.config ?? {}) as unknown as TaskConfigAsHttp;
    const taskType = data?.summary?.task_type || config.type || "";
    if (taskType && taskType !== "http") {
      // 目录列表已按 task_type 过滤，正常流程不会走到这里；留着是为了深链/陈旧列表
      // 传错 id 时给出可执行的指引，而不是把浏览器任务当直连任务渲染成一片空字段
      toastOnly(false, "该任务不是直连任务，请切换到对应列表编辑");
      return;
    }
    const draft =
      // task_id 缺失时回退用请求 id：id 就是文件名 stem，两者本应一致
      httpTaskDraftFromConfig({ ...config, task_id: config.task_id || taskId });
    httpTaskDraft.value = draft;
    // 刚载入的草稿就是磁盘现状：基线对上了，用户不动它就不会发请求
    autosave.markBaseline(draft);
  } catch (error) {
    frontendLogger.error("http-task", "加载直连任务失败: " + taskId, error);
    toastOnly(false, extractApiError(error, "加载直连任务失败"));
  }
}

/**
 * 新建直连任务：只在内存里起一份草稿，**不落盘**。
 *
 * 与 `useTasks.createTask` 同口径（点开又退出不该在磁盘上留下一个没人改过的
 * `untitled_N.json`）：首次真实改动触发自动保存时才创建文件；`_isNew` 期间
 * 「删除」是放弃、导出走内存。
 *
 * 种子带一个**非空**请求地址占位符 `{gateway_host}`：后端 `validate_task` 对
 * http 类型拒绝空地址，用户改完第一个字段时这份草稿必须能通过保存校验。占位符
 * 原样保留在 URL 里、请求必然失败，绝不会误登录到真实地址（面板测试按钮据此提示先替换）。
 */
function createHttpTask(): void {
  // 新建也是"换编辑对象"：先补发上一份草稿在途的改动（否则那半秒内的编辑静默丢失）
  void autosave.flush("switch");
  // 取号对**三类任务**唯一（见 `allTaskIds`）：只看直连列表取号，会撞上浏览器任务里
  // 那个同名的 untitled_1，而后端写盘时会把浏览器桶里那份删掉
  const existingIds = allTaskIds();
  let newId = "untitled_1";
  let counter = 2;
  while (existingIds.has(newId)) {
    newId = `untitled_${counter++}`;
  }
  const draft: HttpTaskDraft = {
    ...emptyHttpTaskDraft(),
    id: newId,
    url: NEW_TASK_PLACEHOLDER_URL,
  };
  httpTaskDraft.value = draft;
  // 种子即"磁盘现状"的替身：基线对上 → 没改过就不会落盘
  autosave.markBaseline(draft);
}

async function deleteHttpTask(taskId: string): Promise<void> {
  // 新建草稿磁盘上还没有它：这个动作是"放弃"而不是"删除"，照旧走后端只会 404
  const draft = httpTaskDraft.value;
  const discard = !!draft && draft._isNew === true && draft.id === taskId;
  const ok = await confirm({
    title: discard ? "放弃新建直连任务" : "删除直连任务",
    message: discard
      ? "这个直连任务还没有保存过（改动后才会创建文件），放弃后当前内容会丢掉。"
      : `确定要删除直连任务「${taskId}」吗？绑定它的方案将无法再用直连方式登录。`,
    danger: true,
  });
  if (!ok) return;
  if (discard) {
    clearHttpTaskDraft();
    return;
  }
  try {
    // 被删的是当前打开的任务时先关编辑器（自动保存可能正要写回一个已删除的 id）
    if (httpTaskDraft.value && !httpTaskDraft.value._isNew && httpTaskDraft.value.id === taskId) {
      clearHttpTaskDraft();
    }
    await tasksApi.delete(taskId);
    await fetchHttpTasks(true);
    frontendLogger.info("http-task", "直连任务删除成功: " + taskId);
    toastOnly(true, "直连任务已删除");
  } catch (error) {
    frontendLogger.error("http-task", "删除直连任务失败: " + taskId, error);
    toastOnly(false, extractApiError(error, "删除失败"));
  }
}

async function duplicateHttpTask(taskId: string): Promise<void> {
  // A11：busy 守卫，避免连点生成重复副本
  if (duplicatingIds.has(taskId)) return;
  duplicatingIds.add(taskId);
  try {
    const data = await tasksApi.get(taskId);
    const config = (data?.config ?? {}) as unknown as TaskConfigAsHttp;

    // 副本 id/名称去重口径与 useTasks.duplicateTask 一致（_copy / _copy_N，
    // 名称后缀同步编号），避免同一门户"复制两次"得到两个同名任务
    const baseId = taskId.replace(/_copy(_\d+)?$/, "").replace(/^untitled(_\d+)?$/, "untitled");
    // 副本 id 与新建同一口径：对三类任务唯一（撞上别桶同 id 会删掉那份文件）
    const existingIds = allTaskIds();
    const baseName = String(data?.summary?.name || config.name || "")
      .replace(/\s*[（(]副本\s*\d*[）)]\s*$/, "")
      .replace(/\s*\(\d+\)$/, "");
    let newId = baseId + "_copy";
    let suffix = "（副本）";
    let counter = 2;
    while (existingIds.has(newId)) {
      newId = baseId + "_copy_" + counter;
      suffix = `（副本${counter}）`;
      counter++;
    }

    // 直接落盘副本（自动保存模式下没有"复制成草稿"的中间态）
    const payload = { ...httpTaskPayload(httpTaskDraftFromConfig({ ...config, task_id: newId })), task_id: newId };
    payload.name = baseName + suffix;
    await tasksApi.save(newId, payload);
    await fetchHttpTasks(true);
    await showHttpTaskEditor(newId);
    frontendLogger.info("http-task", `已复制直连任务: ${taskId} → ${newId}`);
  } catch (error) {
    frontendLogger.error("http-task", "复制直连任务失败: " + taskId, error);
    toastOnly(false, extractApiError(error, "复制失败"));
  } finally {
    duplicatingIds.delete(taskId);
  }
}

async function exportHttpTask(taskId: string): Promise<void> {
  // A11：busy 守卫，避免连点重复下载导出文件
  if (exportingIds.has(taskId)) return;
  exportingIds.add(taskId);
  try {
    const draft = httpTaskDraft.value;
    if (draft && draft._isNew === true && draft.id === taskId) {
      // 新建草稿还没落盘：后端没有这个 id，按 id 导出必 404 → 直接导出内存里的草稿
      downloadBlob(JSON.stringify(draftPayload(draft), null, 2), `${taskId}.json`, "application/json");
      return;
    }
    // 经后端导出端点获取完整任务配置（与 /api/tasks/import 格式对应，可直接回导）
    const data = await tasksApi.export(taskId);
    downloadBlob(JSON.stringify(data, null, 2), `${taskId}.json`, "application/json");
    frontendLogger.info("http-task", "直连任务已导出: " + taskId);
  } catch (error) {
    frontendLogger.error("http-task", "导出直连任务失败: " + taskId, error);
    toastOnly(false, extractApiError(error, "导出失败"));
  } finally {
    exportingIds.delete(taskId);
  }
}

/**
 * 导入条目是否为直连任务。
 *
 * 兼容三种形态：磁盘文件（顶层 `type`）、导出详情（`config.type`）、
 * 以及 `{ data: {...} }` 信封（取内层再次判定）。导入接口认的是后端的
 * `TaskKind`，缺 `type` 会被拒；此处提前判定是为了把"混进来的浏览器任务"
 * 挡在列表之外——后端会照单收下，而列表按 `type` 过滤，导入成功却看不见。
 */
function isHttpImportEntry(item: unknown): boolean {
  if (!item || typeof item !== "object") return false;
  const obj = item as Record<string, unknown>;
  const typeOf = (v: unknown): string => (typeof v === "string" ? v.trim().toLowerCase() : "");
  if (typeOf(obj.type) === "http") return true;
  const config = obj.config;
  if (config && typeof config === "object" && typeOf((config as Record<string, unknown>).type) === "http") {
    return true;
  }
  return isHttpImportEntry(obj.data);
}

async function importHttpTask(): Promise<void> {
  const file = await pickFile(".json");
  if (!file) return;
  try {
    const parsed = JSON.parse(await file.text());
    const items: unknown[] = Array.isArray(parsed) ? parsed : [parsed];
    const httpItems = items.filter(isHttpImportEntry);
    if (httpItems.length === 0) {
      toastOnly(false, '文件里没有直连任务（条目的 type 需为 "http"）');
      return;
    }
    const result = await tasksApi.import(httpItems);
    await fetchHttpTasks(true);
    const imported = result?.imported ?? httpItems.length;
    const skipped = items.length - httpItems.length;
    toastOnly(
      true,
      skipped > 0 ? `已导入 ${imported} 个直连任务，忽略 ${skipped} 个非直连条目` : `已导入 ${imported} 个直连任务`,
    );
  } catch (e) {
    frontendLogger.warn("http-task", "导入失败: " + (e as Error).message);
    toastOnly(false, "导入失败：" + extractApiError(e, "文件不是有效的任务 JSON"));
  }
}

/** 行内类型徽标文案：列表已按 `task_type === "http"` 过滤，未知取值原样透出以免标签说谎 */
function httpTaskTypeLabel(task: TaskItem): string {
  const type = String(task.task_type ?? "");
  return type === "" || type === "http" ? "直连" : type;
}

export function useHttpTasks() {
  return {
    httpTasks: tasks,
    httpTaskDraft,
    isNewDraft,
    autosaveState: autosave.autosaveState,
    draftGapsNow,
    duplicatingIds,
    exportingIds,
    fetchHttpTasks,
    deleteHttpTask,
    showHttpTaskEditor,
    createHttpTask,
    closeHttpTaskEditor,
    clearHttpTaskDraft,
    duplicateHttpTask,
    exportHttpTask,
    importHttpTask,
    httpTaskTypeLabel,
  };
}
