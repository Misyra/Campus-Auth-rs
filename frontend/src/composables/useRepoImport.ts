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
import { TASK_REPO_INDEX_URL, TASK_REPO_SOURCES, type TaskRepoSourceId } from "../utils/constants";
import { useToast } from "./useToast";
import { useTasks } from "./useTasks";

const repoImport = ref({
  visible: false,
  url: TASK_REPO_INDEX_URL,
  source: "github" as TaskRepoSourceId,
  /** 用户手输的自定义索引地址：切走再切回时恢复，避免误点一下就把已填内容冲掉 */
  customUrl: "",
  loading: false,
  error: "",
  tasks: [] as RepoTask[],
  searchQuery: "",
  disclaimer: null as RepoTask | null,
  /** 列表点选的任务（右侧详情预览用；导入仍经 disclaimer 二次确认） */
  selected: null as RepoTask | null,
});

const filteredRepoTasks = computed(() => {
  const q = repoImport.value.searchQuery.trim().toLowerCase();
  if (!q) return repoImport.value.tasks;
  return repoImport.value.tasks.filter((t) => {
    const searchable = [t.name, t.description, t.author, ...(t.tags || [])].filter(Boolean).join(" ").toLowerCase();
    return searchable.includes(q);
  });
});

/** 当前源的选项（含说明文案）；未知源回退到自定义，避免取到 undefined */
const currentSource = computed(
  () => TASK_REPO_SOURCES.find((s) => s.id === repoImport.value.source) ?? TASK_REPO_SOURCES[TASK_REPO_SOURCES.length - 1],
);

/** 当前源的「直接查看仓库」地址：预设源用仓库主页，自定义源回退成用户自填的地址 */
const sourceHomeUrl = computed(() => currentSource.value.homeUrl || repoImport.value.url.trim());

const { toastOnly } = useToast();

// 索引拉取序号（epoch）守卫：只有最新一次请求可以写状态。
// 交错场景——慢索引 A 在途 → 关弹窗重开（loading 被 showRepoImport 复位）→
// 为 URL B 再点一次 → A 迟到覆盖 B 的列表并提前清 loading，用户会把 A 源的
// 任务当 B 源导入。与 useConfig 的 saveSeq / fetchConfigEpoch 同口径。
let fetchIndexSeq = 0;

/** 切换仓库源并回填对应预设索引地址（自定义源恢复上次手输的 URL） */
function selectRepoSource(source: TaskRepoSourceId) {
  // 离开自定义源前先记住手输内容：否则误点一下 GitHub 再点回来，已填的地址就没了
  if (repoImport.value.source === "custom" && source !== "custom") {
    repoImport.value.customUrl = repoImport.value.url;
  }
  repoImport.value.source = source;
  // 预设地址取自 TASK_REPO_SOURCES，不在此处各写一份：
  // 同一 host 曾在多处硬编码，正是「分享适配」指向错仓库那类缺陷的成因
  const preset = TASK_REPO_SOURCES.find((s) => s.id === source);
  if (preset?.indexUrl) {
    repoImport.value.url = preset.indexUrl;
  } else if (repoImport.value.customUrl) {
    // 仅在确实存过手输地址时回填：否则会把输入框清空，反而比保留上一个源的地址更差
    repoImport.value.url = repoImport.value.customUrl;
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
  repoImport.value.selected = null;
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
  // 取号：迟到的旧响应据此丢弃（见 fetchIndexSeq 声明处注释）
  const seq = ++fetchIndexSeq;
  repoImport.value.loading = true;
  repoImport.value.error = "";
  repoImport.value.tasks = [];
  repoImport.value.searchQuery = "";
  repoImport.value.selected = null;
  try {
    const data = await repoApi.fetchIndex(url);
    if (seq !== fetchIndexSeq) return;
    if (!Array.isArray(data) || data.length === 0) {
      repoImport.value.error = "索引为空或格式不正确";
      return;
    }
    repoImport.value.tasks = data;
  } catch (e) {
    // 被取代的旧请求失败同样不写状态：否则会用一个已过期的错误覆盖新请求的结果
    if (seq !== fetchIndexSeq) return;
    const msg = extractApiError(e, "加载失败，请检查地址是否正确");
    repoImport.value.error = msg;
    toastOnly(false, `获取远程索引失败: ${msg}`);
  } finally {
    // 仅最新请求负责复位 loading：旧请求提前清掉会让界面在 B 仍在途时误示"已完成"
    if (seq === fetchIndexSeq) repoImport.value.loading = false;
  }
}

/** 列表点选任务：右侧详情区展示截图大图与完整信息（不触发导入） */
function selectRepoTask(task: RepoTask) {
  repoImport.value.selected = task;
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
    currentSource,
    sourceHomeUrl,
    selectRepoSource,
    showRepoImport,
    closeRepoImport,
    fetchRepoIndex,
    selectRepoTask,
    confirmRepoImport,
    cancelRepoDisclaimer,
    acceptRepoDisclaimer,
  };
}
