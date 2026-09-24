/**
 * 定时任务草稿的纯函数测试：缺口判定与落盘载荷。
 *
 * 这两件事是自动保存的**判据**——缺口错了会在"发一个注定 400 的请求"与
 * "改动明明有效却永远不落盘"之间二选一；载荷错了则会静默改掉用户的调度语义
 * （比如给 cron 任务带上启动参数、或把超时钳成别的值还显示原值）。
 * 放在纯函数上测就不必挂 DOM，也更不容易假通过。
 */
import { describe, expect, it } from "vitest";
import type { ScheduledTask } from "../api/types";
import {
  STARTUP_FORM_LIMITS,
  TIMEOUT_LIMITS,
  clampStartupForm,
  clampTimeout,
  emptyScheduledDraft,
  newScheduledTaskId,
  parseCronToSchedule,
  scheduledDraftFromServer,
  scheduledDraftGaps,
  scheduledDraftPayload,
  scheduledTriggerLabel,
  scheduleToCron,
  switchDraftTargetKind,
  switchDraftTrigger,
  type ScheduledGapContext,
  type ScheduledTaskDraft,
} from "./scheduledDraft";

/** 缺口上下文：默认目标就绪、候选池含 browser1 / script1 */
function ctx(overrides: Partial<ScheduledGapContext> = {}): ScheduledGapContext {
  return { validTargetIds: ["browser1", "script1"], targetsLoaded: true, ...overrides };
}

/** 一份"可以直接落盘"的草稿（各用例按需破坏其中一项） */
function validDraft(overrides: Partial<ScheduledTaskDraft> = {}): ScheduledTaskDraft {
  return {
    ...emptyScheduledDraft(),
    name: "早八签到",
    target_id: "browser1",
    _isNew: false,
    ...overrides,
  };
}

function serverTask(overrides: Partial<ScheduledTask> = {}): ScheduledTask {
  return {
    id: "sched_1",
    name: "早八签到",
    description: "",
    task_type: "browser",
    target_id: "browser1",
    cron: "0 8 * * *",
    enabled: true,
    trigger: "cron",
    ...overrides,
  };
}

describe("parseCronToSchedule", () => {
  it("标准每日表达式解析出时分", () => {
    expect(parseCronToSchedule("30 8 * * *")).toEqual({ hour: 8, minute: 30, valid: true });
    expect(parseCronToSchedule("0 0 * * *")).toEqual({ hour: 0, minute: 0, valid: true });
  });

  it("表单表达不了的表达式一律判为无效（分时非纯数字 / 非每日 / 字段数不对）", () => {
    for (const cron of [
      "*/5 * * * *", // 步进
      "0 8-18 * * *", // 区间
      "0,30 8 * * *", // 列表
      "0 8 * * MON", // 星期限制：分时是纯数字，但只在一周里跑一次
      "0 8 1 * *", // 月内某天
      "0 8 * *", // 4 字段
      "0 0 8 * * *", // 6 字段（秒级口径）
      "",
      "abc",
      "not a cron at all",
    ]) {
      expect(parseCronToSchedule(cron).valid, cron).toBe(false);
    }
    expect(parseCronToSchedule("30 8 * * *").valid).toBe(true);
    expect(parseCronToSchedule("0 0 * * *").valid).toBe(true);
  });

  it("时分越界的纯数字也判为无效（回归：曾显示 99:99 并存成永不匹配的 cron）", () => {
    for (const cron of [
      "99 99 * * *",
      "60 8 * * *", // 分越界
      "0 24 * * *", // 时越界
    ]) {
      expect(parseCronToSchedule(cron).valid, cron).toBe(false);
    }
  });

  it("区间端点仍是有效表达式", () => {
    expect(parseCronToSchedule("0 0 * * *").valid).toBe(true);
    expect(parseCronToSchedule("59 23 * * *")).toEqual({ hour: 23, minute: 59, valid: true });
  });

  it("无效时回退 08:00（仅作表单展示初值，调用方必须提示覆盖后果）", () => {
    expect(parseCronToSchedule("*/5 * * * *")).toEqual({ hour: 8, minute: 0, valid: false });
  });

  it("scheduleToCron 生成 5 字段表达式", () => {
    expect(scheduleToCron(8, 30)).toBe("30 8 * * *");
    expect(scheduleToCron(0, 5)).toBe("5 0 * * *");
  });
});

describe("clampStartupForm / clampTimeout", () => {
  it("区间内原样通过", () => {
    expect(clampStartupForm({ max_runs_per_day: 2, max_retries: 3, startup_delay_secs: 45 })).toEqual({
      max_runs_per_day: 2,
      max_retries: 3,
      startup_delay_secs: 45,
    });
    expect(clampTimeout(120)).toBe(120);
  });

  it("NaN 回退缺省值", () => {
    expect(clampStartupForm({ max_runs_per_day: NaN, max_retries: NaN, startup_delay_secs: NaN })).toEqual({
      max_runs_per_day: STARTUP_FORM_LIMITS.maxRunsPerDay.fallback,
      max_retries: STARTUP_FORM_LIMITS.maxRetries.fallback,
      startup_delay_secs: STARTUP_FORM_LIMITS.startupDelaySecs.fallback,
    });
    expect(clampTimeout(NaN)).toBe(TIMEOUT_LIMITS.fallback);
  });

  it("越界钳到区间边界", () => {
    expect(clampStartupForm({ max_runs_per_day: 0, max_retries: 999, startup_delay_secs: -5 })).toEqual({
      max_runs_per_day: STARTUP_FORM_LIMITS.maxRunsPerDay.min,
      max_retries: STARTUP_FORM_LIMITS.maxRetries.max,
      startup_delay_secs: STARTUP_FORM_LIMITS.startupDelaySecs.min,
    });
    expect(clampStartupForm({ max_runs_per_day: 1000, max_retries: 99, startup_delay_secs: 999999 })).toEqual({
      max_runs_per_day: STARTUP_FORM_LIMITS.maxRunsPerDay.max,
      max_retries: STARTUP_FORM_LIMITS.maxRetries.max,
      startup_delay_secs: STARTUP_FORM_LIMITS.startupDelaySecs.max,
    });
    expect(clampTimeout(0)).toBe(TIMEOUT_LIMITS.min);
    expect(clampTimeout(99999)).toBe(TIMEOUT_LIMITS.max);
  });

  it("小数四舍五入到整数（后端字段是整数）", () => {
    expect(clampStartupForm({ max_runs_per_day: 2.6, max_retries: 1.2, startup_delay_secs: 30.5 }).max_runs_per_day).toBe(3);
    expect(clampTimeout(60.6)).toBe(61);
  });
});

describe("草稿构造", () => {
  it("新任务的 ID 满足后端 ID 规则且带 sched 前缀", () => {
    const id = newScheduledTaskId();
    expect(id).toMatch(/^sched_[a-z0-9]+_[a-z0-9]{4}$/);
    // 连续生成不撞号
    expect(newScheduledTaskId()).not.toBe(id);
  });

  it("空草稿：定时执行 08:00、启用、新建态、默认参数取自后端缺省", () => {
    const draft = emptyScheduledDraft();
    expect(draft.name).toBe("");
    expect(draft.target_id).toBe("");
    expect(draft.enabled).toBe(true);
    expect(draft.trigger).toBe("cron");
    expect(draft.schedule).toEqual({ hour: 8, minute: 0 });
    expect(draft.timeout).toBe(TIMEOUT_LIMITS.fallback);
    expect(draft._isNew).toBe(true);
  });

  it("由列表任务构造：cron → 时:分，缺省字段回退", () => {
    const draft = scheduledDraftFromServer(serverTask({ cron: "30 6 * * *" }));
    expect(draft.schedule).toEqual({ hour: 6, minute: 30 });
    expect(draft._isNew).toBe(false);
    expect(draft._originalCronInvalid).toBe(false);
    expect(draft.timeout).toBe(TIMEOUT_LIMITS.fallback);
    expect(draft.max_retries).toBe(STARTUP_FORM_LIMITS.maxRetries.fallback);
  });

  it("非每日表达式：回退 08:00 并标记（编辑页据此明示「保存会改成每日」）", () => {
    const draft = scheduledDraftFromServer(serverTask({ cron: "*/5 * * * *" }));
    expect(draft.schedule).toEqual({ hour: 8, minute: 0 });
    expect(draft._originalCronInvalid).toBe(true);
    expect(draft._originalCron).toBe("*/5 * * * *");
  });

  it("启动触发：不带 cron，时分为中性初值，启动参数取自任务", () => {
    const draft = scheduledDraftFromServer(
      serverTask({ trigger: "startup", cron: "", max_runs_per_day: 3, max_retries: 1, startup_delay_secs: 15 }),
    );
    expect(draft.trigger).toBe("startup");
    expect(draft._originalCronInvalid).toBe(false);
    expect(draft.max_runs_per_day).toBe(3);
    expect(draft.startup_delay_secs).toBe(15);
  });

  it("未知 task_type 归浏览器任务（后端会在目标被删后回退，取值不可信）", () => {
    expect(scheduledDraftFromServer(serverTask({ task_type: "weird" })).task_type).toBe("browser");
    expect(scheduledDraftFromServer(serverTask({ task_type: "script" })).task_type).toBe("script");
  });

  it("切换目标类型会清空已选目标（否则会存成一个跨类型的死引用）", () => {
    const draft = validDraft();
    switchDraftTargetKind(draft, "script");
    expect(draft.task_type).toBe("script");
    expect(draft.target_id).toBe("");
    // 同类型再切不回清
    draft.target_id = "script1";
    switchDraftTargetKind(draft, "script");
    expect(draft.target_id).toBe("script1");
  });

  it("切换触发方式收窄为合法枚举", () => {
    const draft = validDraft();
    switchDraftTrigger(draft, "startup");
    expect(draft.trigger).toBe("startup");
    switchDraftTrigger(draft, "hourly");
    expect(draft.trigger).toBe("cron");
  });
});

describe("列表「触发」列文案", () => {
  it("每日表达式渲染成「每天 HH:MM」（后端存的是 5 字段 cron，直接铺出来读不出人话）", () => {
    expect(scheduledTriggerLabel({ trigger: "cron", cron: "0 8 * * *" })).toBe("每天 08:00");
    expect(scheduledTriggerLabel({ trigger: "cron", cron: "5 22 * * *" })).toBe("每天 22:05");
  });

  it("表达不了的表达式原样显示（它正是用户要去改它的理由）", () => {
    expect(scheduledTriggerLabel({ trigger: "cron", cron: "*/5 * * * *" })).toBe("*/5 * * * *");
    expect(scheduledTriggerLabel({ trigger: "cron", cron: "0 8 * * MON" })).toBe("0 8 * * MON");
    expect(scheduledTriggerLabel({ trigger: "cron", cron: "" })).toBe("（无表达式）");
    expect(scheduledTriggerLabel({ trigger: "cron" })).toBe("（无表达式）");
  });

  it("启动触发不看表达式", () => {
    expect(scheduledTriggerLabel({ trigger: "startup", cron: "" })).toBe("启动后执行");
    expect(scheduledTriggerLabel({ trigger: "startup", cron: "0 8 * * *" })).toBe("启动后执行");
  });
});

describe("缺口判定", () => {
  it("齐备的草稿没有缺口", () => {
    expect(scheduledDraftGaps(validDraft(), ctx())).toEqual([]);
  });

  it("名称与目标为空各自成缺口", () => {
    expect(scheduledDraftGaps(validDraft({ name: "   " }), ctx())).toEqual(["任务名称"]);
    expect(scheduledDraftGaps(validDraft({ target_id: "" }), ctx())).toEqual(["目标任务"]);
  });

  it("目标不在候选池里算缺口；未就绪时不判定（冷启动深链不该被说成「已被删除」）", () => {
    expect(scheduledDraftGaps(validDraft({ target_id: "ghost" }), ctx())).toEqual([
      "有效的目标任务（原目标已不存在）",
    ]);
    expect(scheduledDraftGaps(validDraft({ target_id: "ghost" }), ctx({ targetsLoaded: false }))).toEqual([]);
  });

  it("超时越界算缺口而不是静默钳制（静默钳制会让界面值与磁盘值不一致）", () => {
    expect(scheduledDraftGaps(validDraft({ timeout: 1 }), ctx())).toEqual([`超时（${TIMEOUT_LIMITS.min}–${TIMEOUT_LIMITS.max} 秒）`]);
    expect(scheduledDraftGaps(validDraft({ timeout: NaN }), ctx())).toEqual([`超时（${TIMEOUT_LIMITS.min}–${TIMEOUT_LIMITS.max} 秒）`]);
  });

  it("执行时间越界算缺口（cron 触发才判）", () => {
    expect(scheduledDraftGaps(validDraft({ schedule: { hour: 24, minute: 0 } }), ctx())).toEqual(["执行时间"]);
    expect(scheduledDraftGaps(validDraft({ schedule: { hour: 9, minute: 90 } }), ctx())).toEqual(["执行时间"]);
    // 启动触发不看时刻
    expect(scheduledDraftGaps(validDraft({ trigger: "startup", schedule: { hour: 99, minute: 0 } }), ctx())).toEqual([]);
  });

  it("启动参数越界各自成缺口，且只在启动触发下判", () => {
    const bad = validDraft({ trigger: "startup", max_runs_per_day: 0, max_retries: 99, startup_delay_secs: -1 });
    expect(scheduledDraftGaps(bad, ctx())).toEqual([
      `每天最多成功次数（${STARTUP_FORM_LIMITS.maxRunsPerDay.min}–${STARTUP_FORM_LIMITS.maxRunsPerDay.max}）`,
      `失败重试次数（${STARTUP_FORM_LIMITS.maxRetries.min}–${STARTUP_FORM_LIMITS.maxRetries.max}）`,
      `延迟执行秒数（${STARTUP_FORM_LIMITS.startupDelaySecs.min}–${STARTUP_FORM_LIMITS.startupDelaySecs.max}）`,
    ]);
    // cron 触发下这三个值不参与判定（它们对 cron 无意义）
    const cronDraft = validDraft({ max_runs_per_day: 0, max_retries: 99, startup_delay_secs: -1 });
    expect(scheduledDraftGaps(cronDraft, ctx())).toEqual([]);
  });
});

describe("落盘载荷", () => {
  it("cron 触发：发 cron 表达式，不带启动参数", () => {
    const payload = scheduledDraftPayload(validDraft({ schedule: { hour: 7, minute: 5 }, timeout: 90 }));
    expect(payload).toEqual({
      name: "早八签到",
      description: "",
      target_id: "browser1",
      cron: "5 7 * * *",
      enabled: true,
      timeout: 90,
      trigger: "cron",
    });
    expect(payload.max_runs_per_day).toBeUndefined();
  });

  it("启动触发：cron 发空串，携带钳制后的启动参数", () => {
    const payload = scheduledDraftPayload(
      validDraft({ trigger: "startup", max_runs_per_day: 2, max_retries: 3, startup_delay_secs: 45 }),
    );
    expect(payload.cron).toBe("");
    expect(payload.trigger).toBe("startup");
    expect(payload.max_runs_per_day).toBe(2);
    expect(payload.max_retries).toBe(3);
    expect(payload.startup_delay_secs).toBe(45);
  });

  it("名称去首尾空格、描述原样保留（描述里的排版是用户的）", () => {
    const payload = scheduledDraftPayload(validDraft({ name: "  早八签到  ", description: "  两个空格 内  " }));
    expect(payload.name).toBe("早八签到");
    expect(payload.description).toBe("  两个空格 内  ");
  });

  it("载荷里的数字即使越界也已钳制（缺口是第一道闸，这是最后一道）", () => {
    const payload = scheduledDraftPayload(validDraft({ trigger: "startup", timeout: 1, max_runs_per_day: 999 }));
    expect(payload.timeout).toBe(TIMEOUT_LIMITS.min);
    expect(payload.max_runs_per_day).toBe(STARTUP_FORM_LIMITS.maxRunsPerDay.max);
  });

  it("载荷不含界面态字段（_isNew / _originalCron 只属于编辑期）", () => {
    const payload = scheduledDraftPayload(validDraft()) as Record<string, unknown>;
    expect(payload._isNew).toBeUndefined();
    expect(payload._originalCron).toBeUndefined();
    expect(payload._originalSchedule).toBeUndefined();
    expect(payload.id).toBeUndefined();
    expect(payload.task_type).toBeUndefined();
  });

  it("表达不了的表达式：只改别处时原样带回，不再被静默改成每日", () => {
    // `0 8 * * MON`：只在周一跑。载入时表单回退 08:00 并标 _originalCronInvalid
    const draft = scheduledDraftFromServer(serverTask({ cron: "0 8 * * MON" }));
    expect(draft._originalCronInvalid).toBe(true);

    // 用户只改了名称（时间控件没碰）：调度语义必须一个字都不变
    draft.name = "改个名字";
    expect(scheduledDraftPayload(draft).cron).toBe("0 8 * * MON");

    // 目标、启用、超时同理
    draft.target_id = "browser1";
    draft.timeout = 90;
    expect(scheduledDraftPayload(draft).cron).toBe("0 8 * * MON");
  });

  it("表达不了的表达式：动过时间控件才按表单值改写", () => {
    const draft = scheduledDraftFromServer(serverTask({ cron: "0 8 * * MON" }));
    // 表单回退值是 08:00：把它改成 09:30 就是用户的明确选择
    draft.schedule = { hour: 9, minute: 30 };
    expect(scheduledDraftPayload(draft).cron).toBe("30 9 * * *");
  });

  it("每日表达式不受影响：仍按表单值落盘", () => {
    const draft = scheduledDraftFromServer(serverTask({ cron: "0 8 * * *" }));
    expect(draft._originalCronInvalid).toBe(false);
    draft.name = "改个名字";
    draft.schedule = { hour: 6, minute: 15 };
    expect(scheduledDraftPayload(draft).cron).toBe("15 6 * * *");
  });
});
