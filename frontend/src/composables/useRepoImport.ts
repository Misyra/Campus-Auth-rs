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

/** 切换仓库源并回填对应预设索引地址（自定义源保留用户手输的 URL） */
function selectRepoSource(source: "github" | "gitee" | "custom") {
  repoImport.value.source = source;
  if (source === "github") {
    repoImport.value.url = "https://raw.githubusercontent.com/Misyra/campus-auth-tasks/master/index.json";
  } else if (source === "gitee") {
    repoImport.value.url = "https://raw.giteeusercontent.com/Misyra/campus-auth-tasks/raw/master/index.gitee.json";
  }
}

/** 打开导入弹窗并复位上次残留的搜索词/列表/错误，避免旧内容闪现 */
function showRepoImport() {
  repoImport.value.visible = true;
  repoImport.value.error = "";
  repoImport.value.tasks = [];
  repoImport.value.searchQuery = "";
  repoImport.value.loading = false;
  repoImport.value.disclaimer = null;
}

/** 关闭导入弹窗（不清理状态，下次打开时由 showRepoImport 统一复位） */
function closeRepoImport() {
  repoImport.value.visible = false;
}

/** 按当前输入的索引地址拉取远程任务列表；结果非数组或为空视为失败而非清空展示 */
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

/** 确认导入某任务：仅记录待确认项并展示免责声明，实际导入由 acceptRepoDisclaimer 完成 */
function confirmRepoImport(task: RepoTask) {
  repoImport.value.disclaimer = task;
}

/** 取消免责声明，回到任务列表继续浏览 */
function cancelRepoDisclaimer() {
  repoImport.value.disclaimer = null;
}

/** 接受免责声明并执行导入：下载任务 JSON → dirty 确认 → 写入编辑器草稿（保存仍由用户手动触发） */
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
    // 编辑器草稿保护：与其他打开/替换草稿的路径一致，先经 dirty 确认，
    // 否则仓库导入会静默覆盖未保存的修改
    if (!(await tasks.confirmDiscardTaskIfDirty())) {
      // 用户放弃丢弃草稿：恢复免责声明，让弹窗停留在确认页而非静默关闭
      repoImport.value.disclaimer = task;
      return;
    }
    tasks.setTaskDraft({
      id,
      name: (data.name as string) || task.name || "",
      description: (data.description as string) || task.description || "",
      url: (data.url as string) || "",
      json: JSON.stringify(data, null, 2),
      _isNew: true,
    });
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
