/**
 * useAiGenerate 单测：AI 任务页第 3 步生成编排与任务落盘的关键分支。
 * 覆盖：generate 前置校验（未捕获/未保存配置）、SSE 事件状态机翻转（started/attempt_start/
 * delta 心跳/validation_failed/retrying/validated/done/error）、重试轮次清空残文、
 * 芯片行 40 条上限、终态文案延迟收起、AbortError 取消语义、非 Abort 异常兜底、
 * 无 done 帧时流式兜底解析、结果回退当前表单模型、cancelGenerate/dispose 清理、
 * extractJsonFromText 三路解析、clearStream/copyStream/formatJson/streamIdleValue、
 * saveTask 全分支（空载荷/upload_file 拒绝/危险步骤确认/字段注入/校验失败/跳转）。
 */
import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { ref } from "vue";
import type { AiStreamEvent } from "../api/types";

const { aiApiMock, tasksApiMock, toastOnlyMock, confirmMock, navMock } = vi.hoisted(() => ({
  aiApiMock: { generateStream: vi.fn() },
  tasksApiMock: { import: vi.fn() },
  toastOnlyMock: vi.fn(),
  confirmMock: vi.fn(),
  navMock: vi.fn(),
}));

vi.mock("../api", () => ({ aiApi: aiApiMock, tasksApi: tasksApiMock }));

const { useAiGenerate, extractJsonFromText } = await import("./useAiGenerate");

function make(opts: { captureDone?: boolean; configDone?: boolean } = {}) {
  return useAiGenerate({
    isCaptureDone: ref(opts.captureDone ?? true),
    isConfigDone: ref(opts.configDone ?? true),
    model: ref("fallback-model"),
    baseUrl: ref("https://fallback.example.com"),
    toastOnly: toastOnlyMock,
    confirm: confirmMock,
    goToBrowserTasks: navMock,
  });
}

beforeEach(() => {
  vi.clearAllMocks();
  // node 环境无 rAF/DOM：桩掉 requestAnimationFrame 与 document，让 pushStreamEvent 的
  // 滚动回调无害执行（getElementById 返回 null 即跳过 scrollTop 赋值）
  vi.stubGlobal("requestAnimationFrame", vi.fn());
  vi.stubGlobal("document", { getElementById: vi.fn(() => null) });
  vi.useFakeTimers();
});

afterEach(() => {
  vi.unstubAllGlobals();
  vi.useRealTimers();
});

describe("generate 前置校验", () => {
  it("未完成捕获时拦截", async () => {
    const ai = make({ captureDone: false });
    await ai.generate();
    expect(toastOnlyMock).toHaveBeenCalledWith(false, "请先在第 2 步完成页面捕获");
    expect(aiApiMock.generateStream).not.toHaveBeenCalled();
  });

  it("配置有改动时拦截", async () => {
    const ai = make({ configDone: false });
    await ai.generate();
    expect(toastOnlyMock).toHaveBeenCalledWith(false, "模型配置有改动，请先保存第 1 步");
    expect(aiApiMock.generateStream).not.toHaveBeenCalled();
  });
});

describe("SSE 事件状态机", () => {
  it("全流程：started 覆盖结果模型、done 落结果与预览、芯片序列正确", async () => {
    aiApiMock.generateStream.mockImplementation(async (_p: unknown, opts: { onEvent: (ev: AiStreamEvent) => void }) => {
      opts.onEvent({ type: "started", model: "real-model", base_url: "https://real.example.com" });
      opts.onEvent({ type: "attempt_start", attempt: 1, max: 3 });
      opts.onEvent({ type: "delta", attempt: 1, text: "x".repeat(500) });
      opts.onEvent({ type: "attempt_delta_done", attempt: 1, text_len: 500 });
      opts.onEvent({ type: "validated", attempt: 1 });
      opts.onEvent({ type: "done", attempts: 1, warnings: ["w1"], task: { steps: [] } });
    });
    const ai = make();
    await ai.generate();
    expect(ai.streamText.value).toBe("x".repeat(500));
    expect(ai.streamEvents.value.map((e) => e.type)).toEqual([
      "started", "attempt_start", "attempt_delta_done", "validated", "done",
    ]);
    expect(ai.generateResult.value).toMatchObject({
      attempts: 1, warnings: ["w1"], model: "real-model", base_url: "https://real.example.com",
    });
    expect(JSON.parse(ai.taskJson.value)).toEqual({ steps: [] });
    expect(ai.isGenerateDone.value).toBe(true);
    expect(toastOnlyMock).toHaveBeenCalledWith(true, "任务生成成功（第 1 轮通过校验）");
    // done 帧：阶段文案进入终态，6s 后自动收起（sticky 进度条不残留）
    expect(ai.streamPhase.value).toBe("生成完成");
    await vi.advanceTimersByTimeAsync(6000);
    expect(ai.streamPhase.value).toBe("");
  });

  it("无 started 帧时结果回退当前表单模型与地址", async () => {
    aiApiMock.generateStream.mockImplementation(async (_p: unknown, opts: { onEvent: (ev: AiStreamEvent) => void }) => {
      opts.onEvent({ type: "done", attempts: 2, warnings: [], task: { steps: [{ type: "click" }] } });
    });
    const ai = make();
    await ai.generate();
    expect(ai.generateResult.value?.model).toBe("fallback-model");
    expect(ai.generateResult.value?.base_url).toBe("https://fallback.example.com");
    expect(toastOnlyMock).toHaveBeenCalledWith(true, "任务生成成功（第 2 轮通过校验）");
  });

  it("重试轮次：attempt_start 清空上一轮残文并更新轮次文案", async () => {
    aiApiMock.generateStream.mockImplementation(async (_p: unknown, opts: { onEvent: (ev: AiStreamEvent) => void }) => {
      opts.onEvent({ type: "attempt_start", attempt: 1, max: 3 });
      opts.onEvent({ type: "delta", attempt: 1, text: "first draft" });
      opts.onEvent({ type: "validation_failed", attempt: 1, errors: ["bad step"] });
      opts.onEvent({ type: "retrying", attempt: 1, next: 2 });
      opts.onEvent({ type: "attempt_start", attempt: 2, max: 3 });
    });
    const ai = make();
    await ai.generate();
    expect(ai.streamText.value).toBe("");
    expect(ai.streamPhase.value).toBe("第 2/3 轮生成中…");
    expect(ai.streamEvents.value.map((e) => e.type)).toEqual([
      "attempt_start", "validation_failed", "retrying", "attempt_start",
    ]);
    expect(ai.isGenerateDone.value).toBe(false);
    // 无 done/error 且残文已清空：无兜底解析、无成功提示
    expect(toastOnlyMock).not.toHaveBeenCalledWith(expect.stringContaining("生成成功"));
  });

  it("attempt 为 0 的 attempt_start 兜底为无轮次文案", async () => {
    aiApiMock.generateStream.mockImplementation(async (_p: unknown, opts: { onEvent: (ev: AiStreamEvent) => void }) => {
      opts.onEvent({ type: "attempt_start", attempt: 0, max: 0 });
    });
    const ai = make();
    await ai.generate();
    expect(ai.streamPhase.value).toBe("生成中…");
  });

  it("delta 心跳：400 字符边界才更新阶段提示，且 delta 不入芯片", async () => {
    const ai = make();
    const phases: string[] = [];
    aiApiMock.generateStream.mockImplementation(async (_p: unknown, opts: { onEvent: (ev: AiStreamEvent) => void }) => {
      opts.onEvent({ type: "delta", attempt: 1, text: "x".repeat(400) });
      phases.push(ai.streamPhase.value);
      opts.onEvent({ type: "delta", attempt: 1, text: "y" });
      phases.push(ai.streamPhase.value);
    });
    await ai.generate();
    expect(phases).toEqual(["生成中… 400 字符", "生成中… 400 字符"]);
    expect(ai.streamText.value).toBe("x".repeat(400) + "y");
    expect(ai.streamEvents.value).toEqual([]);
    // 非法文本无兜底解析：无任何提示
    expect(toastOnlyMock).not.toHaveBeenCalled();
  });

  it("error 终态帧：整体失败文案与失败提示", async () => {
    aiApiMock.generateStream.mockImplementation(async (_p: unknown, opts: { onEvent: (ev: AiStreamEvent) => void }) => {
      opts.onEvent({ type: "error", message: "LLM 超时" });
    });
    const ai = make();
    await ai.generate();
    expect(ai.streamPhase.value).toBe("生成失败");
    expect(toastOnlyMock).toHaveBeenCalledWith(false, "LLM 超时");
    expect(ai.isGenerateDone.value).toBe(false);
    expect(ai.generating.value).toBe(false);
  });

  it("芯片行只保留最近 40 条", async () => {
    aiApiMock.generateStream.mockImplementation(async (_p: unknown, opts: { onEvent: (ev: AiStreamEvent) => void }) => {
      for (let i = 0; i < 45; i++) opts.onEvent({ type: "attempt_delta_done", attempt: 1, text_len: i });
    });
    const ai = make();
    await ai.generate();
    expect(ai.streamEvents.value.length).toBe(40);
    expect(ai.streamEvents.value.every((e) => e.type === "attempt_delta_done")).toBe(true);
  });

  it("无 done 帧时从流式文本兜底解析 JSON", async () => {
    aiApiMock.generateStream.mockImplementation(async (_p: unknown, opts: { onEvent: (ev: AiStreamEvent) => void }) => {
      opts.onEvent({ type: "delta", attempt: 1, text: '前言 {"steps":[{"type":"click"}]} 后记' });
    });
    const ai = make();
    await ai.generate();
    expect(JSON.parse(ai.taskJson.value)).toEqual({ steps: [{ type: "click" }] });
    expect(toastOnlyMock).toHaveBeenCalledWith(true, "已收到模型输出（流式），请检查下方 JSON 后保存");
  });
});

describe("取消与清理", () => {
  function hangUntilAbort() {
    aiApiMock.generateStream.mockImplementation(
      (_p: unknown, opts: { signal: AbortSignal }) =>
        new Promise((_resolve, reject) => {
          opts.signal.addEventListener("abort", () => reject(new DOMException("aborted", "AbortError")));
        }),
    );
  }

  it("cancelGenerate 中止在途请求并进入已取消分支", async () => {
    hangUntilAbort();
    const ai = make();
    const pending = ai.generate();
    ai.cancelGenerate();
    await pending;
    expect(ai.streamPhase.value).toBe("已取消");
    expect(toastOnlyMock).toHaveBeenCalledWith(false, "已取消生成");
    expect(ai.generating.value).toBe(false);
  });

  it("dispose 中止在途请求，并清掉终态文案的收起计时器", async () => {
    hangUntilAbort();
    const ai = make();
    const pending = ai.generate();
    ai.dispose();
    await pending;
    expect(ai.streamPhase.value).toBe("已取消");
    // error 终态后 dispose：6s 后文案不被收起（计时器已被清掉）
    aiApiMock.generateStream.mockImplementation(async (_p: unknown, opts: { onEvent: (ev: AiStreamEvent) => void }) => {
      opts.onEvent({ type: "error", message: "LLM 超时" });
    });
    await ai.generate();
    expect(ai.streamPhase.value).toBe("生成失败");
    ai.dispose();
    await vi.advanceTimersByTimeAsync(6000);
    expect(ai.streamPhase.value).toBe("生成失败");
  });

  it("非 Abort 异常走统一失败兜底文案", async () => {
    aiApiMock.generateStream.mockRejectedValue("oops");
    const ai = make();
    await ai.generate();
    expect(ai.streamPhase.value).toBe("生成失败");
    expect(toastOnlyMock).toHaveBeenCalledWith(false, "任务生成失败");
    expect(ai.generating.value).toBe(false);
  });
});

describe("extractJsonFromText", () => {
  it("整段 JSON 直解", () => {
    expect(extractJsonFromText('{"a":1}')).toEqual({ a: 1 });
  });

  it("前后缀噪声中按首尾大括号截取", () => {
    expect(extractJsonFromText('说明 {"a":1} 结尾')).toEqual({ a: 1 });
  });

  it("markdown 围栏内截取（含未闭合围栏）", () => {
    expect(extractJsonFromText('```json\n{"a":1}\n```')).toEqual({ a: 1 });
    expect(extractJsonFromText('```\n{"a":1}')).toEqual({ a: 1 });
  });

  it("非对象 JSON 与纯文本返回 null", () => {
    expect(extractJsonFromText("5")).toBeNull();
    expect(extractJsonFromText("没有任何 JSON")).toBeNull();
  });
});

describe("流式辅助操作", () => {
  it("streamIdleValue 双向映射，非法输入回落默认 10 分钟", () => {
    const ai = make();
    ai.streamIdleValue.value = "120000";
    expect(ai.streamIdleMs.value).toBe(120_000);
    ai.streamIdleValue.value = "abc";
    expect(ai.streamIdleMs.value).toBe(600_000);
    expect(ai.streamIdleValue.value).toBe("600000");
  });

  it("clearStream：生成中保留阶段文案，空闲时一并清掉", async () => {
    aiApiMock.generateStream.mockImplementation(
      (_p: unknown, opts: { signal: AbortSignal }) =>
        new Promise((_resolve, reject) => {
          opts.signal.addEventListener("abort", () => reject(new DOMException("aborted", "AbortError")));
        }),
    );
    const ai = make();
    const pending = ai.generate();
    ai.streamText.value = "partial";
    ai.clearStream();
    expect(ai.streamText.value).toBe("");
    expect(ai.streamPhase.value).toBe("连接中…");
    ai.cancelGenerate();
    await pending;
    ai.clearStream();
    expect(ai.streamPhase.value).toBe("");
  });

  it("copyStream：空文本不触发，成功/失败分别提示", async () => {
    const writeText = vi.fn(() => Promise.resolve());
    vi.stubGlobal("navigator", { clipboard: { writeText } });
    const ai = make();
    ai.copyStream();
    expect(writeText).not.toHaveBeenCalled();
    ai.streamText.value = "abc";
    ai.copyStream();
    expect(writeText).toHaveBeenCalledWith("abc");
    await Promise.resolve();
    expect(toastOnlyMock).toHaveBeenCalledWith(true, "已复制流式输出");
    const failing = vi.fn(() => Promise.reject(new Error("denied")));
    vi.stubGlobal("navigator", { clipboard: { writeText: failing } });
    ai.copyStream();
    await Promise.resolve();
    expect(toastOnlyMock).toHaveBeenCalledWith(false, "复制失败");
  });

  it("formatJson：合法 JSON 重排并清错误，非法 JSON 记错误信息", () => {
    const ai = make();
    ai.taskJson.value = '{"a":1}';
    ai.formatJson();
    expect(ai.taskJson.value).toBe('{\n  "a": 1\n}');
    expect(ai.jsonError.value).toBe("");
    ai.taskJson.value = "{oops";
    ai.formatJson();
    expect(ai.jsonError.value).not.toBe("");
  });
});

describe("saveTask", () => {
  it("空载荷不触发导入", async () => {
    const ai = make();
    await ai.saveTask();
    expect(tasksApiMock.import).not.toHaveBeenCalled();
  });

  it("upload_file 步骤直接拒绝且不发导入", async () => {
    const ai = make();
    ai.taskJson.value = JSON.stringify({ steps: [{ type: "upload_file" }] });
    await ai.saveTask();
    expect(toastOnlyMock).toHaveBeenCalledWith(false, "AI 任务不允许包含上传本机文件步骤");
    expect(tasksApiMock.import).not.toHaveBeenCalled();
  });

  it("危险步骤：确认取消（含被抢占 null）不导入，确认后注入字段并跳转", async () => {
    const ai = make();
    ai.taskJson.value = JSON.stringify({ id: "old", source: "legacy", version: 2, steps: [{ type: "eval" }] });
    confirmMock.mockResolvedValue(false);
    await ai.saveTask();
    expect(confirmMock).toHaveBeenCalledWith({
      title: "检测到执行脚本步骤",
      message: expect.stringContaining("1 个"),
      confirmText: "仍然保存",
      danger: true,
    });
    expect(tasksApiMock.import).not.toHaveBeenCalled();
    confirmMock.mockResolvedValue(null);
    await ai.saveTask();
    expect(tasksApiMock.import).not.toHaveBeenCalled();
    confirmMock.mockResolvedValue(true);
    tasksApiMock.import.mockResolvedValue({ failed: [] });
    await ai.saveTask();
    const sent = tasksApiMock.import.mock.calls[0][0].tasks[0] as Record<string, unknown>;
    expect(sent.task_id).toMatch(/^ai-/);
    expect(sent.type).toBe("browser");
    expect(sent.url).toBe("{{LOGIN_URL}}");
    expect(sent.id).toBeUndefined();
    expect(sent.source).toBeUndefined();
    expect(sent.version).toBeUndefined();
    expect(toastOnlyMock).toHaveBeenCalledWith(true, "任务已保存，已在「浏览器任务」中显示");
    expect(navMock).toHaveBeenCalledTimes(1);
    expect(ai.savingTask.value).toBe(false);
  });

  it("导入校验失败：提示首条错误且不跳转", async () => {
    const ai = make();
    ai.taskJson.value = JSON.stringify({ steps: [] });
    tasksApiMock.import.mockResolvedValue({ failed: [{ reason: "bad step" }] });
    await ai.saveTask();
    expect(toastOnlyMock).toHaveBeenCalledWith(false, '任务校验未通过：{"reason":"bad step"}');
    expect(navMock).not.toHaveBeenCalled();
    expect(ai.savingTask.value).toBe(false);
  });

  it("任务 JSON 语法错误走专门提示", async () => {
    const ai = make();
    ai.taskJson.value = "{oops";
    await ai.saveTask();
    expect(toastOnlyMock).toHaveBeenCalledWith(false, "任务 JSON 格式有误，请检查后重试");
    expect(tasksApiMock.import).not.toHaveBeenCalled();
  });
});
