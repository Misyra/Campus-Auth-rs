/**
 * 定时任务草稿：自动保存模式下的编辑模型、缺口校验与落盘载荷（纯函数）。
 *
 * 弹窗时代是"填完点保存"，校验散在 `saveScheduledTask` 的三个 toast 分支里；改成
 * 「列表页 + 二级编辑页」后编辑模型与另外三个面板同构（改动 debounce 静默落盘、
 * 状态字 idle→saving→saved/error），于是校验必须在**发请求之前**可判定——
 * debounce 到点才打一个注定 400 的请求，用户只会看到一句莫名其妙的报错。
 * 故抽成 `scheduledDraftGaps`，与 `utils/autosave` 的状态字同一套口径：
 * **有缺口就拦住落盘并把状态字改口**，而不是让人以为已经存过。
 *
 * 本模块不碰 Vue、不碰网络：缺口与载荷都能被 vitest 直接盯住。
 */

import type { ScheduledTask, ScheduledTaskPayload } from "../api/types";
import { formatScheduleTime } from "./formatters";

/** 触发方式（与后端 `TaskTrigger` 的字符串口径一致） */
export type ScheduledTrigger = "cron" | "startup";

/** 目标类型：仅用于切换目标下拉（保存不上传，后端从 `target_id` 推导） */
export type ScheduledTargetKind = "browser" | "script";

/**
 * 编辑模型：定时任务字段 + 两种界面态（`_` 前缀 = 不进载荷）。
 *
 * 与磁盘字段的差异只有一处：磁盘上是 cron 表达式，编辑里是「时:分」两个数字
 * （表单表达不了步进、区间、列表这类表达式，如「每 5 分钟一次」，见 `_originalCron`）。
 */
export interface ScheduledTaskDraft {
  /** 任务 ID（新建时生成，界面上只读；改动它等于换一个任务） */
  id: string;
  name: string;
  description: string;
  /** 目标类型（切换时清空目标，见 `switchDraftTargetKind`） */
  task_type: ScheduledTargetKind;
  target_id: string;
  enabled: boolean;
  trigger: ScheduledTrigger;
  /** 每日执行时刻（cron 触发才用） */
  schedule: { hour: number; minute: number };
  timeout: number;
  /** 启动触发：每日成功次数上限 */
  max_runs_per_day: number;
  /** 启动触发：失败重试次数 */
  max_retries: number;
  /** 启动触发：延迟执行秒数 */
  startup_delay_secs: number;
  /** 新建但磁盘上还没有：首次落盘走 POST，之后走 PUT */
  _isNew: boolean;
  /** 载入时的原 cron 表达式（非每日格式时用于明示"保存会改写调度语义"） */
  _originalCron: string;
  /** 原表达式不是每日时间格式：表单已回退 08:00（见 `resolvedCron`） */
  _originalCronInvalid: boolean;
  /**
   * 载入时时间控件里的时分。
   *
   * 判据用途：`_originalCronInvalid` 为真时，只有**用户动过时间控件**才允许把调度改写成
   * 表单值；否则（只是改了名称、目标）必须把原表达式原样带回——否则"打开一个每周一跑的
   * 任务、改个名字"会把调度静默改成天天跑。
   */
  _originalSchedule: { hour: number; minute: number };
}

/** 启动触发字段的合法区间（与后端钳制口径一致） */
export const STARTUP_FORM_LIMITS = {
  maxRunsPerDay: { min: 1, max: 99, fallback: 1 },
  maxRetries: { min: 0, max: 10, fallback: 2 },
  startupDelaySecs: { min: 0, max: 86400, fallback: 30 },
} as const;

/** 执行超时的合法区间（秒）：与编辑页输入框的 min/max 同值 */
export const TIMEOUT_LIMITS = { min: 5, max: 3600, fallback: 60 } as const;

/** 每日执行时刻的合法区间 */
const HOUR_RANGE = { min: 0, max: 23 };
const MINUTE_RANGE = { min: 0, max: 59 };

/** 是否为有限数字（`v-model.number` 清空输入框会给出空串 → Number("") = 0，故须区分 NaN） */
function isFiniteNumber(raw: unknown): raw is number {
  return typeof raw === "number" && Number.isFinite(raw);
}

/** 区间判定：非数字一律视为越界（缺口拦下，不静默改成别的值） */
function within(raw: unknown, min: number, max: number): boolean {
  return isFiniteNumber(raw) && raw >= min && raw <= max;
}

/** 钳制启动触发表单值：NaN/越界回退缺省或区间边界（载荷的最后一道保险，缺口已先行拦截） */
export function clampStartupForm(input: {
  max_runs_per_day: number;
  max_retries: number;
  startup_delay_secs: number;
}): { max_runs_per_day: number; max_retries: number; startup_delay_secs: number } {
  const { maxRunsPerDay, maxRetries, startupDelaySecs } = STARTUP_FORM_LIMITS;
  const clamp = (raw: number, min: number, max: number, fallback: number): number => {
    const n = Math.round(Number(raw));
    if (!Number.isFinite(n)) return fallback;
    return Math.min(Math.max(n, min), max);
  };
  return {
    max_runs_per_day: clamp(input.max_runs_per_day, maxRunsPerDay.min, maxRunsPerDay.max, maxRunsPerDay.fallback),
    max_retries: clamp(input.max_retries, maxRetries.min, maxRetries.max, maxRetries.fallback),
    startup_delay_secs: clamp(
      input.startup_delay_secs,
      startupDelaySecs.min,
      startupDelaySecs.max,
      startupDelaySecs.fallback,
    ),
  };
}

/** 钳制超时秒数（NaN/越界回退缺省或区间边界） */
export function clampTimeout(raw: number): number {
  const n = Math.round(Number(raw));
  if (!Number.isFinite(n)) return TIMEOUT_LIMITS.fallback;
  return Math.min(Math.max(n, TIMEOUT_LIMITS.min), TIMEOUT_LIMITS.max);
}

/**
 * 从 5 字段 cron 表达式解析 hour 和 minute。
 *
 * `valid: false` 表示**这张表单表达不了这个表达式**，调用方据此明示"保存后会改成每日"
 * （见 `ScheduledTaskDraft._originalCronInvalid`）。四条判据都是必要的：
 * - 分/时必须是纯数字：`parseInt("8-18")` 会宽松解析成 8，把区间表达式"半解析成功"，
 *   调度语义已经变了却检测不到；
 * - 分/时必须落在真实取值区间内：`99 99 * * *` 是纯数字、也是每日，但 `99` 时 `99` 分
 *   不存在——此前只查"纯数字"会把这种表达式判为有效，表单显示 "99:99" 并原样存成
 *   一条永远匹配不上的 cron，任务从此静默不执行；
 * - 日期 / 月 / 星期三段必须是 `*`：`0 8 * * MON` 的分时是纯数字，但它只在一周里跑一次，
 *   按"每日 08:00"放过去就等于静默把调度改成天天跑；
 * - 字段数必须是 5（用户侧与存储侧的约定）：秒级/7 字段表达式同样落不进这张表单。
 * 前三条以前只查了第一条，缺失的两条都会让"保存"变成一次无声的调度改写。
 */
export function parseCronToSchedule(cron: string): { hour: number; minute: number; valid: boolean } {
  const parts = cron.trim().split(/\s+/);
  // 纯数字 + 落在区间内（`Number.isInteger` 顺带排除 "08" 这类前导零以外的怪值）
  const numberIn = (field: string | undefined, lo: number, hi: number): boolean => {
    if (!field || !/^\d+$/.test(field)) return false;
    const n = Number(field);
    return Number.isInteger(n) && n >= lo && n <= hi;
  };
  const everyValue = (field: string | undefined): boolean => field === "*";
  if (
    parts.length === 5 &&
    numberIn(parts[0], 0, 59) &&
    numberIn(parts[1], 0, 23) &&
    everyValue(parts[2]) &&
    everyValue(parts[3]) &&
    everyValue(parts[4])
  ) {
    return { hour: parseInt(parts[1], 10), minute: parseInt(parts[0], 10), valid: true };
  }
  return { hour: 8, minute: 0, valid: false };
}

/** 从 {hour, minute} 生成 5 字段 cron 表达式（用户侧与存储侧同为 5 字段） */
export function scheduleToCron(hour: number, minute: number): string {
  return `${minute} ${hour} * * *`;
}

/**
 * 新建任务的 ID。
 *
 * 定时任务的 ID 没有外部含义（不像脚本 ID 兼作文件名被引用），故由程序生成、界面上
 * 只读展示，用户不必为它起名。字母开头 + 时间戳 + 随机后缀：满足后端
 * `ScheduledTask::is_valid_id`（字母数字/下划线/连字符、不以 `.` 开头）并避免并发新建撞号。
 */
export function newScheduledTaskId(): string {
  return `sched_${Date.now().toString(36)}_${Math.random().toString(36).slice(2, 6)}`;
}

/** 空草稿（新建）：定时执行 08:00、超时 60 秒、启用，启动参数取后端缺省 */
export function emptyScheduledDraft(): ScheduledTaskDraft {
  return {
    id: newScheduledTaskId(),
    name: "",
    description: "",
    task_type: "browser",
    target_id: "",
    enabled: true,
    trigger: "cron",
    schedule: { hour: 8, minute: 0 },
    timeout: TIMEOUT_LIMITS.fallback,
    max_runs_per_day: STARTUP_FORM_LIMITS.maxRunsPerDay.fallback,
    max_retries: STARTUP_FORM_LIMITS.maxRetries.fallback,
    startup_delay_secs: STARTUP_FORM_LIMITS.startupDelaySecs.fallback,
    _isNew: true,
    _originalCron: "",
    _originalCronInvalid: false,
    // 新建草稿的"载入时时分"就是表单初值：`_originalCronInvalid` 为假时这条不参与判定
    _originalSchedule: { hour: 8, minute: 0 },
  };
}

/**
 * 由列表里的任务构造草稿。
 *
 * 任务字段全部来自列表响应（后端没有单任务 GET）：`cron` → 「时:分」，非每日表达式
 * 回退 08:00 并把 `_originalCronInvalid` 置真，由编辑页明示后果；`task_type` 只保留
 * browser/script 两值（后端会在目标被删后回退，故不信任其取值）。
 */
export function scheduledDraftFromServer(task: ScheduledTask): ScheduledTaskDraft {
  const isStartup = task.trigger === "startup";
  const cron = isStartup ? "" : task.cron || "";
  const schedule = parseCronToSchedule(cron);
  return {
    id: task.id,
    name: task.name || "",
    description: task.description || "",
    task_type: task.task_type === "script" ? "script" : "browser",
    target_id: task.target_id || "",
    enabled: task.enabled !== false,
    trigger: isStartup ? "startup" : "cron",
    // 只取时与分：`parseCronToSchedule` 的 valid 归 `_originalCronInvalid`，草稿的
    // schedule 就是表单里那两个数字（别把解析结果整个塞进来，会多带一个无关字段）
    schedule: isStartup ? { hour: 8, minute: 0 } : { hour: schedule.hour, minute: schedule.minute },
    timeout: task.timeout || TIMEOUT_LIMITS.fallback,
    max_runs_per_day: task.max_runs_per_day || STARTUP_FORM_LIMITS.maxRunsPerDay.fallback,
    max_retries: task.max_retries ?? STARTUP_FORM_LIMITS.maxRetries.fallback,
    startup_delay_secs: task.startup_delay_secs ?? STARTUP_FORM_LIMITS.startupDelaySecs.fallback,
    _isNew: false,
    _originalCron: cron,
    _originalCronInvalid: !isStartup && !schedule.valid,
    // 与上面 schedule 同值：判据是"用户有没有离开过这个值"，不是"这个值是多少"
    _originalSchedule: isStartup
      ? { hour: 8, minute: 0 }
      : { hour: schedule.hour, minute: schedule.minute },
  };
}

/** 切换目标类型：旧类型的目标 id 残留会被保存成另一类型的目标（或死引用），故清空 */
export function switchDraftTargetKind(draft: ScheduledTaskDraft, kind: ScheduledTargetKind): void {
  if (draft.task_type === kind) return;
  draft.task_type = kind;
  draft.target_id = "";
}

/** 切换触发方式：值收窄为合法枚举（CustomSelect 发的是 string） */
export function switchDraftTrigger(draft: ScheduledTaskDraft, trigger: string): void {
  draft.trigger = trigger === "startup" ? "startup" : "cron";
}

export interface ScheduledGapContext {
  /** 当前类型下可选的目标 id（来自任务目录） */
  validTargetIds: readonly string[];
  /**
   * 任务目录是否已拉取过。
   *
   * 未就绪时不判定"目标已不存在"——冷启动深链时目录本来就是空的，把"还没拉完"
   * 说成"目标已被删除"会让用户白改一遍（与 `useTaskEditorQuery` 同口径）。
   */
  targetsLoaded: boolean;
}

/**
 * 缺口列表（空数组 = 可以落盘）。
 *
 * 每项都是名词短语：编辑页的缺口条把它们拼成「还缺 X、Y，补齐前改动不会保存」。
 * 数字区间越界也计入缺口而**不静默钳制**——静默钳制会在"界面上显示 1、磁盘上是 5"
 * 之间留下一处说谎，而这正是状态字那条规矩要避免的。
 */
export function scheduledDraftGaps(
  draft: ScheduledTaskDraft,
  ctx: ScheduledGapContext,
): string[] {
  const gaps: string[] = [];
  if (!draft.name.trim()) gaps.push("任务名称");
  if (!draft.target_id) {
    gaps.push("目标任务");
  } else if (ctx.targetsLoaded && !ctx.validTargetIds.includes(draft.target_id)) {
    // 目标被删/改了 id：列表里仍留着这个 id，只有到触发时才失败，故保存前拦下
    gaps.push("有效的目标任务（原目标已不存在）");
  }
  if (!within(draft.timeout, TIMEOUT_LIMITS.min, TIMEOUT_LIMITS.max)) {
    gaps.push(`超时（${TIMEOUT_LIMITS.min}–${TIMEOUT_LIMITS.max} 秒）`);
  }
  if (draft.trigger === "cron") {
    if (
      !within(draft.schedule.hour, HOUR_RANGE.min, HOUR_RANGE.max) ||
      !within(draft.schedule.minute, MINUTE_RANGE.min, MINUTE_RANGE.max)
    ) {
      gaps.push("执行时间");
    }
  } else {
    const { maxRunsPerDay, maxRetries, startupDelaySecs } = STARTUP_FORM_LIMITS;
    if (!within(draft.max_runs_per_day, maxRunsPerDay.min, maxRunsPerDay.max)) {
      gaps.push(`每天最多成功次数（${maxRunsPerDay.min}–${maxRunsPerDay.max}）`);
    }
    if (!within(draft.max_retries, maxRetries.min, maxRetries.max)) {
      gaps.push(`失败重试次数（${maxRetries.min}–${maxRetries.max}）`);
    }
    if (!within(draft.startup_delay_secs, startupDelaySecs.min, startupDelaySecs.max)) {
      gaps.push(`延迟执行秒数（${startupDelaySecs.min}–${startupDelaySecs.max}）`);
    }
  }
  return gaps;
}

/**
 * 落盘载荷（PUT / POST 共用；`id` 由调用方按语义补上：POST 需要、PUT 不需要）。
 *
 * 口径与弹窗时代一致，两处都是**有意的**：
 * - `cron` 在启动触发下发空串（后端据此不解析表达式）；
 * - 启动参数只在启动触发时携带——PUT 是"出现即覆盖"，给 cron 任务带上这三个字段
 *   会把它们的值改掉（虽然对 cron 无意义，但那是无谓的副作用）。
 *
 * 返回类型用 `ScheduledTaskPayload` 而非 `Record<string, unknown>`：字段名/类型与
 * 前端声明的接口契约对齐，改名或漏字段在 typecheck 就暴露。
 */
export function scheduledDraftPayload(draft: ScheduledTaskDraft): ScheduledTaskPayload {
  const isStartup = draft.trigger === "startup";
  const payload: ScheduledTaskPayload = {
    name: draft.name.trim(),
    description: draft.description,
    target_id: draft.target_id,
    cron: isStartup ? "" : resolvedCron(draft),
    enabled: draft.enabled,
    timeout: clampTimeout(draft.timeout),
    trigger: draft.trigger,
  };
  if (isStartup) {
    Object.assign(
      payload,
      clampStartupForm({
        max_runs_per_day: draft.max_runs_per_day,
        max_retries: draft.max_retries,
        startup_delay_secs: draft.startup_delay_secs,
      }),
    );
  }
  return payload;
}

/**
 * 落盘用的 cron 表达式。
 *
 * 表单表达不了的表达式（如 `0 8 * * MON`：只在周一跑）在载入时会被回退成「每日 08:00」，
 * 此后只要用户改了**别的**字段就把它当表单值写回去 —— 那是一次无声的调度改写
 * （"只在周一跑"变成"天天跑"）。故：
 * - 原表达式表达不了 **且** 时间控件没被触碰 → 原样带回原表达式（改名称、改目标不动调度）；
 * - 动过时间控件（或本来就是每日表达式）→ 按表单值落盘，这正是用户刚刚做出的选择。
 *
 * 编辑页在载入时已经明示"保存后按上方时间改为每日执行"，所以这里的判据与文案一致。
 */
function resolvedCron(draft: ScheduledTaskDraft): string {
  const fromForm = scheduleToCron(draft.schedule.hour, draft.schedule.minute);
  if (draft._originalCronInvalid && sameSchedule(draft.schedule, draft._originalSchedule)) {
    return draft._originalCron;
  }
  return fromForm;
}

/** 时分是否与载入时一致（时间控件是否被动过） */
function sameSchedule(
  a: { hour: number; minute: number },
  b: { hour: number; minute: number },
): boolean {
  return a.hour === b.hour && a.minute === b.minute;
}

/** 载荷指纹（比对"有没有真改动"；不含 `_isNew` 等界面态，可作磁盘现状的代表） */
export function scheduledDraftFingerprint(draft: ScheduledTaskDraft): string {
  return JSON.stringify(scheduledDraftPayload(draft));
}

/**
 * 列表「触发」列的主行文案。
 *
 * 后端存的是 5 字段 cron（`0 8 * * *`），直接铺在列表里是"每天 0 8 * * *"这种读不出
 * 人话的样子；每日表达式按「每天 08:00」渲染，**表达不了的表达式原样显示**——那正是
 * 用户要去编辑页改它的理由（后端另有一行 `schedule_invalid` 标注解析失败的）。
 */
export function scheduledTriggerLabel(task: {
  trigger?: string | null;
  cron?: string | null;
}): string {
  if (task.trigger === "startup") return "启动后执行";
  const cron = (task.cron ?? "").trim();
  if (!cron) return "（无表达式）";
  const parsed = parseCronToSchedule(cron);
  // 复用 formatters 的同一份格式化（零填充口径只留一处）
  return parsed.valid ? `每天 ${formatScheduleTime(parsed)}` : cron;
}
