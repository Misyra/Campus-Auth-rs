/**
 * 自定义脚本状态与操作（单例）。
 * 替代原 scriptData + scriptMethods。
 */

import { ref, reactive } from "vue";
import type { Script, BinaryInfo } from "../api/types";
import { scriptsApi, tasksApi } from "../api";
import { extractApiError } from "../api/client";
import { frontendLogger } from "../utils/logger";
import { downloadBlob, pickFile, getBinaryName } from "../utils/file";
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

const scripts = ref<Script[]>([]);
const availableBinaries = ref<BinaryInfo[]>([]);
const editingTask = ref<ScriptDraft | null>(null);

// A11：运行 busy 守卫（响应式 Set），防止连点重复提交
const runningIds = reactive(new Set<string>());

const { toastOnly } = useToast();
const { confirm } = useConfirm();

// P15：5 秒内已成功拉取则跳过（useUi.init 已拉全部数据，View mount / 路由往返
// 不再重复请求）。lastFetchAt 仅成功后更新（失败不更新以便重试）；
// force: true 供变更后刷新 / 重连回调等显式刷新场景绕过守卫。
let lastFetchAt = 0;

async function fetchScripts(force = false): Promise<void> {
  if (!force && Date.now() - lastFetchAt < 5000) return;
  try {
    const data = await scriptsApi.list();
    if (Array.isArray(data)) {
      // GET /api/tasks 与 /api/scripts 返回同一混合列表，此处仅保留脚本类（script/shell）
      const scriptTasks = data.filter((t) => {
        const tt = (t.task_type as string) || (t.type as string) || "";
        return tt === "script" || tt === "shell";
      });
      scripts.value.splice(0, scripts.value.length, ...scriptTasks);
    }
    lastFetchAt = Date.now();
  } catch (error) {
    frontendLogger.error("scripts", "获取脚本列表失败", error);
  }
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

async function showScriptEditor(taskId?: string): Promise<void> {
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
      editingTask.value = {
        id: taskId,
        name: data.name || "",
        description: data.description || "",
        content: data.content || "",
        binary_path: selectValue,
        _customBinary: customBinary,
        _isNew: false,
      };
    } catch (error) {
      toastOnly(false, extractApiError(error, "加载脚本失败"));
    }
  } else {
    editingTask.value = {
      id: "",
      name: "",
      description: "",
      content: '#!/usr/bin/env python3\n"""自定义登录脚本"""\nimport httpx\n\n',
      binary_path: "",
      _customBinary: "",
      _isNew: true,
    };
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
    editingTask.value = null;
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
  editingTask.value = {
    id,
    name: "",
    description: "",
    content,
    binary_path: "",
    _customBinary: "",
    _isNew: true,
  };
  frontendLogger.info("scripts", "已导入脚本文件，请检查后保存");
}

async function setActiveScript(taskId: string): Promise<void> {
  try {
    await tasksApi.setActive(taskId);
    // 服务端已切换，此处仅同步本地状态
    useTasks().activeTaskId.value = taskId;
    toastOnly(true, `已将「${taskId}」设为活动任务`);
  } catch (error) {
    toastOnly(false, extractApiError(error, "设置失败"));
  }
}

function loadScriptTemplate(): void {
  if (!editingTask.value) return;
  editingTask.value.content = `#!/usr/bin/env python3
"""自定义登录脚本示例

脚本只需发送登录请求，登录是否成功由系统网络检测自动判断。
"""

LOGIN_URL = "http://10.0.0.1/login"
USERNAME = "your_username"
PASSWORD = "your_password"
ISP = "cmcc"

import httpx
resp = httpx.post(LOGIN_URL, data={"username": USERNAME, "password": PASSWORD, "operator": ISP}, timeout=30)
print(f"HTTP {resp.status_code}")
`;
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
