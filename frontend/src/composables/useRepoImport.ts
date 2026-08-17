/**
 * 云端仓库任务导入（单例）。
 * 从 useTasks 拆出：仓库索引拉取、免责声明与导入到编辑器。
 * 导入确认后需要写入任务编辑器草稿，通过 useTasks() 单例获取（无循环依赖）。
 */

import { ref, computed } from "vue";
import type { RepoTask } from "../api/types";
import { repoApi } from "../api";
import { extractApiError } from "../api/client";
import { frontendLogger } from "../utils/logger";
import { useToast } from "./useToast";
import { useTasks } from "./useTasks";

const repoImport = ref({
  visible: false,
  url: "https://raw.githubusercontent.com/Misyra/campus-auth-tasks/master/index.json",
  source: "github" as "github" | "gitee" | "custom",
  loading: false,
  error: "",
  tasks: [] as RepoTask[],
  searchQuery: "",
  disclaimer: null as RepoTask | null,
});

const filteredRepoTasks = computed(() => {
  const q = repoImport.value.searchQuery.trim().toLowerCase();
  if (!q) return repoImport.value.tasks;
  return repoImport.value.tasks.filter((t) => {
    const searchable = [t.name, t.description, t.author, ...(t.tags || [])].filter(Boolean).join(" ").toLowerCase();
    return searchable.includes(q);
  });
});

const { toastOnly } = useToast();

function selectRepoSource(source: "github" | "gitee" | "custom") {
  repoImport.value.source = source;
  if (source === "github") {
    repoImport.value.url = "https://raw.githubusercontent.com/Misyra/campus-auth-tasks/master/index.json";
  } else if (source === "gitee") {
    repoImport.value.url = "https://raw.giteeusercontent.com/Misyra/campus-auth-tasks/raw/master/index.gitee.json";
  }
}

function showRepoImport() {
  repoImport.value.visible = true;
  repoImport.value.error = "";
  repoImport.value.tasks = [];
  repoImport.value.searchQuery = "";
  repoImport.value.loading = false;
  repoImport.value.disclaimer = null;
}

function closeRepoImport() {
  repoImport.value.visible = false;
}

async function fetchRepoIndex() {
  const url = repoImport.value.url.trim();
  if (!url) {
    repoImport.value.error = "请输入索引地址";
    return;
  }
  repoImport.value.loading = true;
  repoImport.value.error = "";
  repoImport.value.tasks = [];
  repoImport.value.searchQuery = "";
  try {
    const data = await repoApi.fetchIndex(url);
    if (!Array.isArray(data) || data.length === 0) {
      repoImport.value.error = "索引为空或格式不正确";
      return;
    }
    repoImport.value.tasks = data;
  } catch (e) {
    const msg = extractApiError(e, "加载失败，请检查地址是否正确");
    repoImport.value.error = msg;
    toastOnly(false, `获取远程索引失败: ${msg}`);
  } finally {
    repoImport.value.loading = false;
  }
}

function confirmRepoImport(task: RepoTask) {
  repoImport.value.disclaimer = task;
}

function cancelRepoDisclaimer() {
  repoImport.value.disclaimer = null;
}

async function acceptRepoDisclaimer() {
  const task = repoImport.value.disclaimer;
  repoImport.value.disclaimer = null;
  if (!task) return;

  try {
    const data = await repoApi.fetchTask(task.url);
    let id = (task.id || (data.name as string) || "imported").replace(/[^A-Za-z0-9_]/g, "_");
    if (/^[0-9]/.test(id)) {
      id = "task_" + id;
    }
    const tasks = useTasks();
    tasks.editingTask.value = {
      id,
      name: (data.name as string) || task.name || "",
      description: (data.description as string) || task.description || "",
      url: (data.url as string) || "",
      json: JSON.stringify(data, null, 2),
      _isNew: true,
    };
    tasks.editingTaskType.value = "browser";
    tasks.jsonError.value = "";
    closeRepoImport();
    frontendLogger.info("tasks", `已从仓库导入: ${task.name}`);
    toastOnly(true, `已导入「${task.name}」，请在右侧编辑器内确认后保存`);
  } catch (e) {
    const msg = extractApiError(e, "下载任务失败");
    frontendLogger.error("tasks", "远程任务下载失败", msg);
    toastOnly(false, `远程任务下载失败: ${msg}`);
  }
}

export function useRepoImport() {
  return {
    repoImport,
    filteredRepoTasks,
    selectRepoSource,
    showRepoImport,
    closeRepoImport,
    fetchRepoIndex,
    confirmRepoImport,
    cancelRepoDisclaimer,
    acceptRepoDisclaimer,
  };
}
