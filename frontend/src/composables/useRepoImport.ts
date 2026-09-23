/**
 * 云端仓库任务导入（单例）。
 * 从 useTasks 拆出：仓库索引拉取、免责声明与导入到编辑器。
 *
 * 索引是**混合**的：同一个仓库同时承载浏览器任务与直连任务（`RepoTask.type`），
 * 故打开弹窗前先由调用方声明要看哪一类（`showRepoImport("browser" | "http")`），
 * 列表只列该类型，确认导入时也按类型写进对应的编辑器草稿——
 * 导入确认后需要写入编辑器草稿，通过 useTasks / useHttpTasks 单例获取（无循环依赖）。
 */

import { ref, computed } from "vue";
import type { HttpTaskConfig, RepoTask } from "../api/types";
import { repoApi } from "../api";
import { extractApiError } from "../api/client";
import { frontendLogger } from "../utils/logger";
import { TASK_REPO_INDEX_URL, TASK_REPO_SOURCES, type TaskRepoSourceId } from "../utils/constants";
import { httpTaskDraftFromConfig } from "../utils/httpTask";
import { useToast } from "./useToast";
import { useTasks } from "./useTasks";
import { useHttpTasks } from "./useHttpTasks";

/** 仓库条目的归属类型（本页只处理这两类） */
export type RepoKind = "browser" | "http";

/**
 * 归一化条目的类型。
 *
 * 缺省/空串视为 `browser`：直连任务加入索引之前发布的老条目没有 `type` 字段，
 * 把缺省当 browser 才能让同一个仓库同时承载两类条目而不破坏既有条目；
 * 其余类型（如 `script`）返回空串，即"两类列表都不进"。
 */
function normalizeRepoTaskKind(type: string | undefined): RepoKind | "" {
  const value = (type ?? "").trim().toLowerCase();
  if (value === "" || value === "browser") return "browser";
  if (value === "http") return "http";
  return "";
}

const repoImport = ref({
  visible: false,
  url: TASK_REPO_INDEX_URL,
  source: "github" as TaskRepoSourceId,
  /** 本次要看哪一类条目：决定列表过滤与导入去向（见 acceptRepoDisclaimer） */
  repoKind: "browser" as RepoKind,
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
  // 先按类型过滤再按关键词：搜索不该把另一类条目"搜"出来（导入会写错编辑器）
  const sameKind = repoImport.value.tasks.filter(
    (t) => normalizeRepoTaskKind(t.type) === repoImport.value.repoKind,
  );
  const q = repoImport.value.searchQuery.trim().toLowerCase();
  if (!q) return sameKind;
  return sameKind.filter((t) => {
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

/**
 * 打开导入弹窗并复位上次残留的搜索词/列表/错误，避免旧内容闪现。
 *
 * `kind` 必须由调用方声明：任务页两个 Tab（浏览器/直连）、设置页入口各自知道
 * 自己要哪一类，默认值会让"忘了传"变成静默导入错类型（列表看着空空如也）。
 */
function showRepoImport(kind: RepoKind) {
  repoImport.value.visible = true;
  repoImport.value.repoKind = kind;
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

/**
 * 接受免责声明并执行导入：下载任务 JSON → dirty 确认 → 写入编辑器草稿（保存仍由用户手动触发）。
 *
 * 两条路径都落在"新建草稿"上而非直接落盘：仓库内容未审核，写盘前必须让用户
 * 在编辑器里过一眼（并能改 ID/名称）。
 */
async function acceptRepoDisclaimer() {
  const task = repoImport.value.disclaimer;
  repoImport.value.disclaimer = null;
  if (!task) return;

  try {
    const data = (await repoApi.fetchTask(task.url)) as Record<string, unknown>;
    // 兼容导出详情形态（{ summary, config }）：字段都在 config 里，取内层再读
    const config = (data.config && typeof data.config === "object" ? data.config : data) as Record<string, unknown>;
    const kind = repoImport.value.repoKind;
    const entryKind = normalizeRepoTaskKind(
      (data.type as string | undefined) ?? (config.type as string | undefined),
    );
    // 防御：列表已按类型过滤，能走到这里说明索引与实际文件不一致
    // （条目声明 browser 而文件是 http 等），此时按声明的类型落草稿会得到一堆空字段
    if (entryKind !== kind) {
      toastOnly(false, kind === "http" ? '该条目不是直连任务（type 需为 "http"）' : "该条目的类型与当前列表不一致，请在对应 Tab 导入");
      return;
    }

    const name = String(data.name ?? config.name ?? task.name ?? "");
    const description = String(data.description ?? config.description ?? task.description ?? "");

    if (kind === "http") {
      // 任务 ID 归一化：只允许 ASCII 字母/数字/下划线/连字符（HTTP_TASK_ID_PATTERN）
      const rawId = String(task.id || config.task_id || config.name || "imported");
      const id = rawId.replace(/[^A-Za-z0-9_-]/g, "_") || "imported";
      const httpTasks = useHttpTasks();
      // 编辑器草稿保护：与其他打开/替换草稿的路径一致，先经 dirty 确认，
      // 否则仓库导入会静默覆盖未保存的修改
      if (!(await httpTasks.confirmDiscardHttpTaskIfDirty())) {
        repoImport.value.disclaimer = task;
        return;
      }
      const draft = httpTaskDraftFromConfig({
        ...(config as unknown as HttpTaskConfig),
        task_id: id,
      });
      // 新建语义：允许用户改 ID 后再保存，避免与他人已导入的同名任务撞车
      draft._isNew = true;
      if (name) draft.name = name;
      if (description) draft.description = description;
      httpTasks.setHttpTaskDraft(draft);
    } else {
      // 浏览器任务 ID 除字符集外还要求以字母开头，故数字开头的 id 补前缀
      let id = String(task.id || name || "imported").replace(/[^A-Za-z0-9_]/g, "_");
      if (/^[0-9]/.test(id)) {
        id = "task_" + id;
      }
      const tasks = useTasks();
      if (!(await tasks.confirmDiscardTaskIfDirty())) {
        // 用户放弃丢弃草稿：恢复免责声明，让弹窗停留在确认页而非静默关闭
        repoImport.value.disclaimer = task;
        return;
      }
      tasks.setTaskDraft({
        id,
        name: name || task.name || "",
        description,
        url: String(config.url ?? ""),
        json: JSON.stringify(config, null, 2),
        _isNew: true,
      });
      tasks.jsonError.value = "";
    }

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
