/**
 * 直连任务（`type: "http"`）状态与操作（单例）。
 *
 * 结构与 `useTasks` 同构：列表来自 `useTaskDirectory` 单次拉取的 httpTasks 视图，
 * 草稿是平铺的 `HttpTaskDraft`（草稿 ⇄ 落盘载荷的互转与缺口校验见 `utils/httpTask`），
 * 增删改查复用 `tasksApi`（后端一套 CRUD 按 `type` 分派）。
 *
 * 与 `useTasks` 的差异（为什么）：
 * - 草稿是强类型平铺字段而非一段 JSON 文本（`HttpTaskFields` 原地修改），故没有
 *   JSON 校验/格式化/模板加载/危险步骤检测——那些都是浏览器任务"手写 JSON"的产物。
 * - 缺口校验委托 `httpTaskDraftGaps`（id 形态 + 请求地址），没有 `success_condition`
 *   这类浏览器任务特有语义。
 * - 没有「执行」入口：直连任务不经 Python Worker，验证路径是发一次测试请求
 *   （见 `useHttpTaskTest`，凭据由宿主传入，本模块不关心）。
 */

import { ref } from "vue";
import type { HttpTaskConfig, TaskItem } from "../api/types";
import { tasksApi } from "../api";
import { extractApiError } from "../api/client";
import { frontendLogger } from "../utils/logger";
import { downloadBlob, pickFile } from "../utils/file";
import { useBusyIds } from "../utils/guards";
import {
  HTTP_TASK_ID_PATTERN,
  emptyHttpTaskDraft,
  httpTaskDraftFromConfig,
  httpTaskDraftGaps,
  httpTaskPayload,
  type HttpTaskDraft,
} from "../utils/httpTask";
import { useTaskDirectory } from "./useTaskDirectory";
import { useDirtySnapshot } from "./useDirtySnapshot";
import { useToast } from "./useToast";
import { useConfirm } from "./useConfirm";

export type { HttpTaskDraft };

/**
 * 任务详情里的 config 在接口层是宽类型 `TaskConfig`（三类任务共用信封字段）。
 * 直连任务的实际形状由后端 `TaskKind::Http` 保证，此处按 `HttpTaskConfig` 读取，
 * 缺字段由 `httpTaskDraftFromConfig` 逐项兜底，故转换是安全的。
 */
type TaskConfigAsHttp = HttpTaskConfig & { type?: string };

// 列表复用任务目录的混合拉取（含 5 秒守卫与首败通知），只取其中的直连任务视图
const { httpTasks: tasks, fetchDirectory } = useTaskDirectory();
const httpTaskDraft = ref<HttpTaskDraft | null>(null);

/**
 * 列表行的「请求方法 / 请求地址」直接取任务摘要自带字段。
 *
 * 后端 `TaskSummary` 现在带 `url` 与 `http_method`（列表读取本来就把整个 JSON 解析
 * 出来了，顺手取字段是免费的），故面板不再需要对每条任务补发详情请求——此前那套
 * 「详情缓存 + 并发补齐 + 刷新淘汰」的 N+1 机制已随之删除。
 */
// A11：执行类操作 busy 守卫（响应式 Set），防止连点重复提交
const duplicatingIds = useBusyIds(); // duplicateHttpTask 复制中
const exportingIds = useBusyIds(); // exportHttpTask 导出中

const { toastOnly } = useToast();
const { confirm } = useConfirm();

// 拉取统一委托任务目录（force 语义与其他 fetch 一致）
function fetchHttpTasks(force = false): Promise<void> {
  return fetchDirectory(force);
}

// ---- 编辑器 dirty 快照：通用实现见 useDirtySnapshot（与浏览器/脚本任务同口径）----
const {
  setDraft: setHttpTaskDraft,
  isDirty: isDraftDirty,
  confirmDiscardIfDirty: confirmDiscardHttpTaskIfDirty,
  resetSnapshot: resetHttpTaskSnapshot,
} = useDirtySnapshot(httpTaskDraft, { entityName: "直连任务" });

/** 关闭直连任务编辑器（带 dirty 确认）。 */
async function closeHttpTaskEditor(): Promise<void> {
  if (!(await confirmDiscardHttpTaskIfDirty())) return;
  clearHttpTaskDraft();
}

/** 清空草稿（保存成功等无需确认的场景）。 */
function clearHttpTaskDraft(): void {
  httpTaskDraft.value = null;
  resetHttpTaskSnapshot();
}

/** 打开「新建直连任务」草稿。 */
async function showNewHttpTaskDraft(): Promise<void> {
  // 打开/替换草稿前先确认当前草稿是否有未保存改动，避免静默丢弃
  if (!(await confirmDiscardHttpTaskIfDirty())) return;
  setHttpTaskDraft(emptyHttpTaskDraft());
}

/** 打开指定直连任务的编辑器（先取详情再转草稿）。 */
async function showHttpTaskEditor(taskId: string): Promise<void> {
  if (!(await confirmDiscardHttpTaskIfDirty())) return;
  try {
    const data = await tasksApi.get(taskId);
    // 后端返回 TaskDetail: { summary: { id, name, description, task_type }, config: {...} }
    const config = (data?.config ?? {}) as unknown as TaskConfigAsHttp;
    const taskType = data?.summary?.task_type || config.type || "";
    if (taskType && taskType !== "http") {
      // 目录列表已按 task_type 过滤，正常流程不会走到这里；留着是为了深链/陈旧列表
      // 传错 id 时给出可执行的指引，而不是把浏览器任务当直连任务渲染成一片空字段
      toastOnly(false, "该任务不是直连任务，请切换到对应标签页编辑");
      return;
    }
    setHttpTaskDraft(
      // task_id 缺失时回退用请求 id：id 就是文件名 stem，两者本应一致
      httpTaskDraftFromConfig({ ...config, task_id: config.task_id || taskId }),
    );
  } catch (error) {
    frontendLogger.error("http-task", "加载直连任务失败: " + taskId, error);
    toastOnly(false, extractApiError(error, "加载直连任务失败"));
  }
}

/** 保存请求 in-flight 标记：防连点并发两次 PUT（新建时第二次会撞"已存在"） */
const httpTaskSaving = ref(false);

async function saveHttpTask(): Promise<void> {
  if (httpTaskSaving.value) return;
  const draft = httpTaskDraft.value;
  if (!draft) return;
  const id = draft.id.trim();
  // id 形态：新建时由用户输入，已保存的任务来自服务端（本不该非法）；
  // 仍统一校验，避免陈旧/手工构造的草稿把非法 id 提交上去
  if (!HTTP_TASK_ID_PATTERN.test(id)) {
    toastOnly(false, "任务ID需为 1-64 位字母、数字、下划线或连字符");
    return;
  }
  // 缺口清单复用 utils/httpTask 的统一判定（与仓库导入、方案编辑器同口径）
  const gaps = httpTaskDraftGaps(draft);
  if (gaps.length > 0) {
    toastOnly(false, `请先填写：${gaps.join("、")}`);
    return;
  }
  if (!draft.name.trim()) {
    toastOnly(false, "请填写任务名称");
    return;
  }
  // 凭据变换脚本是要在登录时**执行**的 JavaScript：与浏览器任务的 eval/custom_js
  // 步骤同级（`useTasks.saveTask` 同样在保存前弹一次确认），故这里给一次显式确认，
  // 讲清它的执行环境与能力边界，而不是只写"包含脚本"
  if (draft.crypto_script.trim()) {
    const ok = await confirm({
      title: "任务包含凭据变换脚本",
      message: "保存后登录时会执行其中的 JavaScript（沙箱内运行、无网络与文件访问）。确定要继续保存吗？",
      danger: true,
    });
    if (!ok) return;
  }

  httpTaskSaving.value = true;
  try {
    // 后端 PUT 需要完整 TaskKind JSON（含 type），载荷构造集中在 httpTaskPayload
    const payload: Record<string, unknown> = { ...httpTaskPayload(draft), task_id: id };
    const data = await tasksApi.save(id, payload);
    clearHttpTaskDraft();
    await fetchHttpTasks(true);
    toastOnly(true, data?.message || "保存成功");
  } catch (error) {
    frontendLogger.error("http-task", "保存直连任务失败", error);
    toastOnly(false, extractApiError(error, "保存失败"));
  } finally {
    httpTaskSaving.value = false;
  }
}

async function deleteHttpTask(taskId: string): Promise<void> {
  const ok = await confirm({
    title: "删除直连任务",
    message: `确定要删除直连任务「${taskId}」吗？绑定它的方案将无法再用直连方式登录。`,
    danger: true,
  });
  if (!ok) return;
  try {
    await tasksApi.delete(taskId);
    // 被删的正是当前编辑的对象时必须关掉编辑器：留着会让人以为"还能保存"，
    // 而保存其实会以同一 id 重新新建（删除动作看起来没生效）
    if (httpTaskDraft.value && !httpTaskDraft.value._isNew && httpTaskDraft.value.id === taskId) {
      clearHttpTaskDraft();
    }
    await fetchHttpTasks(true);
    frontendLogger.info("http-task", "直连任务删除成功: " + taskId);
    toastOnly(true, "直连任务已删除");
  } catch (error) {
    frontendLogger.error("http-task", "删除直连任务失败: " + taskId, error);
    toastOnly(false, extractApiError(error, "删除失败"));
  }
}

async function duplicateHttpTask(taskId: string): Promise<void> {
  // 复制会整体替换当前草稿，先确认未保存改动
  if (!(await confirmDiscardHttpTaskIfDirty())) return;
  // A11：busy 守卫，避免连点生成 _copy 与 _copy_2 等重复草稿
  if (duplicatingIds.has(taskId)) return;
  duplicatingIds.add(taskId);
  try {
    const data = await tasksApi.get(taskId);
    const config = (data?.config ?? {}) as unknown as TaskConfigAsHttp;

    // 副本 id/名称去重口径与 useTasks.duplicateTask 一致（_copy / _copy_N，
    // 名称后缀同步编号），避免同一门户"复制两次"得到两个同名任务
    const baseId = taskId.replace(/_copy(_\d+)?$/, "");
    const existingIds = new Set((tasks.value || []).map((t) => t.id));
    const baseName = String(data?.summary?.name || config.name || "").replace(/\s*[（(]副本\s*\d*[）)]\s*$/, "");
    let newId = baseId + "_copy";
    let suffix = "（副本）";
    let counter = 2;
    while (existingIds.has(newId)) {
      newId = baseId + "_copy_" + counter;
      suffix = `（副本${counter}）`;
      counter++;
    }

    const draft = httpTaskDraftFromConfig({ ...config, task_id: newId });
    draft.name = baseName + suffix;
    draft._isNew = true;
    setHttpTaskDraft(draft);
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
    httpTaskSaving,
    duplicatingIds,
    exportingIds,
    fetchHttpTasks,
    saveHttpTask,
    deleteHttpTask,
    showNewHttpTaskDraft,
    showHttpTaskEditor,
    closeHttpTaskEditor,
    clearHttpTaskDraft,
    setHttpTaskDraft,
    isDraftDirty,
    confirmDiscardHttpTaskIfDirty,
    duplicateHttpTask,
    exportHttpTask,
    importHttpTask,
    httpTaskTypeLabel,
  };
}
