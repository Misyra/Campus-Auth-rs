/**
 * 自定义脚本状态与操作（单例）。
 * 替代原 scriptData + scriptMethods。
 * 列表数据由 useTaskDirectory 单次拉取提供（任务/脚本共用一个混合列表源）。
 *
 * 依赖方向：useScripts → useTasks（单向）。useTasks 不再反向引用本模块，
 * 脚本列表与任务列表通过共享的任务目录（useTaskDirectory）取数，无循环依赖。
 */

import { ref } from "vue";
import type { Script, BinaryInfo } from "../api/types";
import { scriptsApi } from "../api";
import { extractApiError } from "../api/client";
import { frontendLogger } from "../utils/logger";
import { downloadBlob, pickFile, getBinaryName } from "../utils/file";
import { LOGIN_SCRIPT_TEMPLATE, NEW_SCRIPT_STUB } from "../utils/scriptTemplates";
import { useBusyIds } from "../utils/guards";
import { useTaskDirectory } from "./useTaskDirectory";
import { useToast } from "./useToast";
import { useConfirm } from "./useConfirm";
import { useTasks } from "./useTasks";

export interface ScriptDraft {
  id: string;
  name: string;
  description: string;
  content: string;
  binary_path: string;
  _customBinary: string;
  _isNew: boolean;
}

// 列表来自任务目录（与任务共用单次拉取，含 5 秒守卫与首败通知）
const { scripts, fetchDirectory } = useTaskDirectory();
const availableBinaries = ref<BinaryInfo[]>([]);
const editingTask = ref<ScriptDraft | null>(null);

// A11：运行 busy 守卫（响应式 Set），防止连点重复提交
const runningIds = useBusyIds();

const { toastOnly } = useToast();
const { confirm } = useConfirm();

// 拉取统一委托任务目录（force 语义与其他 fetch 一致）
function fetchScripts(force = false): Promise<void> {
  return fetchDirectory(force);
}

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

// ---- 编辑器 dirty 快照（对齐 useProfiles 的快照模式）----
let editingScriptSnapshot = "";

function snapshotOf(draft: ScriptDraft | null): string {
  return draft ? JSON.stringify(draft) : "";
}

/** 统一的草稿写入入口：赋值并同步刷新 dirty 基准快照。 */
function setScriptDraft(draft: ScriptDraft): void {
  editingTask.value = draft;
  editingScriptSnapshot = snapshotOf(draft);
}

/** 当前编辑器是否有未保存改动。 */
function isScriptDirty(): boolean {
  return editingTask.value !== null && snapshotOf(editingTask.value) !== editingScriptSnapshot;
}

/**
 * 若存在未保存改动，弹窗确认是否放弃；无改动则直接放行。
 * 返回 true 才允许继续（放弃修改）；false（用户取消）与 null（被新对话框抢占）
 * 一律不放行——保留现状、不丢弃数据（A10 语义：被抢占≠用户放弃）。
 */
async function confirmDiscardScriptIfDirty(): Promise<boolean | null> {
  if (!isScriptDirty()) return true;
  return confirm({
    title: "放弃未保存的修改",
    message: "当前脚本有未保存的修改，确定放弃吗？",
    danger: true,
  });
}

/** 关闭脚本编辑器（带 dirty 确认）。 */
async function closeScriptEditor(): Promise<void> {
  if (!(await confirmDiscardScriptIfDirty())) return;
  clearScriptDraft();
}

/** 清空草稿（保存成功等无需确认的场景）。 */
function clearScriptDraft(): void {
  editingTask.value = null;
  editingScriptSnapshot = "";
}

async function showScriptEditor(taskId?: string): Promise<void> {
  // 打开/切换编辑器前先确认当前草稿是否有未保存改动，避免静默丢弃
  if (!(await confirmDiscardScriptIfDirty())) return;
  if (!availableBinaries.value.length) await fetchAvailableBinaries();
  if (taskId) {
    try {
      const data = await scriptsApi.get(taskId);
      const binaryPath = data.binary_path || "";
      const isKnownBinary = binaryPath && availableBinaries.value.some((b) => b.path === binaryPath);
      let selectValue = binaryPath;
      let customBinary = "";
      if (!isKnownBinary && binaryPath) {
        // 未知路径一律视为自定义可执行文件（exe/bat 等）；Python 应使用项目内解释器（binary_path 留空）
        selectValue = "__custom__";
        customBinary = binaryPath;
      }
      setScriptDraft({
        id: taskId,
        name: data.name || "",
        description: data.description || "",
        content: data.content || "",
        binary_path: selectValue,
        _customBinary: customBinary,
        _isNew: false,
      });
    } catch (error) {
      toastOnly(false, extractApiError(error, "加载脚本失败"));
    }
  } else {
    setScriptDraft({
      id: "",
      name: "",
      description: "",
      content: NEW_SCRIPT_STUB,
      binary_path: "",
      _customBinary: "",
      _isNew: true,
    });
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

async function saveScript(): Promise<void> {
  if (!editingTask.value) return;
  const id = editingTask.value.id.trim();
  if (!id) {
    toastOnly(false, "脚本ID不能为空");
    return;
  }
  if (!/^[A-Za-z][A-Za-z0-9_]*$/.test(id)) {
    toastOnly(false, "脚本ID必须以字母开头，且只能包含字母、数字和下划线");
    return;
  }
  if (!editingTask.value.content.trim()) {
    toastOnly(false, "脚本内容不能为空");
    return;
  }
  const maxSize = 100 * 1024;
  if (new TextEncoder().encode(editingTask.value.content).length > maxSize) {
    toastOnly(false, `脚本内容超过大小限制（最大 ${maxSize / 1024}KB）`);
    return;
  }
  let binaryPath = editingTask.value.binary_path;
  if (binaryPath === "__custom__") binaryPath = editingTask.value._customBinary || "";
  // Python 使用项目内解释器：binary_path 留空，由后端使用项目自带 Python
  if (binaryPath) {
    const lower = binaryPath.toLowerCase();
    // 禁止 PowerShell：即使通过自定义路径传入也拒绝
    if (lower.includes("powershell") || lower.includes("pwsh") || lower.endsWith(".ps1")) {
      toastOnly(false, "不支持 PowerShell，仅支持 shell / bat / python / exe 四类脚本");
      return;
    }
  }

  const payload = {
    // 后端 TaskKind 反序列化在 type 缺失时默认归为 browser 任务，
    // 会把脚本负载静默转存为空浏览器任务（脚本内容丢失），必须显式声明
    type: "script" as const,
    name: editingTask.value.name || id,
    description: editingTask.value.description || "",
    content: editingTask.value.content,
    binary_path: binaryPath,
  };
  try {
    const data = await scriptsApi.save(id, payload);
    clearScriptDraft();
    await fetchScripts(true);
    toastOnly(true, data?.message || "保存成功");
  } catch (error) {
    toastOnly(false, extractApiError(error, "保存失败"));
  }
}

async function deleteScript(taskId: string): Promise<void> {
  const ok = await confirm({ title: "删除脚本", message: `确定删除脚本「${taskId}」吗？`, danger: true });
  if (!ok) return;
  try {
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
    toastOnly(true, data?.message || "执行完成");
  } catch (error) {
    toastOnly(false, extractApiError(error, "执行失败"));
  } finally {
    runningIds.delete(taskId);
  }
}

async function exportScript(taskId: string): Promise<void> {
  try {
    const data = await scriptsApi.get(taskId);
    const ext = inferScriptExtension((data as Script).binary_path, (data as Script).content);
    downloadBlob((data as Script).content || "", `${taskId}${ext}`, "text/plain");
  } catch (error) {
    toastOnly(false, extractApiError(error, "导出失败"));
  }
}

async function importScript(): Promise<void> {
  const file = await pickFile(".py,.sh,.bat,.exe,.cmd,.txt");
  if (!file) return;
  // 导入会整体替换当前草稿，先确认未保存改动
  if (!(await confirmDiscardScriptIfDirty())) return;
  const reader = new FileReader();
  reader.onload = (ev) => {
    const content = ev.target?.result as string;
    let id = file.name.replace(/\.[^.]+$/, "").replace(/[^A-Za-z0-9_]/g, "_");
    if (/^[0-9]/.test(id)) id = "sc_" + id;
    if (scripts.value.some((s) => s.id === id)) {
      void confirm({ title: "脚本已存在", message: `脚本「${id}」已存在，是否覆盖？` }).then((ok) => {
        if (!ok) return;
        openImportedDraft(id, content);
      });
      return;
    }
    openImportedDraft(id, content);
  };
  reader.readAsText(file);
}

function openImportedDraft(id: string, content: string): void {
  setScriptDraft({
    id,
    name: "",
    description: "",
    content,
    binary_path: "",
    _customBinary: "",
    _isNew: true,
  });
  frontendLogger.info("scripts", "已导入脚本文件，请检查后保存");
}

async function setActiveScript(taskId: string): Promise<void> {
  // 走 useTasks 的正规 setActiveTask（API + 本地同步），不再直捅 activeTaskId
  const ok = await useTasks().setActiveTask(taskId);
  if (ok) {
    toastOnly(true, `已将「${taskId}」设为活动任务`);
  }
}

function loadScriptTemplate(): void {
  if (!editingTask.value) return;
  editingTask.value.content = LOGIN_SCRIPT_TEMPLATE;
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
    runningIds,
    getBinaryName,
    fetchScripts,
    fetchAvailableBinaries,
    showScriptEditor,
    closeScriptEditor,
    isScriptDirty,
    onBinarySelectChange,
    saveScript,
    deleteScript,
    runScript,
    exportScript,
    importScript,
    setActiveScript,
    loadScriptTemplate,
  };
}
