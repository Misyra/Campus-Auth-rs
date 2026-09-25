/**
 * 自定义脚本状态与操作（单例）——自动保存模式（方案 G）。
 *
 * 编辑模型与 `useTasks` / `useHttpTasks` 对齐：**没有草稿态与显式保存**。字段变更
 * debounce 静默 PUT，头部状态字 idle→saving→saved/error；缺口（见 `utils/scriptDraft`）
 * 会拦住落盘并把状态字改成「有 N 处待补全，改动暂未保存」。
 *
 * 与另两个面板的唯一差异在**新建**：直连任务的 ID 没有外部含义，新建即落盘一个
 * `untitled_N` 种子；脚本的 ID 是文件名、也是「定时任务」引用它的值，得由用户命名，
 * 故新建先给一份空 ID 草稿、ID 合法后第一次自动保存才创建文件（`_isNew` 期间 ID 可改，
 * 落盘后固定）。
 *
 * 列表数据由 useTaskDirectory 单次拉取提供（任务/脚本共用一个混合列表源）。
 * 依赖方向：本模块与 useTasks 无依赖关系；脚本可作为方案「自定义脚本」渠道的登录脚本，
 * 但绑定关系存在方案侧（`ProfileData.active_script_task`，在方案编辑器里选），
 * 故脚本面板本身没有「设为活动任务」入口。
 */

import { computed, nextTick, ref, watch } from "vue";
import type { BinaryInfo, TaskExecuteResult } from "../api/types";
import { scriptsApi } from "../api";
import { extractApiError } from "../api/client";
import { frontendLogger } from "../utils/logger";
import { downloadBlob, pickFile, getBinaryName } from "../utils/file";
import { LOGIN_SCRIPT_TEMPLATE, NEW_SCRIPT_STUB } from "../utils/scriptTemplates";
import { useBusyIds } from "../utils/guards";
import { createAutosaveController, gapBlocker } from "../utils/autosave";
import {
  emptyScriptDraft,
  scriptDraftFromServer,
  scriptDraftGaps,
  scriptDraftPayload,
  type ScriptDraft,
  type ScriptGapContext,
} from "../utils/scriptDraft";
import { useTaskDirectory } from "./useTaskDirectory";
import { initialReconcileState, reconcileDraft } from "../utils/draftReconcile";
import { useToast } from "./useToast";
import { useConfirm } from "./useConfirm";

export type { ScriptDraft };

// 列表来自任务目录（与任务共用单次拉取，含 5 秒守卫与首败通知）
const { scripts, fetchDirectory } = useTaskDirectory();
const availableBinaries = ref<BinaryInfo[]>([]);
const editingTask = ref<ScriptDraft | null>(null);

/**
 * 新建脚本的 ID 是否已确认（见 `ScriptGapContext.idPending`）。
 *
 * 脚本的 ID 是用户打的文件名、落盘后不可改，而闸口只能判"当前值合法"——打字停顿
 * （debounce 到点）就会把 `camp` 落盘并锁住 ID。故新建态一律先按"还在输入中"处理，
 * 由面板在回车 / 失焦时调 `commitScriptId()` 收口。
 */
const scriptIdPending = ref(false);

/** 缺口上下文（渲染与闸口共用同一份，避免状态字与实际拦下的条件分叉） */
function gapContext(): ScriptGapContext {
  return { idPending: scriptIdPending.value };
}

// A11：运行 / 导出 busy 守卫（响应式 Set），防止连点重复提交（导出连点会落两份同名文件）
const runningIds = useBusyIds();
const exportingIds = useBusyIds();

/**
 * 最近一次「立即运行」的结果，带它属于哪个脚本。
 *
 * 脚本的 stdout/stderr 只在这一次响应里（不进日志页），不留下来用户就没有任何地方
 * 能看"脚本为什么失败"。带上 id 是为了换脚本后不把上一次的结果显示在新脚本名下。
 */
const lastRunResult = ref<{ id: string; result: TaskExecuteResult } | null>(null);

const { toastOnly } = useToast();
const { confirm } = useConfirm();

// 拉取统一委托任务目录（force 语义与其他 fetch 一致）
function fetchScripts(force = false): Promise<void> {
  return fetchDirectory(force);
}

/**
 * 列表刷新后对账（四个任务面板同一口径，见 `utils/draftReconcile`）。
 * 正在编辑的脚本被别处删掉时，编辑器必须自己退出——否则每改一处都 PUT 一个不存在的
 * id、吃 404 与失败提示，草稿却留在页面上。
 * 走 `clearScriptDraft` 而不是 `closeScriptEditor`：后者"退出即落盘"，对已删除的脚本
 * 再发一次 PUT 只会再吃一次 404。
 */
let reconcileState = initialReconcileState(scripts.value.map((s) => s.id));
watch(
  () => scripts.value.map((s) => s.id).join("\u0000"),
  () => {
    const listIds = scripts.value.map((s) => s.id);
    const result = reconcileDraft({
      state: reconcileState,
      draftId: editingTask.value?.id,
      listIds,
    });
    reconcileState = result.state;
    if (!result.gone) return;
    clearScriptDraft();
    toastOnly(false, "正在编辑的脚本已不存在，已退出编辑");
  },
);

async function fetchAvailableBinaries(): Promise<void> {
  try {
    const data = await scriptsApi.binaries();
    if (Array.isArray(data)) {
      availableBinaries.value.splice(0, availableBinaries.value.length, ...data);
    }
  } catch (error) {
    frontendLogger.error("scripts", "获取可用二进制列表失败", error);
  }
}

/** 当前草稿的缺口（渲染用）：自动保存被缺口拦住时状态字据此改口 */
const draftGapsNow = computed<string[]>(() =>
  editingTask.value ? scriptDraftGaps(editingTask.value, gapContext()) : [],
);

// ---- 自动保存 ----

/** 落盘载荷指纹（比对用；载荷不含 `_isNew` 这类界面态，故可作磁盘状态代表） */
function fingerprint(draft: ScriptDraft): string {
  return JSON.stringify(scriptDraftPayload(draft));
}

/**
 * 自动保存：状态机、在途请求序号与 debounce 都交给共享控制器（见 `utils/autosave`）。
 *
 * 指纹判据在这里比另外两个面板更要紧：new 草稿的 ID 是空的、由用户输入，按"草稿 id 变了"
 * 判"换了编辑对象"会把**改名当切对象**，于是首次落盘永远不会被排期。
 */
const autosave = createAutosaveController<ScriptDraft>({
  draft: editingTask,
  idOf: (draft) => draft.id,
  fingerprintOf: fingerprint,
  blockReasonOf: gapBlocker((draft) => scriptDraftGaps(draft, gapContext())),
  persist: async (draft) => {
    await scriptsApi.save(draft.id.trim(), scriptDraftPayload(draft));
    await fetchScripts(true);
  },
  onSaved: (draft) => {
    // 首次落盘后 ID 固定：文件名是定时任务的引用值，改 ID 等于换一个脚本
    draft._isNew = false;
    // 落盘之后就没有"还在输入"这回事了（否则缺口条会一直挂着那句话）
    scriptIdPending.value = false;
  },
  toast: toastOnly,
  logScope: "scripts",
  detachedSubject: "上一份脚本",
});

/** 关闭编辑器：在途 debounce 立即落盘（「退出即生效」承诺） */
async function closeScriptEditor(): Promise<void> {
  await autosave.flush("close");
  clearScriptDraft();
}

/** 清空编辑器状态（删除脚本等无需再保存的场景）；草稿由控制器一并关掉 */
function clearScriptDraft(): void {
  scriptIdPending.value = false;
  autosave.clear();
}

/**
 * 新建脚本草稿（**不落盘**）。
 *
 * ID 留空由用户命名：脚本 ID 既是文件名、也是定时任务的引用值，取一个 `untitled_N`
 * 会在用户手里留下一个必须再改名的半成品。缺口提示会明说"还缺脚本 ID"，补上后
 * 第一次自动保存创建文件。
 *
 * **命名必须由用户收口**（`scriptIdPending`）：ID 一合法就落盘的话，打字停顿会把
 * `camp` 锁成文件名，`campus` 再也打不完。故这里先置"输入中"，由面板的回车 / 失焦
 * 调 `commitScriptId()`。
 */
function createScriptDraft(): void {
  const draft = emptyScriptDraft(NEW_SCRIPT_STUB);
  editingTask.value = draft;
  scriptIdPending.value = true;
  // 空 ID 本来就是缺口（发不出去），登记基线只是省掉一发注定被拦的定时器
  autosave.markBaseline(draft);
}

/**
 * 确认脚本 ID（面板在输入框回车 / 失焦时调用）：解除"输入中"闸口，并顺手把名字打完后
 * 的第一笔落盘补上（否则要等用户再动一个字段才创建，看着像"没保存"）。
 *
 * 延后一拍执行是有意的：失焦与"点另一个按钮"在同一轮事件里发生，用户点的是「放弃」时
 * 草稿已被清空 —— 这里就什么都不做，否则会先创建一份再把它删掉（还会多弹一次确认）。
 */
function commitScriptId(): void {
  void nextTick(() => {
    const draft = editingTask.value;
    if (!draft || !draft._isNew || !scriptIdPending.value) return;
    scriptIdPending.value = false;
    if (scriptDraftGaps(draft).length === 0) void autosave.saveNow(draft);
  });
}

/** 打开脚本编辑器：无参 = 新建草稿，带参 = 载入已保存脚本 */
async function showScriptEditor(taskId?: string): Promise<void> {
  // 换编辑对象：上一份草稿在途的改动先补发（不 await——打开必须立刻发生；那一发
  // 以 detached 方式落盘，不会回头改这份新草稿的基线）
  void autosave.flush("switch");
  if (!taskId) {
    createScriptDraft();
    return;
  }
  if (!availableBinaries.value.length) await fetchAvailableBinaries();
  try {
    const data = await scriptsApi.get(taskId);
    const draft = scriptDraftFromServer(data, availableBinaries.value);
    editingTask.value = draft;
    // 已存在的脚本：ID 早已定下，不存在"还在输入中"
    scriptIdPending.value = false;
    // 刚载入的草稿就是磁盘现状：基线对上了，用户不动它就不会发请求
    autosave.markBaseline(draft);
  } catch (error) {
    frontendLogger.error("scripts", "加载脚本失败: " + taskId, error);
    toastOnly(false, extractApiError(error, "加载脚本失败"));
  }
}

function onBinarySelectChange(): void {
  if (!editingTask.value) return;
  if (editingTask.value.binary_path === "__custom__") {
    editingTask.value._customBinary = editingTask.value._customBinary || "";
  } else {
    editingTask.value._customBinary = "";
  }
}

/** 当前编辑的是否为"还没落盘的新建草稿"（面板据此禁运行、改「删除」为「放弃」） */
const isNewDraft = computed(() => editingTask.value?._isNew === true);

/**
 * 该 id 是否就是"当前这份还没落盘的新建草稿"。
 *
 * 面板把「当前编辑的对象 id」传下来，而新建脚本的 id 可能还是空串（用户还没命名），
 * 故同时按"草稿本身是新建态"判定：任何针对新建草稿的删除/导出都不该打到后端去。
 */
function isCurrentNewDraft(editingId: string): boolean {
  const draft = editingTask.value;
  if (!draft || draft._isNew !== true) return false;
  return !editingId || editingId === draft.id;
}

async function deleteScript(taskId: string): Promise<void> {
  // 新建草稿磁盘上还没有它：这个动作是"放弃"而不是"删除"，照旧走后端只会 404
  const discard = isCurrentNewDraft(taskId);
  const ok = await confirm({
    title: discard ? "放弃新建脚本" : "删除脚本",
    message: discard
      ? "这个脚本还没有保存过（补上脚本 ID 后才会创建文件），放弃后当前内容会丢掉。"
      : `确定删除脚本「${taskId}」吗？`,
    danger: true,
  });
  if (!ok) return;
  if (discard) {
    clearScriptDraft();
    return;
  }
  try {
    // 被删的是当前打开的那条时先关编辑器：自动保存可能正要写回一个已删除的 id
    if (editingTask.value && !editingTask.value._isNew && editingTask.value.id === taskId) {
      clearScriptDraft();
    }
    const data = await scriptsApi.delete(taskId);
    await fetchScripts(true);
    toastOnly(true, data?.message || "删除成功");
  } catch (error) {
    toastOnly(false, extractApiError(error, "删除失败"));
  }
}

async function runScript(taskId: string): Promise<void> {
  // A11：busy 守卫，运行中连点直接忽略，避免重复提交
  if (runningIds.has(taskId)) return;
  runningIds.add(taskId);
  try {
    const data = await scriptsApi.run(taskId);
    // 脚本输出**不进日志页**（进程 stdout/stderr 只作为执行结果回包，见
    // python_worker/README 与 src/tasks/executor.rs），面板不就地展示的话用户
    // 根本看不到脚本为什么失败——故连结果一起留下。
    lastRunResult.value = data ? { id: taskId, result: data } : null;
    // 执行失败同样是 HTTP 200（后端脚本执行把成败放在业务字段里），只看 HTTP
    // 会把它弹成绿色的"执行完成"——按业务字段 success 分流。
    if (data?.success) {
      toastOnly(true, `脚本执行成功（${data.duration_ms}ms）`);
      return;
    }
    const detail = firstNonEmptyLine(data?.error || data?.output || "");
    frontendLogger.warn("scripts", `脚本执行失败: ${taskId} ${data?.error || ""}`);
    toastOnly(
      false,
      detail
        ? `脚本执行失败（退出码 ${data?.exit_code ?? "?"}）：${truncate(detail, 120)}`
        : "脚本执行失败",
    );
  } catch (error) {
    lastRunResult.value = null;
    toastOnly(false, extractApiError(error, "执行失败"));
  } finally {
    runningIds.delete(taskId);
  }
}

/** 取最后一行非空文本：报错信息在 stdout/stderr 拼接串的末尾 */
function firstNonEmptyLine(text: string): string {
  const lines = text.split("\n").map((l) => l.trim()).filter(Boolean);
  return lines.length ? lines[lines.length - 1] : "";
}

function truncate(text: string, max: number): string {
  return text.length > max ? text.slice(0, max) + "…" : text;
}

async function exportScript(taskId: string): Promise<void> {
  // A11：busy 守卫，与浏览器任务 / 直连任务的导出同口径
  if (exportingIds.has(taskId)) return;
  exportingIds.add(taskId);
  try {
    // 新建草稿还没落盘：后端没有这个 id，按 id 取必 404 → 直接导出草稿内容
    if (isCurrentNewDraft(taskId)) {
      const draft = editingTask.value!;
      downloadBlob(
        draft.content || "",
        `${draft.id || "untitled"}${inferScriptExtension(draft.binary_path, draft.content)}`,
        "text/plain",
      );
      return;
    }
    const data = await scriptsApi.get(taskId);
    const ext = inferScriptExtension(data.binary_path, data.content);
    downloadBlob(data.content || "", `${taskId}${ext}`, "text/plain");
  } catch (error) {
    toastOnly(false, extractApiError(error, "导出失败"));
  } finally {
    exportingIds.delete(taskId);
  }
}

/**
 * 导入脚本文件。
 *
 * 与旧流程（读进草稿 → 用户点保存）的差别：内容直接进编辑器交给自动保存落盘。
 * 已存在的 ID 按**覆盖**语义（先确认），新 ID 首次落盘即创建；登记「磁盘上还没有它」
 * （`markUnsaved`）是必需的——覆盖时列表摘要里拿不到盘上的正文，沿用旧基线会让自动保存
 * 以为"和磁盘一样"从而什么都不写。
 *
 * @returns 打开的脚本 ID；用户取消或文件不可用时为空串（面板据此决定要不要写 `?task=`）
 */
async function importScript(): Promise<string> {
  // 不收 .exe：按文本读入只会得到乱码（exe 应直接填 binary_path，不走导入）
  const file = await pickFile(".py,.sh,.bat,.cmd,.txt");
  if (!file) return "";
  const content = await file.text();
  // ID 取文件名 stem，清洗规则与 SCRIPT_ID_PATTERN（= 后端 is_valid_task_id）一致：
  // 连字符是合法字符，不该被下划线顶掉
  let id = file.name.replace(/\.[^.]+$/, "").replace(/[^A-Za-z0-9_-]/g, "_");
  if (/^[0-9]/.test(id)) id = "sc_" + id;
  if (!/[A-Za-z]/.test(id)) {
    toastOnly(false, `文件名「${file.name}」无法作为脚本 ID，请先重命名文件`);
    return "";
  }
  const exists = scripts.value.some((s) => s.id === id);
  if (exists) {
    const ok = await confirm({
      title: "脚本已存在",
      message: `脚本「${id}」已存在，导入的内容会覆盖它。是否继续？`,
      danger: true,
    });
    if (!ok) return "";
  }
  void autosave.flush("switch");
  editingTask.value = {
    id,
    name: "",
    description: "",
    content,
    binary_path: "",
    _customBinary: "",
    _isNew: !exists,
  };
  // 导入的内容**与磁盘不同**（覆盖时列表摘要里也拿不到盘上正文）：登记"磁盘上还没有"，
  // 留一个旧基线会让自动保存以为"和磁盘一样"从而什么都不写
  autosave.markUnsaved();
  // ID 来自文件名，用户挑文件那一刻就定下了：不存在"还在输入中"
  scriptIdPending.value = false;
  frontendLogger.info("scripts", `已导入脚本内容，待自动保存: ${id}`);
  // 返回打开的 id：面板据此把 `?task=` 写上（覆盖既有脚本时 `_isNew` 为 false，
  // 面板那个"落盘后补 query"的 watcher 不会触发，刷新就掉回列表态）
  return id;
}

function loadScriptTemplate(): void {
  if (!editingTask.value) return;
  // 覆盖已有内容前仍要确认：自动保存会把模板直接写进磁盘（旧实现是覆盖草稿，
  // 现在多一层后果，确认文案相应说明"会直接保存"）
  void (async () => {
    const current = editingTask.value?.content ?? "";
    if (current.trim() && current.trim() !== LOGIN_SCRIPT_TEMPLATE.trim()) {
      const ok = await confirm({
        title: "加载示例模板",
        message: "当前脚本内容会被示例模板覆盖，并立即保存到磁盘。是否继续？",
        danger: true,
      });
      if (!ok) return;
    }
    if (!editingTask.value) return;
    editingTask.value.content = LOGIN_SCRIPT_TEMPLATE;
  })();
}

function inferScriptExtension(binaryPath?: string, content?: string): string {
  if (binaryPath) {
    const base = binaryPath.split(/[/\\]/).pop()?.toLowerCase() || "";
    if (base.startsWith("python") || base === "py" || (base.endsWith(".exe") && base.includes("python"))) return ".py";
    if (base === "bash" || base === "sh" || base === "zsh") return ".sh";
    if (base === "cmd" || base === "cmd.exe" || base === "bat" || base.endsWith(".bat")) return ".bat";
    if (base.endsWith(".exe")) return ".exe";
  }
  if (content) {
    const firstLine = content.split("\n")[0];
    if (firstLine.includes("python")) return ".py";
    if (firstLine.includes("bash") || firstLine.includes("sh")) return ".sh";
  }
  return ".py";
}

export function useScripts() {
  return {
    scripts,
    availableBinaries,
    editingTask,
    isNewDraft,
    runningIds,
    exportingIds,
    lastRunResult,
    autosaveState: autosave.autosaveState,
    draftGapsNow,
    scriptIdPending,
    commitScriptId,
    getBinaryName,
    fetchScripts,
    fetchAvailableBinaries,
    showScriptEditor,
    createScriptDraft,
    closeScriptEditor,
    clearScriptDraft,
    onBinarySelectChange,
    deleteScript,
    runScript,
    exportScript,
    importScript,
    loadScriptTemplate,
  };
}
