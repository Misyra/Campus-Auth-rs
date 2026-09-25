/**
 * 云端仓库任务导入（单例）。
 * 从 useTasks 拆出：仓库索引拉取、免责声明与导入到编辑器。
 *
 * 两类任务**各有一份索引**（`index.json` 收浏览器任务、`index.http.json` 收直连任务，
 * 见 `utils/constants.ts` 的 `TaskRepoKind`），故打开弹窗前必须由调用方声明要看哪一类
 * （`showRepoImport("browser" | "http")`）：它同时决定**读哪份索引**、列表的防御性过滤
 * 与导入去向。索引地址同时取决于类别与源（类别 × 源），故切类别、切源都要重取一次
 * （`applyPresetIndexUrl`）——任一处各写一份就会出现「在直连列表里拉了浏览器索引」。
 * 导入确认后需要写入编辑器草稿，通过 useTasks / useHttpTasks 单例获取（无循环依赖）。
 */

import { ref, computed } from "vue";
import type { HttpTaskConfig, RepoTask } from "../api/types";
import { repoApi } from "../api";
import { extractApiError } from "../api/client";
import { frontendLogger } from "../utils/logger";
import {
  TASK_REPO_SOURCES,
  presetRepoIndexUrl,
  type TaskRepoKind,
  type TaskRepoSourceId,
} from "../utils/constants";
import { httpTaskDraftFromConfig, httpTaskPayload } from "../utils/httpTask";
import { useToast } from "./useToast";
import { tasksApi } from "../api";
import { useTasks } from "./useTasks";
import { useHttpTasks } from "./useHttpTasks";

/** 仓库条目的归属类型（本页只处理这两类）；与 `constants` 的 `TaskRepoKind` 同源 */
export type RepoKind = TaskRepoKind;

/** 类别的中文名：标题、空态与失败提示共用一处措辞 */
export function repoKindLabel(kind: RepoKind): string {
  return kind === "http" ? "直连任务" : "浏览器任务";
}

/**
 * 归一化条目的类型。
 *
 * 索引文件本身只承载一类条目，故此函数在这里是**防御**（远端索引被写混、或自定义源
 * 指到了另一类的索引），用它与当前类别比对。缺省/空串视为 `browser`：浏览器任务条目
 * 不带 `type`，那是直连任务加入仓库之前唯一的形态；其余类型（如 `script`）返回空串，
 * 即"两类列表都不进"。
 */
function normalizeRepoTaskKind(type: string | undefined): RepoKind | "" {
  const value = (type ?? "").trim().toLowerCase();
  if (value === "" || value === "browser") return "browser";
  if (value === "http") return "http";
  return "";
}

const repoImport = ref({
  visible: false,
  /** 当前索引地址：预设源由 (类别, 源) 决定，自定义源由用户手填 */
  url: presetRepoIndexUrl("browser", "github"),
  source: "github" as TaskRepoSourceId,
  /** 本次要看哪一类条目：决定读哪份索引、列表过滤与导入去向（见 acceptRepoDisclaimer） */
  repoKind: "browser" as RepoKind,
  /** 用户手输的自定义索引地址：切走再切回时恢复，避免误点一下就把已填内容冲掉 */
  customUrl: "",
  loading: false,
  /** 最近一次拉取是否成功（含"合法但为空"）：空态据此区分「还没加载」与「该源没有条目」 */
  loaded: false,
  error: "",
  tasks: [] as RepoTask[],
  searchQuery: "",
  disclaimer: null as RepoTask | null,
  /** 列表点选的任务（右侧详情预览用；导入仍经 disclaimer 二次确认） */
  selected: null as RepoTask | null,
});

/**
 * 与当前类别不符而被跳过的条目数。
 *
 * 索引文件只承载一类条目，出现不符即该文件写错了（或自定义地址指到了另一类的索引）。
 * 此时静默过滤会让用户对着空列表猜原因，故把它显式暴露给弹窗提示。
 */
const foreignRepoTaskCount = computed(
  () =>
    repoImport.value.tasks.filter(
      (t) => normalizeRepoTaskKind(t.type) !== repoImport.value.repoKind,
    ).length,
);

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

/**
 * 按当前的 (类别, 源) 回填索引地址。
 *
 * 预设源取该类别在该源下的那份索引文件；自定义源是用户手填的地址、与类别无关，
 * 只在确实存过手输内容时回填（否则会把输入框清空，比保留上一个源的地址更差）。
 */
function applyPresetIndexUrl() {
  const preset = presetRepoIndexUrl(repoImport.value.repoKind, repoImport.value.source);
  if (preset) {
    repoImport.value.url = preset;
  } else if (repoImport.value.customUrl) {
    repoImport.value.url = repoImport.value.customUrl;
  }
}

/** 切换仓库源并回填该类别下对应的预设索引地址（自定义源恢复上次手输的 URL） */
function selectRepoSource(source: TaskRepoSourceId) {
  // 离开自定义源前先记住手输内容：否则误点一下 GitHub 再点回来，已填的地址就没了
  if (repoImport.value.source === "custom" && source !== "custom") {
    repoImport.value.customUrl = repoImport.value.url;
  }
  repoImport.value.source = source;
  // 地址取自 TASK_REPO_SOURCES（经 presetRepoIndexUrl，按类别），不在此处各写一份：
  // 同一 host 曾在多处硬编码，正是「分享适配」指向错仓库那类缺陷的成因
  applyPresetIndexUrl();
}

/**
 * 打开导入弹窗并复位上次残留的搜索词/列表/错误，避免旧内容闪现。
 *
 * `kind` 必须由调用方声明：任务页两个 Tab（浏览器/直连）、设置页入口各自知道
 * 自己要哪一类，默认值会让"忘了传"变成静默导入错类型（列表看着空空如也）。
 * 换类别同时意味着**换索引文件**，故一并重取预设地址。
 */
function showRepoImport(kind: RepoKind) {
  repoImport.value.visible = true;
  repoImport.value.repoKind = kind;
  applyPresetIndexUrl();
  repoImport.value.error = "";
  repoImport.value.loaded = false;
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

/** 按当前输入的索引地址拉取远程任务列表；结果非数组视为失败，空数组是"该源暂无条目" */
async function fetchRepoIndex() {
  const url = repoImport.value.url.trim();
  if (!url) {
    repoImport.value.error = "请输入索引地址";
    return;
  }
  // 取号：迟到的旧响应据此丢弃（见 fetchIndexSeq 声明处注释）
  const seq = ++fetchIndexSeq;
  const label = repoKindLabel(repoImport.value.repoKind);
  repoImport.value.loading = true;
  repoImport.value.error = "";
  repoImport.value.loaded = false;
  repoImport.value.tasks = [];
  repoImport.value.searchQuery = "";
  repoImport.value.selected = null;
  try {
    const data = await repoApi.fetchIndex(url);
    if (seq !== fetchIndexSeq) return;
    if (!Array.isArray(data)) {
      repoImport.value.error = "索引格式不正确（应为 JSON 数组）";
      return;
    }
    // 空数组不是失败：这一类的索引里暂时没有条目是合法状态（新仓库、镜像源尚未收录），
    // 由空态文案说明"该源暂无条目"，不弹失败提示
    repoImport.value.loaded = true;
    repoImport.value.tasks = data;
  } catch (e) {
    // 被取代的旧请求失败同样不写状态：否则会用一个已过期的错误覆盖新请求的结果
    if (seq !== fetchIndexSeq) return;
    const msg = extractApiError(e, "加载失败，请检查地址是否正确");
    repoImport.value.error = msg;
    toastOnly(false, `获取${label}索引失败: ${msg}`);
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
 * 接受免责声明并执行导入：下载任务 JSON → 规范化类型与 ID → 直接落盘为新任务，
 * 随后打开编辑器并跳转过去（改动此后走自动保存）。
 *
 * 导入即落盘而非写"未保存草稿"：自动保存模式下没有草稿态可承接，且撞已有 id 时
 * 会追加 _N 后缀（导入不是覆盖语义），落盘后由用户在编辑器里继续调整。
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
    // （条目声明 browser 而文件是 http 等），此时按声明的类型落盘会得到一堆空字段
    if (entryKind !== kind) {
      toastOnly(false, kind === "http" ? '该条目不是直连任务（type 需为 "http"）' : "该条目的类型与当前列表不一致，请在对应列表导入");
      return;
    }

    const name = String(data.name ?? config.name ?? task.name ?? "");
    const description = String(data.description ?? config.description ?? task.description ?? "");

    // 自动保存模式下没有"未保存草稿"：仓库导入直接落盘为任务，
    // 再跳到该任务的编辑页（?task=<id>），由用户继续调整（改动自动保存）。
    if (kind === "http") {
      // 任务 ID 归一化：只允许 ASCII 字母/数字/下划线/连字符（HTTP_TASK_ID_PATTERN）
      const rawId = String(task.id || config.task_id || config.name || "imported");
      let id = rawId.replace(/[^A-Za-z0-9_-]/g, "_") || "imported";
      const httpTasks = useHttpTasks();
      // 撞已有 id：追加 _N 后缀（导入不是覆盖语义，覆盖应由删除+重导显式发生）。
      // 后缀必须拼在**清洗后**的 id 上：拼原始条目名会把非 ASCII 字符带回 id
      // （中文任务名很常见），而后端 `is_valid_task_id` 只收 `[A-Za-z0-9_-]`——
      // 第二次导入同一条目就必然被拒，报错还只显示"任务不存在"。
      const existingHttp = new Set(httpTasks.httpTasks.value.map((t) => t.id));
      let n = 2;
      while (existingHttp.has(id)) {
        id = `${id}_${n++}`;
      }
      const payload: Record<string, unknown> = {
        ...httpTaskDraftFromConfig({
          ...(config as unknown as HttpTaskConfig),
          task_id: id,
        }) as unknown as Record<string, unknown>,
      };
      const draftPayload = httpTaskDraftFromConfig({ ...(config as unknown as HttpTaskConfig), task_id: id });
      if (name) draftPayload.name = name;
      if (description) draftPayload.description = description;
      Object.assign(payload, httpTaskPayload(draftPayload), { task_id: id });
      await tasksApi.save(id, payload);
      await httpTasks.fetchHttpTasks(true);
      await httpTasks.showHttpTaskEditor(id);
      // 跳到编辑页（面板消费 ?task=<id>）
      await navigateToTaskEditor("tasks-http", id);
    } else {
      // 浏览器任务 ID 除字符集外还要求以字母开头，故数字开头的 id 补前缀
      let id = String(task.id || name || "imported").replace(/[^A-Za-z0-9_]/g, "_");
      if (/^[0-9]/.test(id)) {
        id = "task_" + id;
      }
      const tasks = useTasks();
      const existingBrowser = new Set(tasks.tasks.value.map((t) => t.id));
      let n = 2;
      while (existingBrowser.has(id)) {
        id = `${id}_${n++}`;
      }
      const payload = { ...config } as Record<string, unknown>;
      payload.type = "browser";
      payload.task_id = id;
      if (name) payload.name = name;
      if (description) payload.description = description;
      delete payload.version;
      delete payload.source;
      await tasksApi.save(id, payload);
      await tasks.fetchTasks(true);
      await tasks.showTaskEditor(id);
      await navigateToTaskEditor("tasks-browser", id);
    }

    closeRepoImport();
    frontendLogger.info("tasks", `已从仓库导入: ${task.name}`);
    toastOnly(true, `已导入「${name || task.name}」，已打开编辑器`);
  } catch (e) {
    const msg = extractApiError(e, "下载任务失败");
    frontendLogger.error("tasks", "远程任务下载失败", msg);
    toastOnly(false, `远程任务下载失败: ${msg}`);
  }
}

/** 导入后跳转到对应面板的编辑页（?task=<id> 由面板消费） */
async function navigateToTaskEditor(routeName: string, taskId: string): Promise<void> {
  const { router } = await import("../router");
  await router.push({ name: routeName, query: { task: taskId } });
}

export function useRepoImport() {
  return {
    repoImport,
    filteredRepoTasks,
    foreignRepoTaskCount,
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
