/**
 * 云端仓库任务导入（单例）。
 * 从 useTasks 拆出：仓库索引拉取、免责声明与导入到编辑器。
 *
 * 一次读取来源的全部索引，按条目类型自动保存并打开对应编辑器。
 */

import { ref, computed, watch } from "vue";
import type { HttpTaskConfig, RepoTask } from "../api/types";
import { repoApi } from "../api";
import { extractApiError } from "../api/client";
import { frontendLogger } from "../utils/logger";
import {
  TASK_REPO_SOURCES,
  presetRepoIndexUrl,
  presetRepoIndexUrls,
  type TaskRepoKind,
  type TaskRepoSourceId,
} from "../utils/constants";
import { httpTaskDraftFromConfig, httpTaskPayload } from "../utils/httpTask";
import { useToast } from "./useToast";
import { tasksApi } from "../api";
import { useTasks } from "./useTasks";
import { useHttpTasks } from "./useHttpTasks";
import { useScripts } from "./useScripts";
import { SCRIPT_MAX_BYTES, scriptContentBytes } from "../utils/scriptDraft";
import { readRepoIndexCache, writeRepoIndexCache } from "../utils/repoIndexCache";
import { isRepoTaskIndex, repoTaskKind, repoTaskKey } from "../utils/repoTask";
import { repoSourceUrl } from "../utils/repoSource";

/** 仓库条目的归属类型；与 `constants` 的 `TaskRepoKind` 同源 */
export type RepoKind = TaskRepoKind;

/** 类别的中文名：标题、空态与失败提示共用一处措辞 */
export function repoKindLabel(kind: RepoKind): string {
  return kind === "http" ? "HTTP 登录任务" : kind === "script" ? "脚本任务" : "浏览器任务";
}

/**
 * 归一化条目的类型。
 *
 * 缺省/空串视为旧浏览器条目；未知类型返回空串并在界面提示。
 */
const normalizeRepoTaskKind = repoTaskKind;

const repoImport = ref({
  visible: false,
  /** 当前索引地址：预设源由源决定，自定义源由用户手填 */
  url: presetRepoIndexUrl("browser", "github"),
  source: "github" as TaskRepoSourceId,
  /** 用户手输的自定义索引地址：切走再切回时恢复，避免误点一下就把已填内容冲掉 */
  customUrl: "",
  loading: false,
  /** 正在下载脚本正文，确认弹窗要等下载完成才能展示。 */
  previewLoading: false,
  /** 确认后的下载、保存与跳转共用互斥，防止重复导入或切换类别。 */
  importing: false,
  importError: "",
  /** 最近一次拉取是否成功（含"合法但为空"）：空态据此区分「还没加载」与「该源没有条目」 */
  loaded: false,
  /** 当前列表对应的完整索引地址与获取时间，用于刷新时判断能否保留旧列表 */
  loadedUrl: "",
  fetchedAt: 0,
  fromCache: false,
  error: "",
  tasks: [] as RepoTask[],
  searchQuery: "",
  disclaimer: null as RepoTask | null,
  /** 下载后的脚本正文：用户确认前完整展示，与最终保存共用同一份数据 */
  scriptPreview: "",
  /** 列表点选的任务（右侧详情预览用；导入仍经 disclaimer 二次确认） */
  selected: null as RepoTask | null,
});

/** 未知类型条目数；三种已知类别均在统一列表展示。 */
const foreignRepoTaskCount = computed(
  () =>
    repoImport.value.tasks.filter(
      (t) => normalizeRepoTaskKind(t.type) === "",
    ).length,
);

/**
 * 按关键词匹配仓库条目，向导的学校匹配可额外限定类别。
 *
 * 弹窗的列表（`filteredRepoTasks`）与首次启动向导的学校名匹配共用这一个口径：
 * 对 name/description/author/tags 拼串做小写
 * 匹配；多个空白分隔的关键词需全部命中。单独导出纯函数是为了两处永不分叉——向导"找到 3 个匹配"而弹窗里
 * 搜不出来，比没有向导更糟。
 */
export function filterRepoTasks(tasks: RepoTask[], kind: RepoKind | "all", query: string): RepoTask[] {
  const sameKind = tasks.filter((t) => {
    const taskKind = repoTaskKind(t.type);
    return taskKind && (kind === "all" || taskKind === kind);
  });
  const keywords = query.trim().toLowerCase().split(/\s+/).filter(Boolean);
  if (!keywords.length) return sameKind;
  return sameKind.filter((t) => {
    const searchable = [t.name, t.description, t.author, ...(t.tags || [])].filter(Boolean).join(" ").toLowerCase();
    return keywords.every((keyword) => searchable.includes(keyword));
  });
}

const filteredRepoTasks = computed(() =>
  filterRepoTasks(repoImport.value.tasks, "all", repoImport.value.searchQuery),
);

/** 搜索隐藏的条目不应继续作为页脚导入目标。 */
watch(filteredRepoTasks, (tasks) => {
  const selected = repoImport.value.selected;
  if (selected && !tasks.some((task) => repoTaskKey(task) === repoTaskKey(selected))) repoImport.value.selected = null;
}, { flush: "sync" });

const repoTaskCount = computed(() => filterRepoTasks(repoImport.value.tasks, "all", "").length);

/** 当前源的选项（含说明文案）；未知源回退到自定义，避免取到 undefined */
const currentSource = computed(
  () => TASK_REPO_SOURCES.find((s) => s.id === repoImport.value.source) ?? TASK_REPO_SOURCES[TASK_REPO_SOURCES.length - 1],
);

/** 当前源的「直接查看仓库」地址：预设源用仓库主页，自定义源回退成用户自填的地址 */
const sourceHomeUrl = computed(() => repoSourceUrl(currentSource.value.homeUrl || repoImport.value.url));

const { toastOnly } = useToast();

// 索引拉取序号（epoch）守卫：只有最新一次请求可以写状态。
// 交错场景——慢索引 A 在途 → 关弹窗重开（loading 被 showRepoImport 复位）→
// 为 URL B 再点一次 → A 迟到覆盖 B 的列表并提前清 loading，用户会把 A 源的
// 任务当 B 源导入。与 useConfig 的 saveSeq / fetchConfigEpoch 同口径。
let fetchIndexSeq = 0;
/** 待确认的脚本载荷；预览和落盘共用，防止确认前后远端文件变化。 */
let pendingScript: Record<string, unknown> | null = null;
/** 关闭或切换条目后，迟到的脚本下载结果不得重新打开确认弹窗。 */
let previewSeq = 0;
/** 同一会话中刷新单份索引失败时保留其上次结果，避免半份任务库消失。 */
const indexSnapshots = new Map<string, { tasks: RepoTask[]; fetchedAt: number }>();

/**
 * 按来源回填入口地址；预设源实际读取全部索引。
 *
 * 预设源的入口地址保持稳定；自定义源是用户手填的地址，
 * 只在确实存过手输内容时回填（否则会把输入框清空，比保留上一个源的地址更差）。
 */
function applyPresetIndexUrl() {
  const preset = presetRepoIndexUrl("browser", repoImport.value.source);
  if (preset) {
    repoImport.value.url = preset;
  } else if (repoImport.value.customUrl) {
    repoImport.value.url = repoImport.value.customUrl;
  }
}

/** 切换仓库源并自动读取其索引；自定义源无地址时等待用户输入。 */
function selectRepoSource(source: TaskRepoSourceId, load = true) {
  if (repoImport.value.importing) return;
  if (source === repoImport.value.source && load) return;
  cancelRepoDisclaimer();
  indexSnapshots.clear();
  // 离开自定义源前先记住手输内容：否则误点一下 GitHub 再点回来，已填的地址就没了
  if (repoImport.value.source === "custom" && source !== "custom") {
    repoImport.value.customUrl = repoImport.value.url;
  }
  const emptyCustom = source === "custom" && repoImport.value.source !== "custom" && !repoImport.value.customUrl;
  repoImport.value.source = source;
  // 地址取自 TASK_REPO_SOURCES（经 presetRepoIndexUrl，按类别），不在此处各写一份：
  // 同一 host 曾在多处硬编码，正是「分享适配」指向错仓库那类缺陷的成因
  if (emptyCustom) repoImport.value.url = "";
  else applyPresetIndexUrl();
  ++fetchIndexSeq;
  repoImport.value.loading = false;
  repoImport.value.loaded = false;
  repoImport.value.loadedUrl = "";
  repoImport.value.fetchedAt = 0;
  repoImport.value.fromCache = false;
  repoImport.value.tasks = [];
  repoImport.value.selected = null;
  repoImport.value.error = "";
  if (load && repoImport.value.visible && repoImport.value.url.trim()) {
    void fetchRepoIndex({ preferCache: true });
  }
}

/**
 * 打开导入弹窗时的预置项（首次启动向导等外部流程使用）。
 *
 * `afterImport` 提供时由调用方接管导入后的流程：不再跳转编辑器
 * （向导需要停留在向导页），落盘后的最终任务 id 经回调交还（用于绑定方案）。
 */
export interface RepoImportOptions {
  /** 打开后预选的仓库源（自定义源无预设地址，传了也只切标签不换地址） */
  source?: TaskRepoSourceId;
  /** 打开后预填的搜索关键词（向导场景 = 学校名） */
  keyword?: string;
  /** 导入成功后的回调（最终 ID 与实际类型）；提供时抑制跳转编辑器 */
  afterImport?: (id: string, kind: RepoKind) => void | Promise<void>;
}

/** 当次打开的预置项；导入成功或关闭弹窗时清空，失败后允许重试。 */
let importOptions: RepoImportOptions | null = null;

/**
 * 打开导入弹窗并复位上次残留的搜索词/列表/错误，避免旧内容闪现。
 *
 * 所有入口展示全部任务，导入去向由选中条目的类型决定。
 *
 * 传入 `opts` 时为外部流程预置：先复位再套用 source/keyword，随后读取缓存或拉索引。
 */
function showRepoImport(opts?: RepoImportOptions) {
  if (repoImport.value.importing) return;
  ++fetchIndexSeq;
  ++previewSeq;
  importOptions = opts ?? null;
  indexSnapshots.clear();
  repoImport.value.visible = true;
  applyPresetIndexUrl();
  repoImport.value.error = "";
  repoImport.value.loaded = false;
  repoImport.value.loadedUrl = "";
  repoImport.value.fetchedAt = 0;
  repoImport.value.fromCache = false;
  repoImport.value.tasks = [];
  repoImport.value.searchQuery = "";
  repoImport.value.loading = false;
  repoImport.value.previewLoading = false;
  repoImport.value.importError = "";
  repoImport.value.disclaimer = null;
  repoImport.value.scriptPreview = "";
  pendingScript = null;
  repoImport.value.selected = null;
  if (opts?.source) {
    // 预设源读取全部索引；自定义源的地址留给用户手填。
    selectRepoSource(opts.source, false);
  }
  if (opts?.keyword) {
    repoImport.value.searchQuery = opts.keyword;
  }
  if (repoImport.value.url.trim()) void fetchRepoIndex({ preferCache: true });
}

/** 关闭导入弹窗（不清理状态，下次打开时由 showRepoImport 统一复位） */
function closeRepoImport() {
  if (repoImport.value.importing) return;
  dismissRepoImport();
}

/** 成功保存后的内部收尾允许关闭；用户关闭操作受导入互斥保护。 */
function dismissRepoImport() {
  ++fetchIndexSeq;
  ++previewSeq;
  importOptions = null;
  pendingScript = null;
  repoImport.value.scriptPreview = "";
  repoImport.value.previewLoading = false;
  repoImport.value.disclaimer = null;
  repoImport.value.importError = "";
  repoImport.value.visible = false;
}

/**
 * 打开/切源时优先读取未过期缓存；手动调用默认强制刷新网络。
 * 结果非数组视为失败，空数组是"该源暂无条目"。
 */
async function fetchRepoIndex(opts?: { preferCache?: boolean }) {
  if (repoImport.value.importing) return;
  const urls = repoImport.value.source === "custom"
    ? [repoImport.value.url.trim()].filter(Boolean)
    : presetRepoIndexUrls(repoImport.value.source);
  if (!urls.length) {
    repoImport.value.error = "请输入索引地址";
    return;
  }
  if (urls.some((url) => !repoSourceUrl(url))) {
    repoImport.value.error = "索引地址必须是完整的 HTTP 或 HTTPS 地址";
    return;
  }
  // 取号：迟到的旧响应据此丢弃（见 fetchIndexSeq 声明处注释）
  const seq = ++fetchIndexSeq;
  const requestKey = urls.join("\n");
  // 缓存有效期只决定是否发请求，不能决定刷新失败后是否仍可浏览已有列表。
  const retainCurrent = repoImport.value.loaded && repoImport.value.loadedUrl === requestKey;
  const selectedKey = retainCurrent && repoImport.value.selected ? repoTaskKey(repoImport.value.selected) : undefined;
  repoImport.value.loading = true;
  repoImport.value.error = "";
  if (!retainCurrent) {
    repoImport.value.loaded = false;
    repoImport.value.loadedUrl = "";
    repoImport.value.fetchedAt = 0;
    repoImport.value.fromCache = false;
    repoImport.value.tasks = [];
    repoImport.value.selected = null;
  }
  try {
    const results = await Promise.allSettled(urls.map(async (url) => {
      const cached = opts?.preferCache ? readRepoIndexCache(url) : null;
      if (cached) return { ...cached, fromCache: true };
      const data = await repoApi.fetchIndex(url);
      if (!Array.isArray(data)) throw new Error("索引格式不正确（应为 JSON 数组）");
      if (!isRepoTaskIndex(data)) throw new Error("索引条目无效：请检查重复 ID、名称、文件地址和标签");
      return { url, tasks: data, fetchedAt: Date.now(), fromCache: false };
    }));
    if (seq !== fetchIndexSeq || !repoImport.value.visible) return;
    const errors: string[] = [];
    const entries: { tasks: RepoTask[]; fetchedAt: number }[] = [];
    let fromCache = true;
    let succeeded = 0;
    results.forEach((result, index) => {
      const url = urls[index]!;
      if (result.status === "fulfilled") {
        const entry = result.value;
        succeeded++;
        fromCache &&= entry.fromCache;
        entries.push(entry);
        indexSnapshots.set(url, entry);
        if (!entry.fromCache) writeRepoIndexCache(url, entry.tasks, entry.fetchedAt);
      } else {
        const label = repoImport.value.source === "custom" ? "任务列表" : url === presetRepoIndexUrl("http", repoImport.value.source) ? "HTTP 任务列表" : "浏览器与脚本任务列表";
        errors.push(`${label}：${extractApiError(result.reason, "加载失败")}`);
        const previous = retainCurrent ? indexSnapshots.get(url) : undefined;
        if (previous) entries.push(previous);
      }
    });
    repoImport.value.error = errors.length ? `${succeeded ? "部分任务加载失败" : "任务库加载失败"}，可刷新或切换来源。${errors.join("；")}` : "";
    if (!succeeded) return;
    // 按类别和 ID 去重，共用索引与跨类别同 ID 都不会造成重复或错选。
    const merged = new Map(entries.flatMap((entry) => entry.tasks).map((task) => [repoTaskKey(task), task]));
    const tasks = [...merged.values()];
    repoImport.value.loaded = true;
    repoImport.value.loadedUrl = requestKey;
    repoImport.value.fetchedAt = Math.min(...entries.map((entry) => entry.fetchedAt));
    repoImport.value.fromCache = fromCache && !errors.length;
    repoImport.value.tasks = tasks;
    repoImport.value.selected = tasks.find((task) => repoTaskKey(task) === selectedKey) ?? null;
  } catch (e) {
    // 被取代的旧请求失败同样不写状态：否则会用一个已过期的错误覆盖新请求的结果
    if (seq !== fetchIndexSeq || !repoImport.value.visible) return;
    const msg = extractApiError(e, "加载失败，请检查地址是否正确");
    repoImport.value.error = msg;
    toastOnly(false, `任务库加载失败: ${msg}`);
  } finally {
    // 仅最新请求负责复位 loading：旧请求提前清掉会让界面在 B 仍在途时误示"已完成"
    if (seq === fetchIndexSeq && repoImport.value.visible) repoImport.value.loading = false;
  }
}

/** 列表点选任务：右侧详情区展示截图大图与完整信息（不触发导入） */
function selectRepoTask(task: RepoTask) {
  if (repoImport.value.importing) return;
  cancelRepoDisclaimer();
  repoImport.value.selected = task;
}

/** 脚本先下载并展示完整正文；其他任务按现有确认流程下载。 */
async function confirmRepoImport(task: RepoTask) {
  if (repoImport.value.importing || repoImport.value.previewLoading) return;
  repoImport.value.importError = "";
  const seq = ++previewSeq;
  pendingScript = null;
  repoImport.value.scriptPreview = "";
  const kind = repoTaskKind(task.type);
  if (!kind) {
    toastOnly(false, "该任务类型暂不支持");
    return;
  }
  if (kind === "script") {
    repoImport.value.previewLoading = true;
    try {
      const data = (await repoApi.fetchTask(task.url)) as Record<string, unknown>;
      if (seq !== previewSeq || !repoImport.value.visible) return;
      const config = (data.config && typeof data.config === "object" ? data.config : data) as Record<string, unknown>;
      if (
        normalizeRepoTaskKind(config.type as string | undefined) !== "script"
        || (data.config && data.type !== undefined && normalizeRepoTaskKind(data.type as string | undefined) !== "script")
      ) {
        throw new Error("索引与脚本文件的类型不一致");
      }
      const content = config.content;
      if (typeof content !== "string" || !content.trim() || scriptContentBytes(content) > SCRIPT_MAX_BYTES) {
        throw new Error("脚本内容为空或超过 100 KB");
      }
      // 当前编辑器只支持内嵌正文和项目内 Python；拒绝无法正确保存或需要本地路径的远程字段。
      if (config.script_path || config.args || config.work_dir || config.binary_path || config.timeout) {
        throw new Error("仓库脚本目前只支持内嵌 Python 正文，请移除本地路径、参数与自定义运行项");
      }
      pendingScript = config;
      repoImport.value.scriptPreview = content;
    } catch (e) {
      if (seq !== previewSeq) return;
      toastOnly(false, extractApiError(e, "脚本下载失败"));
      return;
    } finally {
      if (seq === previewSeq) repoImport.value.previewLoading = false;
    }
  }
  repoImport.value.disclaimer = task;
}

/** 取消免责声明，回到任务列表继续浏览 */
function cancelRepoDisclaimer() {
  if (repoImport.value.importing) return;
  ++previewSeq;
  repoImport.value.disclaimer = null;
  repoImport.value.scriptPreview = "";
  repoImport.value.previewLoading = false;
  pendingScript = null;
  repoImport.value.importError = "";
}

/**
 * 接受免责声明并执行导入：下载任务 JSON → 规范化类型与 ID → 直接落盘为新任务，
 * 随后打开编辑器并跳转过去（改动此后走自动保存）。
 *
 * 导入即落盘而非写"未保存草稿"：自动保存模式下没有草稿态可承接，且撞已有 id 时
 * 会追加 _N 后缀（导入不是覆盖语义），落盘后由用户在编辑器里继续调整。
 */
async function acceptRepoDisclaimer() {
  if (repoImport.value.importing) return;
  const task = repoImport.value.disclaimer;
  if (task && repoTaskKind(task.type) === "script" && !pendingScript) {
    toastOnly(false, "请先下载并预览脚本正文");
    return;
  }
  if (!task) return;
  const external = importOptions;
  const kind = repoTaskKind(task.type);
  if (!kind) return;
  repoImport.value.importing = true;
  repoImport.value.importError = "";
  let savedId = "";

  try {
    const data = (pendingScript ?? (await repoApi.fetchTask(task.url))) as Record<string, unknown>;
    // 兼容导出详情形态（{ summary, config }）：字段都在 config 里，取内层再读
    const config = (data.config && typeof data.config === "object" ? data.config : data) as Record<string, unknown>;
    const entryKind = normalizeRepoTaskKind(config.type as string | undefined);
    const wrapperKind = data.config && data.type !== undefined
      ? normalizeRepoTaskKind(data.type as string | undefined)
      : entryKind;
    // 索引与文件必须声明相同类型，避免文件被替换后按错误模型落盘。
    if (normalizeRepoTaskKind(task.type) !== kind || entryKind !== kind || wrapperKind !== kind) {
      throw new Error("任务文件的类型与列表标注不一致，请联系任务作者修正");
    }

    const name = String(data.name ?? config.name ?? task.name ?? "");
    const description = String(data.description ?? config.description ?? task.description ?? "");
    const existing = new Set((await tasksApi.list()).map((item) => item.id));
    const uniqueId = (base: string): string => {
      let id = base.slice(0, 64);
      let n = 2;
      while (existing.has(id) || id === "default") {
        const suffix = `_${n++}`;
        id = base.slice(0, 64 - suffix.length) + suffix;
      }
      return id;
    };

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
      id = uniqueId(id);
      const draftPayload = httpTaskDraftFromConfig({ ...(config as unknown as HttpTaskConfig), task_id: id });
      if (name) draftPayload.name = name;
      if (description) draftPayload.description = description;
      const payload: Record<string, unknown> = {
        ...httpTaskPayload(draftPayload),
        task_id: id,
        // 仓库来源等元数据保留在任务模型自己的 metadata，而非把 UI 草稿字段落盘。
        metadata: config.metadata ?? {},
      };
      await tasksApi.save(id, payload);
      savedId = id;
      await httpTasks.fetchHttpTasks(true);
      await finishImport(external, id, name, task, "tasks-http", () => httpTasks.showHttpTaskEditor(id));
    } else if (kind === "script") {
      if (typeof config.content !== "string" || !config.content.trim()) {
        throw new Error("脚本内容为空");
      }
      const id = uniqueId(String(task.id || "imported").replace(/[^A-Za-z0-9_-]/g, "_") || "imported");
      await tasksApi.save(id, {
        type: "script",
        task_id: id,
        name: name || id,
        description,
        content: config.content,
        binary_path: "",
      });
      savedId = id;
      const scripts = useScripts();
      await scripts.fetchScripts(true);
      await finishImport(external, id, name, task, "tasks-scripts", () => scripts.showScriptEditor(id));
    } else {
      // 浏览器任务 ID 除字符集外还要求以字母开头，故数字开头的 id 补前缀
      let id = String(task.id || name || "imported").replace(/[^A-Za-z0-9_]/g, "_");
      if (/^[0-9]/.test(id)) {
        id = "task_" + id;
      }
      const tasks = useTasks();
      id = uniqueId(id || "imported");
      const payload = { ...config } as Record<string, unknown>;
      payload.type = "browser";
      payload.task_id = id;
      if (name) payload.name = name;
      if (description) payload.description = description;
      delete payload.version;
      delete payload.source;
      await tasksApi.save(id, payload);
      savedId = id;
      await tasks.fetchTasks(true);
      await finishImport(external, id, name, task, "tasks-browser", () => tasks.showTaskEditor(id));
    }

    frontendLogger.info("tasks", `已从仓库导入: ${task.name}`);
  } catch (e) {
    const msg = extractApiError(e, "导入任务失败");
    frontendLogger.error("tasks", "仓库任务导入失败", msg);
    if (savedId) {
      dismissRepoImport();
      toastOnly(false, `任务已导入（${savedId}），但打开编辑器失败，请从任务列表打开: ${msg}`);
    } else {
      repoImport.value.importError = msg;
      toastOnly(false, `导入失败: ${msg}`);
    }
  } finally {
    repoImport.value.importing = false;
  }
}

/**
 * 导入收尾：外部流程（`external.afterImport`，即首次启动向导）接管后续——
 * 不打开编辑器、不跳路由，回调失败如实说"任务已导入但后续配置失败"（任务本身
 * 已落盘，不能让用户以为白导了）；默认流程维持原行为：打开编辑器并跳转过去。
 */
async function finishImport(
  external: RepoImportOptions | null,
  id: string,
  name: string,
  task: RepoTask,
  routeName: string,
  openEditor: () => Promise<unknown>,
): Promise<void> {
  const label = name || task.name;
  if (external?.afterImport) {
    try {
      await external.afterImport(id, repoTaskKind(task.type) as RepoKind);
      dismissRepoImport();
      toastOnly(true, `已导入「${label}」`);
    } catch (e) {
      const msg = extractApiError(e, "后续配置失败");
      frontendLogger.error("tasks", "导入后续处理失败", msg);
      toastOnly(false, `已导入「${label}」，但后续配置失败: ${msg}`);
      dismissRepoImport();
    }
    return;
  }
  await openEditor();
  // 跳到编辑页（面板消费 ?task=<id>）
  await navigateToTaskEditor(routeName, id);
  dismissRepoImport();
  toastOnly(true, `已导入「${label}」，已打开编辑器`);
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
    repoTaskCount,
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
