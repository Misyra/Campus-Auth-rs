/**
 * 自定义脚本状态与操作（单例）。
 * 替代原 scriptData + scriptMethods。
 */

import { ref } from "vue";
import type { Script, BinaryInfo } from "../api/types";
import { scriptsApi, tasksApi } from "../api";
import { extractApiError } from "../api/client";
import { frontendLogger } from "../utils/logger";
import { downloadBlob, pickFile, getBinaryName } from "../utils/file";
import { useToast } from "./useToast";
import { useConfirm } from "./useConfirm";

export interface ScriptDraft {
  id: string;
  name: string;
  description: string;
  content: string;
  binary_path: string;
  _customBinary: string;
  _customPythonBinary: string;
  _isNew: boolean;
}

const scripts = ref<Script[]>([]);
const availableBinaries = ref<BinaryInfo[]>([]);
const editingTask = ref<ScriptDraft | null>(null);

const { toastOnly } = useToast();
const { confirm } = useConfirm();

async function fetchScripts(): Promise<void> {
  try {
    const data = await scriptsApi.list();
    if (Array.isArray(data)) {
      scripts.value.splice(0, scripts.value.length, ...data);
    }
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
      const realBinaries = availableBinaries.value.filter((b) => b.path !== "__custom_python__");
      const isKnownBinary = binaryPath && realBinaries.some((b) => b.path === binaryPath);
      let selectValue = binaryPath;
      let customBinary = "";
      let customPythonBinary = "";
      if (!isKnownBinary && binaryPath) {
        if (binaryPath.toLowerCase().includes("python")) {
          selectValue = "__custom_python__";
          customPythonBinary = binaryPath;
        } else {
          selectValue = "__custom__";
          customBinary = binaryPath;
        }
      }
      editingTask.value = {
        id: taskId,
        name: data.name || "",
        description: data.description || "",
        content: data.content || "",
        binary_path: selectValue,
        _customBinary: customBinary,
        _customPythonBinary: customPythonBinary,
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
      _customPythonBinary: "",
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
  if (editingTask.value.binary_path === "__custom_python__") {
    editingTask.value._customPythonBinary = editingTask.value._customPythonBinary || "";
  } else {
    editingTask.value._customPythonBinary = "";
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
  else if (binaryPath === "__custom_python__") binaryPath = editingTask.value._customPythonBinary || "";

  const payload = {
    name: editingTask.value.name || id,
    description: editingTask.value.description || "",
    content: editingTask.value.content,
    binary_path: binaryPath,
  };
  try {
    const data = await scriptsApi.save(id, payload);
    editingTask.value = null;
    await fetchScripts();
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
    await fetchScripts();
    toastOnly(true, data?.message || "删除成功");
  } catch (error) {
    toastOnly(false, extractApiError(error, "删除失败"));
  }
}

async function runScript(taskId: string): Promise<void> {
  try {
    const data = await scriptsApi.run(taskId);
    toastOnly(true, data?.message || "执行完成");
  } catch (error) {
    toastOnly(false, extractApiError(error, "执行失败"));
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
  const file = await pickFile(".py,.sh,.bat,.ps1,.txt");
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
    _customPythonBinary: "",
    _isNew: true,
  };
  frontendLogger.info("scripts", "已导入脚本文件，请检查后保存");
}

async function setActiveScript(taskId: string): Promise<void> {
  try {
    await tasksApi.setActive(taskId);
    const { useTasks } = await import("./useTasks");
    // 服务端已切换，此处仅同步本地状态（用封装 setter，不直接拨弄其他 composable 的 ref）
    useTasks().setActiveTaskId(taskId);
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
    if (base === "cmd" || base === "cmd.exe" || base === "bat") return ".bat";
    if (base === "powershell" || base === "pwsh") return ".ps1";
  }
  if (content) {
    const firstLine = content.split("\n")[0];
    if (firstLine.includes("python")) return ".py";
    if (firstLine.includes("bash") || firstLine.includes("sh")) return ".sh";
    if (firstLine.includes("powershell") || firstLine.includes("pwsh")) return ".ps1";
  }
  return ".py";
}

export function useScripts() {
  return {
    scripts,
    availableBinaries,
    editingTask,
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
