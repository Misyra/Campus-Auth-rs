/**
 * 浏览器任务状态与操作（单例）——自动保存模式。
 *
 * 编辑模型（方案 G 终稿）：**没有草稿态与显式保存**。打开编辑器后所有字段
 * 就地修改，变更经 debounce 静默 PUT 到后端（JSON 语法非法时不落盘、只标红），
 * 「放弃未保存的修改？」确认链随草稿态一并退役。
 *
 * 列表数据仍由 useTaskDirectory 单次拉取提供（任务/脚本共用一个混合列表源）。
 */

import { computed, ref, watch } from "vue";
import type { DangerStep, TaskConfig } from "../api/types";
import { tasksApi } from "../api";
import { extractApiError } from "../api/client";
import { frontendLogger } from "../utils/logger";
import { downloadBlob, pickFile } from "../utils/file";
import { useBusyIds } from "../utils/guards";
import { useTaskDirectory } from "./useTaskDirectory";
import { createAutosaveController } from "../utils/autosave";
import { initialReconcileState, reconcileDraft } from "../utils/draftReconcile";
import { useToast } from "./useToast";
import { useConfirm } from "./useConfirm";

export interface BrowserTaskDraft {
  id: string;
  name: string;
  description: string;
  url: string;
  json: string;
  /**
   * 磁盘上还没有这个任务（新建草稿）。
   *
   * 自动保存模式下没有"显式保存"这一步，**新建也不再先落一份种子文件**：
   * 什么都没改就退出不该在磁盘上留下 `untitled_N.json`。于是新建态的草稿需要
   * 自己说明"未落盘"，删除（此时是"放弃"）、导出、调试运行三个入口据此改道。
   */
  _isNew: boolean;
}

// 危险步骤类型：涵盖后端兼容的多个别名。StepEditor 实际产出 `evaluate`（执行 JS），
// eval / custom_js 为历史别名，三者均需视为危险（历史遗留：前端危险步骤检测失效）
const DANGEROUS_STEP_TYPES = new Set(["eval", "custom_js", "evaluate"]);

// 列表来自任务目录（与脚本共用单次拉取，含 5 秒守卫与首败通知）
const { browserTasks: tasks, fetchDirectory, allTaskIds } = useTaskDirectory();
const editingTask = ref<BrowserTaskDraft | null>(null);
const jsonError = ref("");

// A11：执行类操作 busy 守卫（响应式 Set），防止连点重复提交
const duplicatingIds = useBusyIds(); // duplicateTask 复制中
const exportingIds = useBusyIds(); // exportTask 导出中

const { toastOnly } = useToast();
const { confirm } = useConfirm();

/** 当前编辑的是否为"还没落盘的新建草稿"（面板据此禁运行、改「删除」为「放弃」） */
const isNewDraft = computed(() => editingTask.value?._isNew === true);

// 拉取统一委托任务目录（force 语义与其他 fetch 一致）
function fetchTasks(force = false): Promise<void> {
  return fetchDirectory(force);
}

/**
 * 列表刷新后对账（四个任务面板同一口径，见 `utils/draftReconcile`）。
 * 正在编辑的任务被别处删掉时，编辑器必须自己退出——否则每改一处都 PUT 一个不存在的
 * id、吃 404 与失败提示，草稿却留在页面上。
 * 走 `clearTaskDraft` 而不是 `closeTaskEditor`：后者"退出即落盘"，对已删除的任务再发
 * 一次 PUT 只会再吃一次 404。
 */
let reconcileState = initialReconcileState(tasks.value.map((t) => t.id));
watch(
  () => tasks.value.map((t) => t.id).join("\u0000"),
  () => {
    const listIds = tasks.value.map((t) => t.id);
    const result = reconcileDraft({
      state: reconcileState,
      draftId: editingTask.value?.id,
      listIds,
    });
    reconcileState = result.state;
    if (!result.gone) return;
    clearTaskDraft();
    toastOnly(false, "正在编辑的任务已不存在，已退出编辑");
  },
);

/** 扫描任务步骤中的危险类型（执行 JS 类）。保留导出：仓库导入预览等场景复用。 */
export function detectDangerousSteps(config: { steps?: Array<Record<string, unknown>> }): DangerStep[] {
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

// ---- 自动保存 ----

/**
 * 当前草稿里的危险步骤（执行 JS 类）。
 *
 * 旧版在**显式保存前**弹一次「检测到危险步骤」确认；自动保存模式下那个时机已经不存在
 * （改动停手半秒就落盘），确认框若还挂在"保存前"就等于永远不弹。改为在编辑器里常驻提示：
 * `evaluate` / `eval` 能在页面上下文里跑任意 JS，这层提示是"用户知不知道自己在存什么"
 * 的唯一保障（导入的适配尤其需要）。
 *
 * JSON 语法错误时返回空数组：那种内容根本不会落盘，另有一条 JSON 错误提示负责。
 * 只读解析、不触碰 `jsonError`——computed 里写共享状态会在渲染期产生副作用。
 */
const dangerousSteps = computed<DangerStep[]>(() => {
  const json = editingTask.value?.json ?? "";
  if (!json.trim()) return [];
  try {
    return detectDangerousSteps(JSON.parse(json) as { steps?: Array<Record<string, unknown>> });
  } catch {
    return [];
  }
});

/** 草稿 → PUT 载荷。JSON 语法非法时返回 null（不落盘，只标红）。
 *
 * `quiet` 供取指纹时使用：比对指纹不该顺手写 `jsonError`（草稿合法时那是一次
 * 无意义的清空，非法时则会把"用户还没改过"的界面直接标红）。 */
function draftToPayload(draft: BrowserTaskDraft, quiet = false): Record<string, unknown> | null {
  let config: Record<string, unknown>;
  try {
    config = JSON.parse(draft.json);
  } catch (e) {
    if (!quiet) jsonError.value = (e as Error).message;
    return null;
  }
  const payload = { ...config };
  payload.name = draft.name || (config.name as string);
  payload.description = draft.description || (config.description as string);
  payload.url = draft.url || (config.url as string) || "{{LOGIN_URL}}";
  // 确保 type 字段存在（后端 TaskKind 反序列化需要；本编辑器只产出浏览器任务）
  if (!payload.type) {
    payload.type = "browser";
  }
  delete payload.version;
  delete payload.source;
  return payload;
}

/** 落盘载荷指纹（`_isNew` 这类界面态不进载荷，故可作磁盘状态代表） */
function fingerprint(draft: BrowserTaskDraft): string | null {
  const payload = draftToPayload(draft, true);
  return payload === null ? null : JSON.stringify(payload);
}

/**
 * 自动保存：状态机、在途请求序号与 debounce 都交给共享控制器（见 `utils/autosave`），
 * 本面板只提供三件口径——**什么算有改动**（载荷指纹）、**什么算发不出去**（闸口）、
 * **往哪儿落盘**。
 *
 * 与另外三个面板的差别只在闸口：浏览器任务没有字段级缺口，**JSON 解析不出载荷就不落盘**
 * （`jsonError` 由编辑器标红；状态字另由面板的 `jsonGate` 改口）。
 */
const autosave = createAutosaveController<BrowserTaskDraft>({
  draft: editingTask,
  idOf: (draft) => draft.id,
  fingerprintOf: fingerprint,
  blockReasonOf: (draft) => {
    // 非 quiet 调用：JSON 非法时顺手把 jsonError 写上，提示里才有解析器的原话
    if (draftToPayload(draft) !== null) return null;
    return draft.json.trim()
      ? `JSON 语法错误，改动未保存：${jsonError.value}`
      : "JSON 配置为空，改动未保存";
  },
  persist: async (draft) => {
    const payload = draftToPayload(draft);
    if (payload === null) return; // 闸口已拦；此处只为类型收窄
    await tasksApi.save(draft.id, payload);
    await fetchTasks(true);
  },
  onSaved: (draft) => {
    // 首次落盘完成：它已经是磁盘上的任务了，不再是"新建草稿"
    draft._isNew = false;
    jsonError.value = "";
  },
  toast: toastOnly,
  logScope: "tasks",
  detachedSubject: "上一份任务",
});

/** 关闭编辑器：在途 debounce 立即落盘，保证「退出即生效」承诺 */
async function closeTaskEditor(): Promise<void> {
  await autosave.flush("close");
  clearTaskDraft();
}

/** 清空编辑器状态（删除 / 放弃新建等无需再保存的场景）。 */
function clearTaskDraft(): void {
  autosave.clear(); // 顺带把 editingTask 置空：见 AutosaveController.clear 的注释
  jsonError.value = "";
}

/** 打开指定任务的编辑器（加载详情进草稿；深链 ?task=<id> 也走这里） */
async function showTaskEditor(taskId: string): Promise<void> {
  // 换编辑对象：上一份草稿在途的改动先补发（不 await——打开必须立刻发生；那一发
  // 以 detached 方式落盘，不会回头改这份新草稿的指纹）
  void autosave.flush("switch");
  try {
    const data = await tasksApi.get(taskId);
    // 后端返回 TaskDetail: { summary: { id, name, description, task_type }, config: {...} }
    const summary = data.summary;
    const taskConfig: TaskConfig = data.config ?? {};
    const taskType = summary?.task_type || taskConfig.type;

    if (taskType === "script") {
      // 脚本类型由「任务」页的脚本 Tab 编辑器负责；此处不跨模块转交（避免 useTasks→useScripts 循环依赖），
      // 任务列表本身已过滤为浏览器任务，正常流程不会走到该分支
      toastOnly(false, "该任务为脚本类型，请切换到「脚本」标签页编辑");
      return;
    }
    const draft: BrowserTaskDraft = {
      id: taskId,
      name: summary?.name || taskConfig.name || "",
      description: summary?.description || taskConfig.description || "",
      url: taskConfig.url || "",
      json: JSON.stringify(taskConfig, null, 2),
      _isNew: false,
    };
    editingTask.value = draft;
    // 刚载入的草稿就是磁盘现状：基线对上了，用户不动它就不会发请求
    autosave.markBaseline(draft);
    jsonError.value = "";
  } catch (error) {
    frontendLogger.error("tasks", "加载任务失败: " + taskId, error);
    toastOnly(false, "加载任务失败");
  }
}

/**
 * 新建任务：只在内存里起一份草稿，**不落盘**。
 *
 * 自动保存模式下"新建就先写一份种子文件"会留下噪音（点开又退出 → 磁盘上多一个
 * 没人改过的 `untitled_N.json`）。改成：首次**真实改动**触发自动保存时才创建文件
 * （`_isNew` 期间删除即放弃、导出走内存、调试运行禁用）。
 *
 * ID 仍是本地生成的 `untitled_N`（撞目录里的已有 id 时递增）：直连任务与浏览器
 * 任务的 ID 没有外部含义，不像脚本 ID 那样是文件名兼定时任务引用值，不必让用户先命名。
 */
function createTask(): void {
  // 新建也是"换编辑对象"：先补发上一份草稿在途的改动（否则那半秒内的编辑静默丢失）
  void autosave.flush("switch");
  // 取号对**三类任务**唯一：后端写盘时会把另外两个桶里同 id 的文件删掉，只看本类列表
  // 会让"浏览器任务与直连任务都叫 untitled_1"这种撞车静默删掉一份文件
  const existingIds = allTaskIds();
  let newId = "untitled_1";
  let counter = 2;
  while (existingIds.has(newId)) {
    newId = `untitled_${counter++}`;
  }
  // 种子的 steps 不能为空（后端 validate_task 对 browser 类型拒绝空 steps——
  // 空步骤序列的任务执行等于什么都不做，属于"存得下但必然失败"的配置）。
  // 用一步 sleep 1000ms 占位：通过校验、执行无害，用户在编辑页的 JSON 里替换。
  const seed: Record<string, unknown> = {
    type: "browser",
    task_id: newId,
    name: "未命名任务",
    description: "",
    url: "{{LOGIN_URL}}",
    steps: [
      {
        id: "placeholder_step",
        type: "sleep",
        description: "占位步骤：编辑 JSON 时替换成真实登录步骤",
        duration: 1000,
      },
    ],
  };
  const draft: BrowserTaskDraft = {
    id: newId,
    name: seed.name as string,
    description: "",
    url: seed.url as string,
    json: JSON.stringify(seed, null, 2),
    _isNew: true,
  };
  editingTask.value = draft;
  // 种子即"磁盘现状"的替身：基线对上 → 没改过就不会落盘
  autosave.markBaseline(draft);
  jsonError.value = "";
}

/**
 * 元信息 → JSON 单向同步：把编辑器上方的 name/description 输入框写回 JSON 文本。
 * 方向易混淆：本函数是"表单覆盖 JSON"，与 syncJsonToMeta 相反；JSON 无效时静默跳过。
 */
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

/**
 * JSON → 元信息单向同步：从 JSON 文本读出 name/description 回填上方输入框。
 * 方向与 syncMetaToJson 相反（"JSON 覆盖表单"）；仅回填 JSON 中显式存在的键，JSON 无效时静默跳过。
 */
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
    const nextJson = JSON.stringify(taskConfig, null, 2);
    // 覆盖前要一次明确同意：自动保存模式下"替换"等于立刻写盘，而且没有撤销那一步
    // （脚本面板的「加载示例模板」是同一口径，本按钮此前是一键覆盖手写好的步骤）
    const current = editingTask.value.json.trim();
    if (current && current !== nextJson.trim()) {
      const ok = await confirm({
        title: "加载默认模板",
        message: "当前 JSON 内容会被默认模板覆盖，并立即保存到磁盘。是否继续？",
        danger: true,
      });
      if (!ok) return;
    }
    editingTask.value.json = nextJson;
    const name = summary?.name || taskConfig.name || "";
    if (name) editingTask.value.name = name;
    const desc = summary?.description || taskConfig.description || "";
    if (desc) editingTask.value.description = desc;
    jsonError.value = "";
    // 模板替换即变更：交给自动保存落盘
    void autosave.saveNow(editingTask.value);
  } catch (error) {
    frontendLogger.error("tasks", "加载模板失败: " + templateId, error);
    toastOnly(false, "加载模板失败");
  }
}

/** 校验编辑器 JSON 文本语法，仅刷新 jsonError 提示，不改写内容 */
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

/** 格式化编辑器 JSON 文本（2 空格缩进）；语法错误时提示且不改写原文 */
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
  // A11：busy 守卫，避免连点生成 _copy 与 _copy_2 等重复任务
  if (duplicatingIds.has(taskId)) return;
  duplicatingIds.add(taskId);
  try {
    const data = await tasksApi.get(taskId);
    // 解包 TaskDetail 嵌套结构
    const summary = data.summary;
    const taskConfig: TaskConfig = data.config ?? {};

    const baseId = taskId.replace(/_copy(_\d+)?$/, "").replace(/^untitled(_\d+)?$/, "untitled");
    // 与新建同一口径：副本 id 也必须对三类任务唯一（否则会删掉别桶里同 id 的文件）
    const existingIds = allTaskIds();
    const baseName = (summary?.name || taskConfig.name || "").replace(/\s*\(副本\)(\s*\d+)?$/, "").replace(/\s*\(\d+\)$/, "");
    let newId = baseId + "_copy";
    let suffix = " (副本)";
    let counter = 2;
    while (existingIds.has(newId)) {
      newId = baseId + "_copy_" + counter;
      suffix = ` (副本${counter})`;
      counter++;
    }
    const payload = { ...taskConfig } as Record<string, unknown>;
    payload.type = "browser";
    payload.task_id = newId;
    payload.name = (baseName + suffix).trim();
    await tasksApi.save(newId, payload);
    await fetchTasks(true);
    await showTaskEditor(newId);
    frontendLogger.info("tasks", `已复制任务: ${taskId} → ${newId}`);
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
    const draft = editingTask.value;
    if (draft && draft._isNew && draft.id === taskId) {
      // 新建草稿还没落盘：后端没有这个 id，按 id 导出必 404 → 直接导出内存里的草稿
      const payload = draftToPayload(draft);
      if (payload === null) {
        toastOnly(false, "JSON 语法错误，修正后才能导出");
        return;
      }
      downloadBlob(JSON.stringify(payload, null, 2), `${taskId}.json`, "application/json");
      return;
    }
    // 经后端导出端点获取完整任务配置（与 /api/tasks/import 格式对应）
    const data = await tasksApi.export(taskId);
    downloadBlob(JSON.stringify(data, null, 2), `${taskId}.json`, "application/json");
    frontendLogger.info("tasks", "任务已导出");
  } catch (error) {
    frontendLogger.error("tasks", "导出任务失败: " + taskId, error);
    toastOnly(false, extractApiError(error, "导出失败"));
  } finally {
    exportingIds.delete(taskId);
  }
}

/**
 * 导入条目是否为浏览器任务。
 *
 * 与 `useHttpTasks.isHttpImportEntry` 同口径。不加这层过滤的后果：用户从「浏览器任务」
 * 导入一份含直连 / 脚本条目的文件时，后端会照单收下并按 `type` 分流落盘，而本列表按
 * `task_type` 过滤 → 列表毫无变化、toast 却说"已导入 N 个任务"，用户只能得出"任务丢了"。
 *
 * `type` 缺省视为浏览器任务：导出端点的 `config` 与磁盘文件形态历史上都不强制带
 * `type`，那种写法只有浏览器任务一种含义（后端 TaskKind 反序列化时会再判一次）。
 */
function isBrowserImportEntry(item: unknown): boolean {
  if (!item || typeof item !== "object") return false;
  const obj = item as Record<string, unknown>;
  const typeOf = (v: unknown): string => (typeof v === "string" ? v.trim().toLowerCase() : "");
  // API 信封 `{ code, data: {...} }`：取内层再判定
  if (obj.data && typeof obj.data === "object" && !Array.isArray(obj.data)) {
    return isBrowserImportEntry(obj.data);
  }
  const config = obj.config;
  for (const t of [
    typeOf(obj.type),
    config && typeof config === "object"
      ? typeOf((config as Record<string, unknown>).type)
      : "",
  ]) {
    if (t) return t === "browser";
  }
  return true;
}

async function importTask(): Promise<void> {
  const file = await pickFile(".json");
  if (!file) return;
  try {
    const text = await file.text();
    const data = JSON.parse(text);
    const payload: unknown[] = Array.isArray(data) ? data : [data];
    // 只把浏览器任务交给后端，其余条目跳过并说明（口径同直连面板）
    const browserItems = payload.filter(isBrowserImportEntry);
    if (!browserItems.length) {
      toastOnly(false, "文件里没有浏览器任务");
      return;
    }
    const result = await tasksApi.import(browserItems);
    const imported = result?.imported ?? browserItems.length;
    const ignored = payload.length - browserItems.length;
    // 后端逐条导入互不中止且恒回 200：imported=0 + failed 非空时若仍按成功弹
    // "已导入 0 个任务"会把失败伪装成成功，必须把失败明细透出
    const failed = result?.failed ?? [];
    await fetchTasks(true);
    const ignoredNote = ignored ? `，忽略 ${ignored} 个非浏览器条目` : "";
    if (failed.length > 0) {
      const reason = failed[0]?.reason ?? "未知原因";
      if (imported === 0) {
        toastOnly(false, `导入失败：${failed.length} 个任务未通过校验（${reason}）${ignoredNote}`);
      } else {
        toastOnly(false, `已导入 ${imported} 个任务，${failed.length} 个失败（${reason}）${ignoredNote}`);
      }
      return;
    }
    toastOnly(true, ignored ? `已导入 ${imported} 个任务${ignoredNote}` : `已导入 ${imported} 个任务`);
  } catch (e) {
    frontendLogger.warn("tasks", "导入失败: " + (e as Error).message);
    toastOnly(false, "导入失败：" + extractApiError(e, "文件不是有效的任务 JSON"));
  }
}

async function deleteTask(taskId: string): Promise<void> {
  // 新建草稿磁盘上还没有它：这个动作是"放弃"而不是"删除"，照旧走后端只会 404
  const draft = editingTask.value;
  const discard = !!draft && draft._isNew && draft.id === taskId;
  const ok = await confirm({
    title: discard ? "放弃新建任务" : "删除任务",
    message: discard
      ? "这个任务还没有保存过（改动后才会创建文件），放弃后当前内容会丢掉。"
      : // 绑定关系会被悄悄改掉，删之前得说清楚：浏览器任务被删后，绑它的方案会
        // 回退到内置 default 任务（见 src/login/mod.rs 的 resolve_task_active，
        // 回退只记一条 warn 日志，用户在界面上看不到任何提示）
        "确定要删除这个任务吗？删除后无法恢复。若有方案绑定它，那些方案会回退到内置的 default 任务。",
    danger: true,
  });
  if (!ok) return;
  if (discard) {
    clearTaskDraft();
    return;
  }
  try {
    // 被删的是当前打开的任务时先关编辑器（自动保存可能正要写回一个已删除的 id）
    if (editingTask.value?.id === taskId) clearTaskDraft();
    await tasksApi.delete(taskId);
    frontendLogger.info("tasks", "任务删除成功: " + taskId);
    toastOnly(true, "任务已删除");
    await fetchTasks(true);
  } catch (error) {
    frontendLogger.error("tasks", "删除任务异常: " + taskId, error);
    toastOnly(false, "删除任务失败");
  }
}

export function useTasks() {
  return {
    tasks,
    editingTask,
    isNewDraft,
    jsonError,
    dangerousSteps,
    autosaveState: autosave.autosaveState,
    duplicatingIds,
    exportingIds,
    fetchTasks,
    deleteTask,
    showTaskEditor,
    createTask,
    closeTaskEditor,
    clearTaskDraft,
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
