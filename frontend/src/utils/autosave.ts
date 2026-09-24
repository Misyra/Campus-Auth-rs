/**
 * 自动保存状态机、状态字与控制器（四个任务面板共用）。
 *
 * 抽出来的理由：状态字此前在浏览器任务 / 直连任务两个面板各写一份 `switch`，
 * 脚本面板迁移后又会有第三份；而"有缺口时不能显示『改动自动保存』"这条
 * 恰恰是最容易被漏掉的一处——状态字说谎比不显示状态字更坏。
 *
 * `createAutosaveController` 是 2026-09-24 补上的第二层：面板里那套
 * `autosaveTimer + autosaveSeq + pendingChanges + lastSavedFingerprint + flushPendingAutosave`
 * 原本四份**逐字重复**（连注释都一样），其中两条判据都在关键路径上——
 * "在途响应可能属于上一份草稿"（detached）与"改回原样不发请求"（载荷指纹）。
 * 四份各自演进迟早分叉，而这类分叉的表现是"静默丢改动"，最难在事后发现。
 * 先把四份单测补齐（useTasks / useScripts 的面板测试），再动这一步；替换是机械的：
 * 各面板只剩"注入口径"——什么算有改动、什么算发不出去、往哪儿落盘。
 *
 * 2026-09-24 收尾补的两条判据（都属"同一份草稿有两发请求在飞"这一类）：
 * 1. **同一份载荷不发两遍**：debounce 到点会清掉 `pendingChanges`，而基线要等响应才
 *    更新，于是"在途"这段窗口里 `flush` 会重复发一次；定时任务的第二发 POST 会吃 409
 *    （用户看到一句假的"已存在"，任务其实建成功了）。改为按**在途载荷指纹**比对。
 * 2. **被顶掉的那一发成功也算数**：`onSaved` 表达的是"这份草稿已经在磁盘上"这一事实，
 *    与"由哪一发接管界面态"无关；只在最后那一发里调用的话，被顶掉的首次创建永远不翻
 *    `_isNew` → 定时任务此后每次自动保存都 POST 一个已存在的 id（409 卡死）。
 *    `detached` 仍然不调 `onSaved`：那时共享状态已经属于新草稿了。
 */

import { ref, watch, type Ref } from "vue";
import { extractApiError } from "../api/client";
import { frontendLogger } from "./logger";

/** 自动保存状态机：idle（无在途保存）→ saving（请求在途）→ saved（最近一次成功） */
export type AutosaveState = "idle" | "saving" | "saved" | "error";

/** 自动保存 debounce（毫秒）：打字停顿半秒即落盘 */
export const AUTOSAVE_DEBOUNCE_MS = 500;

export interface AutosaveLabel {
  /** 状态字文案 */
  text: string;
  /** 状态类（tsk-autosave 的修饰类） */
  cls: string;
}

/**
 * 状态字：缺口优先于 idle/saved。
 *
 * `gaps` 非空表示"这次改动根本发不出去"（请求地址空着、前置请求填了一半…），
 * 此时说「改动自动保存」或「已保存」都是谎话——用户会把没落盘的编辑当成已存。
 * `saving` 例外：请求真的在途时先如实说保存中，落盘后再由缺口表达。
 *
 * `isNew` 是"新建但磁盘上还没有"：自动保存模式下新建不再先落一份种子文件，
 * 所以此时说「改动自动保存」同样容易读成"已经存过了"（用户以为列表里已经有它，
 * 返回列表却找不到）。只在空闲态区分——保存中 / 刚保存成功之后它已经存在了。
 */
export function autosaveLabel(
  state: AutosaveState,
  gaps: readonly string[] = [],
  isNew = false,
): AutosaveLabel {
  if (state === "saving") {
    return { text: "保存中…", cls: "tsk-autosave saving" };
  }
  if (gaps.length > 0) {
    return {
      text: `有 ${gaps.length} 处待补全，改动暂未保存`,
      cls: "tsk-autosave blocked",
    };
  }
  switch (state) {
    case "saved":
      return { text: "已保存 · 刚刚", cls: "tsk-autosave saved" };
    case "error":
      return { text: "保存失败，修改后自动重试", cls: "tsk-autosave error" };
    default:
      return isNew
        ? { text: "尚未创建 · 改动后自动保存", cls: "tsk-autosave" }
        : { text: "改动自动保存", cls: "tsk-autosave" };
  }
}

/**
 * 一个面板的自动保存接线。
 *
 * 控制器只认三件事：**什么算"有改动"**（指纹）、**什么算"发不出去"**（闸口）、
 * **往哪儿发**（落盘动作）。草稿 ref 由调用方持有——四个面板的草稿类型与 ref 名都
 * 不同（`editingTask` / `httpTaskDraft` / `scheduledTaskDraft`），控制器不猜字段。
 */
export interface AutosaveSpec<TDraft> {
  /** 正在编辑的草稿（null = 列表态）；控制器据此挂深度 watcher */
  draft: Ref<TDraft | null>;
  /** 草稿 id：仅用于失败日志，多面板同时自动保存时能对上是谁 */
  idOf(draft: TDraft): string;
  /**
   * 落盘载荷指纹；`null` = 这份草稿根本构造不出载荷（浏览器任务的 JSON 语法非法）。
   * 与基线比对即可判断"有没有真改动"。
   */
  fingerprintOf(draft: TDraft): string | null;
  /** 闸口：发不出去时返回要说的话（缺口 / JSON 非法），`null` = 可以发 */
  blockReasonOf(draft: TDraft): string | null;
  /**
   * 真正落盘，**含落盘后的列表刷新**——列表里的调度摘要 / 下次执行时间由后端回填，
   * 不刷新就不准，它属于"这次落盘带来的可见后果"。抛错 = 失败。
   */
  persist(draft: TDraft): Promise<void>;
  /** 落盘成功后的界面态修正（`_isNew` 翻转、清 JSON 错误提示等）；detached 时不调用 */
  onSaved?(draft: TDraft): void;
  /** toast 出口：由调用方注入，免得本模块反向依赖 composables 分层 */
  toast(success: boolean, message: string): void;
  /** 日志模块名（tasks / scripts / http-task / scheduler） */
  logScope: string;
  /** 换编辑对象时失败提示的主语，如「上一份脚本」 */
  detachedSubject: string;
}

export interface AutosaveController<TDraft> {
  autosaveState: Ref<AutosaveState>;
  /** 登记基线：「当前草稿的内容 == 磁盘上的那份」（打开已存在对象 / 新建种子后调用） */
  markBaseline(draft: TDraft): void;
  /** 登记「磁盘上还没有它 / 已知与磁盘不同步」（导入覆盖的场景）：下一次变更必定落盘 */
  markUnsaved(): void;
  /** 立即落盘（模板替换这类程序化改动，不等 debounce） */
  saveNow(draft: TDraft): Promise<void>;
  /**
   * 冲刷在途 debounce（换编辑对象 / 关闭编辑器前调用）。
   *
   * @param reason `close` = 关闭编辑器：被闸口拦下时出声，否则「退出即生效」在缺口态下
   *   变成静默丢弃；`switch` = 换编辑对象：不接管状态机也不提示（见 `run` 的 detached），
   *   用户只是在列表里点另一条，不该被弹一句。
   */
  flush(reason: "switch" | "close"): Promise<void>;
  /** 清空编辑器状态（删除 / 放弃新建等无需再保存的场景）；顺带关掉草稿，见 `clear` */
  clear(): void;
}

/**
 * 缺口制面板的闸口（脚本 / 直连 / 定时任务三个面板共用）。
 *
 * 浏览器任务面板不用它：那边的闸口是 JSON 语法，说法也不同（要带上解析器的原话）。
 */
export function gapBlocker<TDraft>(
  gapsOf: (draft: TDraft) => string[],
): (draft: TDraft) => string | null {
  return (draft) => {
    const gaps = gapsOf(draft);
    return gaps.length > 0 ? `改动未保存：还缺 ${gaps.join("、")}` : null;
  };
}

export function createAutosaveController<TDraft>(
  spec: AutosaveSpec<TDraft>,
): AutosaveController<TDraft> {
  const autosaveState = ref<AutosaveState>("idle");
  let timer: ReturnType<typeof setTimeout> | null = null;
  /** 在途请求序号：新变更到达时旧响应作废，避免「慢请求后到覆盖新状态」 */
  let seq = 0;
  /** 距上次成功落盘是否又有改动（决定要不要补一发保存） */
  let pendingChanges = false;
  /**
   * 基线：磁盘上那份内容的指纹（落盘载荷的 JSON 串）；`null` = 磁盘上还没有。
   *
   * 判据用"载荷指纹"而不是"草稿 id 变了"：新建草稿的 id 是本地生成的，与磁盘无关，
   * 按 id 判会把"新建"当成"换了个编辑对象"从而永不排期；而指纹比较顺带解决两件事——
   * **新建后什么都没改就不落盘**、以及"内容改回原样也不必再发一次请求"。
   */
  let lastSavedFingerprint: string | null = null;
  /**
   * 正在发送 / 等待响应的那一发（序号 + 载荷指纹）。
   *
   * 用途只有一个：**同一份载荷不要发两遍**。debounce 到点那一刻会把 `pendingChanges`
   * 置回 false，而 `lastSavedFingerprint` 要等响应才更新——于是"已经发出、响应还没回"
   * 这段时间里，判据看不出有一发在路上，`flush`（换编辑对象 / 关闭编辑器）会再发一次
   * 一模一样的请求。对三个 PUT 面板是白跑一趟，对**定时任务**则是第二次 POST 一个刚被
   * 建出来的 id → 409，用户看到一句"已存在"的红字（任务其实建成功了）。
   */
  let inFlight: { seq: number; mark: string } | null = null;

  function stopTimer(): void {
    if (timer) {
      clearTimeout(timer);
      timer = null;
    }
  }

  function markBaseline(draft: TDraft): void {
    stopTimer();
    pendingChanges = false;
    lastSavedFingerprint = spec.fingerprintOf(draft);
    autosaveState.value = "idle";
  }

  /**
   * 复位状态机（不动草稿 ref）。
   *
   * 顺带让在途那一发作废（`seq` 前进）：`markUnsaved` / `clear` 之后这份草稿的
   * 归属已经变了，前一发回来时不该再写基线或状态字。注意这**不能**撤回已经发出的
   * 请求——「放弃新建」时那一发仍会把文件创建出来，这是接口的硬限制（要真取消得引入
   * AbortController），故放弃路径的提示文案不要承诺"什么都没写"。
   */
  function resetMachine(): void {
    stopTimer();
    pendingChanges = false;
    lastSavedFingerprint = null;
    autosaveState.value = "idle";
    seq += 1;
  }

  function markUnsaved(): void {
    resetMachine();
  }

  /**
   * 结束编辑：复位状态机**并关掉草稿**。
   *
   * 关草稿这一步不能留给调用方"顺手"做：深度 watcher 是异步批处理的，只复位状态的话
   * 那一轮迟到的回调仍会读到残留的草稿（基线已空 → 判定有改动）再排一次落盘，
   * 于是"删除 / 放弃后又被写回一次"。watcher 回调读的是**运行当下**的 `draft.value`，
   * 所以置空本身就把那一轮堵死了。
   */
  function clear(): void {
    resetMachine();
    spec.draft.value = null;
  }

  /**
   * 落盘核心。
   *
   * `detached`：换编辑对象时补发的那一发（见 `flush`）。旧草稿的响应回到浏览器时新草稿
   * 已经在编辑中，此时共享状态（基线 / 状态字 / `_isNew`）都不该再被它改写——基线被旧
   * 草稿覆盖会让新草稿被误判成"有改动"，凭空写一次刚新建、用户还没碰过的任务。列表刷新
   * 与失败提示照旧保留。
   */
  async function run(draft: TDraft, detached: boolean): Promise<void> {
    if (spec.blockReasonOf(draft) !== null) {
      // 闸口拦下：不报错、不打扰编辑（缺口由编辑页的提示条与状态字表达，用户正在逐项填）
      if (!detached) autosaveState.value = "idle";
      return;
    }
    const mark = spec.fingerprintOf(draft);
    if (mark === null || mark === lastSavedFingerprint) {
      // 载荷构造不出来，或与磁盘上那份完全一致（新建后未改 / 改动被改回原样）：不发请求
      if (!detached) autosaveState.value = "idle";
      return;
    }
    if (inFlight?.mark === mark) {
      // 同一份内容已经在路上：再发一遍只会拿到同一个结果（定时任务是 409 红字）
      return;
    }
    const mine = ++seq;
    inFlight = { seq: mine, mark };
    if (!detached) autosaveState.value = "saving";
    try {
      await spec.persist(draft);
      if (inFlight?.seq === mine) inFlight = null;
      // 落盘成功是**关于这份草稿的事实**（它现在就在磁盘上），与"由哪一发接管界面态"
      // 无关：被后一发顶掉的那一发若不算数，`_isNew` 就永远是 true，定时任务之后每次
      // 自动保存都会 POST 一个已存在的 id（409 卡死）。调用条件只保留"这份草稿还在
      // 编辑器里"——已被 clear / 切走的草稿，它的界面态不再属于任何人。
      if (!detached && spec.draft.value === draft) spec.onSaved?.(draft);
      if (mine !== seq) return; // 已有更新的变更接管了状态机
      if (!detached) {
        lastSavedFingerprint = mark;
        autosaveState.value = "saved";
      }
    } catch (error) {
      if (inFlight?.seq === mine) inFlight = null;
      const message = extractApiError(error, "自动保存失败");
      frontendLogger.error(spec.logScope, `自动保存失败: ${spec.idOf(draft)}`, error);
      if (detached) {
        // 换编辑对象那一发失败意味着用户刚改的内容没落盘。这一发的序号**通常**已经被
        // 新草稿的变更顶掉了，所以提示必须在序号判断之前发出去——否则静默等于偷偷丢掉。
        spec.toast(false, `${spec.detachedSubject}的改动未保存：${message}`);
        return;
      }
      if (mine !== seq) return;
      autosaveState.value = "error";
      spec.toast(false, message);
    }
  }

  async function saveNow(draft: TDraft): Promise<void> {
    stopTimer();
    pendingChanges = false;
    await run(draft, false);
  }

  async function flush(reason: "switch" | "close"): Promise<void> {
    stopTimer();
    const draft = spec.draft.value;
    if (!draft) return;
    // 判据不只看 `pendingChanges`：深度 watcher 是异步批处理的，"改字段"与"换对象"落在
    // 同一 tick 时它还来不及置位（程序化写入后立刻切走就会踩到）。载荷指纹与磁盘那份比对
    // 是**当下**的事实，不依赖 watcher 何时跑。
    if (!pendingChanges && spec.fingerprintOf(draft) === lastSavedFingerprint) return;
    pendingChanges = false;
    const blocked = spec.blockReasonOf(draft);
    if (blocked !== null) {
      // 闸口拦住了自动保存：关掉编辑器前明说，否则"退出即生效"在缺口态下变成静默丢弃
      if (reason === "close") spec.toast(false, blocked);
      return;
    }
    await run(draft, reason === "switch");
  }

  /**
   * 草稿深度监听：任何**真实**字段变更 → debounce 后静默落盘。
   *
   * 与基线一致的改动直接跳过——刚打开的草稿、新建后没动过的草稿、改了又改回原样的字段
   * 都不发请求（"点开又退出也在磁盘上留个文件"正是当初修掉的那类噪音）。闸口拦下的改动
   * 同样不发（语法非法 / 缺口未补齐），修正后由下一次变更自动补发。
   *
   * **换编辑对象不必在这里识别**：各入口（打开 / 新建 / 导入）都在赋值后立刻
   * `markBaseline` / `markUnsaved`，等这个 watcher 跑起来时基线已经对上了。
   */
  watch(
    spec.draft,
    () => {
      const draft = spec.draft.value;
      stopTimer();
      if (!draft) {
        pendingChanges = false;
        return;
      }
      if (spec.fingerprintOf(draft) === lastSavedFingerprint) {
        pendingChanges = false;
        return;
      }
      pendingChanges = true;
      timer = setTimeout(() => {
        timer = null;
        pendingChanges = false;
        const current = spec.draft.value;
        if (current) void run(current, false);
      }, AUTOSAVE_DEBOUNCE_MS);
    },
    { deep: true },
  );

  return {
    autosaveState,
    markBaseline,
    markUnsaved,
    saveNow,
    flush,
    clear,
  };
}
