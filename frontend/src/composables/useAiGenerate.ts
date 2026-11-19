/**
 * AI 任务页第 3 步：生成编排（SSE 流式增量回显 / 空闲超时 / 取消 / 阶段进度）
 * 与任务 JSON 落盘保存（危险步骤确认 → tasksApi 导入 → 跳转浏览器任务）。
 *
 * 自 AiTaskView.vue 按职责拆出（设计系统收口 T8-2）：本模块只管「生成与保存」；
 * LLM 配置草稿见 useAiLlmConfig，页面捕获见 useAiCapture。
 * 前置状态（isCaptureDone / isConfigDone）与当前模型配置由调用方以 ref 显式注入，
 * 路由跳转经 goToBrowserTasks 回调注入——本模块不感知 router，便于脱离组件单测。
 */
import { computed, ref, unref } from "vue";
import type { Ref } from "vue";
import type { SelectOption } from "../components/common/CustomSelect.vue";
import type { AiGenerateResult, AiStreamEvent } from "../api/types";
import type { ConfirmOptions } from "./useConfirm";
import { aiApi, tasksApi } from "../api";
import { extractApiError } from "../api/client";

const streamIdleOptions: SelectOption[] = [
  { label: "2 分钟", value: "120000" },
  { label: "5 分钟", value: "300000" },
  { label: "10 分钟（默认）", value: "600000" },
];

/**
 * 从模型自由文本里抠出任务 JSON：整段直解 → 首尾大括号截取 → Markdown 围栏内截取。
 * 纯函数（导出供单测直测），解析不出即 null（保留原文让用户手动处理）。
 */
export function extractJsonFromText(text: string): Record<string, unknown> | null {
  const t = text.trim();
  const tryParse = (s: string): Record<string, unknown> | null => {
    try { const v = JSON.parse(s) as Record<string, unknown>; return v && typeof v === "object" ? v : null; } catch { return null; }
  };
  const direct = tryParse(t);
  if (direct) return direct;
  const start = t.indexOf("{");
  const end = t.lastIndexOf("}");
  if (start !== -1 && end > start) {
    const cand = tryParse(t.slice(start, end + 1));
    if (cand) return cand;
  }
  if (t.startsWith("```")) {
    const after = t.slice(3).split("\n").slice(1).join("\n");
    const fenceEnd = after.lastIndexOf("```");
    const inner = fenceEnd !== -1 ? after.slice(0, fenceEnd) : after;
    const parsed = tryParse(inner.trim());
    if (parsed) return parsed;
  }
  return null;
}

export interface UseAiGenerateOptions {
  /** 第 2 步捕获是否已完成（生成前置校验） */
  isCaptureDone: Ref<boolean>;
  /** 第 1 步配置是否已保存无改动（生成前置校验） */
  isConfigDone: Ref<boolean>;
  /** 当前表单模型/地址：生成结果回显的兜底值（后端 started 帧到达时被覆盖） */
  model: Ref<string>;
  baseUrl: Ref<string>;
  /** 提示回调（成功/失败 toast，与视图共用同一单例语义） */
  toastOnly: (success: boolean, message: string) => void;
  /** 危险步骤确认框（useConfirm.confirm：false/null 一律按取消处理） */
  confirm: (options: ConfirmOptions) => Promise<boolean | null>;
  /** 保存成功后的跳转回调（路由细节留在视图层） */
  goToBrowserTasks: () => void;
}

/** AI 任务页第 3 步的生成编排与任务保存；入参显式、无模块级可变状态 */
export function useAiGenerate(options: UseAiGenerateOptions) {
  const { isCaptureDone, isConfigDone, model, baseUrl, toastOnly, confirm, goToBrowserTasks } = options;

  const extraPrompt = ref("");
  const generating = ref(false);
  const generateResult = ref<AiGenerateResult | null>(null);
  const taskJson = ref("");
  const jsonError = ref("");
  const savingTask = ref(false);
  const streamText = ref("");
  const streamEvents = ref<AiStreamEvent[]>([]);
  const streamPhase = ref("");
  const streamIdleMs = ref(600_000);
  const streamIdleValue = computed({
    get: () => String(streamIdleMs.value),
    set: (v: string) => { streamIdleMs.value = Number(v) || 600_000; },
  });
  const isGenerateDone = computed(() => !!generateResult.value);
  let streamAbort: AbortController | null = null;
  // 底部进度条的自动收起计时器（done/error 后延迟清空，避免 sticky 条永久残留）
  let streamPhaseTimer: ReturnType<typeof setTimeout> | null = null;

  function pushStreamEvent(ev: AiStreamEvent): void {
    // 芯片行只保留有意义的状态，去重连续 delta
    const last = streamEvents.value[streamEvents.value.length - 1];
    if (ev.type === "delta" && last && last.type === "delta") return;
    streamEvents.value.push(ev);
    if (streamEvents.value.length > 40) streamEvents.value.splice(0, streamEvents.value.length - 40);
    if (ev.type === "started") streamPhase.value = "模型已开始输出…";
    else if (ev.type === "attempt_start") {
      // 新一轮尝试 = 上一轮残文不再属于最终结果，清空预览避免多轮输出拼接
      streamText.value = "";
      streamPhase.value = ev.attempt ? `第 ${ev.attempt}/${ev.max} 轮生成中…` : "生成中…";
    } else if (ev.type === "validation_failed") streamPhase.value = "校验未通过，准备重试…";
    else if (ev.type === "retrying") streamPhase.value = "正在重试…";
    else if (ev.type === "validated") streamPhase.value = "校验通过，收尾中…";
    else if (ev.type === "done") dismissStreamPhaseLater("生成完成");
    else if (ev.type === "error") dismissStreamPhaseLater("生成失败");
    requestAnimationFrame(() => {
      const el = document.getElementById("ai-stream-preview");
      if (el) el.scrollTop = el.scrollHeight;
    });
  }

  /** 终态阶段文案延迟自动收起：sticky 进度条不应在结束后永久停留 */
  function dismissStreamPhaseLater(finalText: string): void {
    streamPhase.value = finalText;
    if (streamPhaseTimer) clearTimeout(streamPhaseTimer);
    streamPhaseTimer = setTimeout(() => {
      if (streamPhase.value === finalText) streamPhase.value = "";
      streamPhaseTimer = null;
    }, 6000);
  }

  async function generate(): Promise<void> {
    if (!isCaptureDone.value) {
      toastOnly(false, "请先在第 2 步完成页面捕获");
      return;
    }
    if (!isConfigDone.value) {
      toastOnly(false, "模型配置有改动，请先保存第 1 步");
      return;
    }
    generating.value = true;
    generateResult.value = null;
    taskJson.value = "";
    jsonError.value = "";
    streamText.value = "";
    streamEvents.value = [];
    streamPhase.value = "连接中…";
    const abort = new AbortController();
    streamAbort = abort;
    try {
      let lastError: string | null = null;
      let startedModel = model.value;
      let startedBaseUrl = baseUrl.value;
      await aiApi.generateStream(
        { extra_prompt: extraPrompt.value || undefined },
        {
          signal: abort.signal,
          idleTimeoutMs: streamIdleMs.value,
          onEvent: (ev) => {
            // 帧形态见 AiStreamEvent 判别联合（types.ts）：按 type 收窄后直读字段
            if (ev.type === "started") {
              startedModel = ev.model;
              startedBaseUrl = ev.base_url;
            }
            if (ev.type === "delta") {
              streamText.value += ev.text;
              // 轻量心跳，不堆芯片，仅在需要时更新阶段提示
              if (streamText.value.length % 400 < ev.text.length) {
                streamPhase.value = `生成中… ${streamText.value.length} 字符`;
              }
              return;
            }
            if (ev.type === "done") {
              generateResult.value = {
                task: ev.task,
                attempts: ev.attempts,
                warnings: ev.warnings,
                model: startedModel,
                base_url: startedBaseUrl,
              };
              taskJson.value = JSON.stringify(generateResult.value.task, null, 2);
              pushStreamEvent(ev);
              return;
            }
            if (ev.type === "error") {
              lastError = ev.message;
              pushStreamEvent(ev);
              return;
            }
            pushStreamEvent(ev);
          },
        },
      );
      // 终判经 unref 读取：await 期间闭包对 generateResult 的写入不参与外层控制流
      // 收窄，直接读 .value 会被此前置空动作钉死成 null（可选链非空分支随之成 never）
      const done = unref(generateResult);
      if (done?.task && Object.keys(done.task).length) {
        toastOnly(true, `任务生成成功（第 ${done.attempts} 轮通过校验）`);
      } else if (lastError) {
        throw new Error(lastError);
      } else if (!taskJson.value && streamText.value.trim()) {
        const cand = extractJsonFromText(streamText.value);
        if (cand) {
          taskJson.value = JSON.stringify(cand, null, 2);
          toastOnly(true, "已收到模型输出（流式），请检查下方 JSON 后保存");
        }
      }
    } catch (error) {
      if ((error as DOMException)?.name === "AbortError") {
        streamPhase.value = "已取消";
        toastOnly(false, "已取消生成");
      } else {
        streamPhase.value = "生成失败";
        toastOnly(false, extractApiError(error, "任务生成失败"));
      }
    } finally {
      generating.value = false;
      streamAbort = null;
    }
  }

  function cancelGenerate(): void {
    streamAbort?.abort();
  }

  function copyStream(): void {
    if (!streamText.value) return;
    navigator.clipboard.writeText(streamText.value).then(
      () => toastOnly(true, "已复制流式输出"),
      () => toastOnly(false, "复制失败"),
    );
  }

  function clearStream(): void {
    streamText.value = "";
    streamEvents.value = [];
    if (!generating.value) streamPhase.value = "";
  }

  async function saveTask(): Promise<void> {
    if (!taskJson.value.trim()) return;
    try {
      const task = JSON.parse(taskJson.value) as Record<string, unknown>;
      const steps = Array.isArray(task.steps) ? task.steps as Array<Record<string, unknown>> : [];
      if (steps.some((step) => step.type === "upload_file")) {
        toastOnly(false, "AI 任务不允许包含上传本机文件步骤");
        return;
      }
      const dangerous = steps.filter((step) => ["eval", "custom_js", "evaluate"].includes(String(step.type)));
      if (dangerous.length > 0) {
        const ok = await confirm({
          title: "检测到执行脚本步骤",
          message: `任务包含 ${dangerous.length} 个会在登录页执行 JavaScript 的步骤。请确认页面来源可信后再保存。`,
          confirmText: "仍然保存",
          danger: true,
        });
        if (!ok) return;
      }
      delete task.id;
      delete task.source;
      delete task.version;
      // crypto.randomUUID 仅在安全上下文（HTTPS / localhost）存在，局域网 IP 访问时是
      // undefined——兜底用时间戳 + 随机后缀，避免保存时直接抛 TypeError
      const uuid = crypto.randomUUID?.() ?? `${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 10)}`;
      task.task_id = `ai-${uuid}`;
      task.type = "browser";
      task.url = "{{LOGIN_URL}}";
      savingTask.value = true;
      const r = await tasksImport(task);
      if (r.failed && (r.failed as unknown[]).length > 0) {
        toastOnly(false, `任务校验未通过：${JSON.stringify(r.failed[0])}`);
        return;
      }
      toastOnly(true, "任务已保存，已在「浏览器任务」中显示");
      // 生成完直接落到浏览器任务 Tab：AI 页与任务列表同属 /tasks 区域，
      // 留在原处会让用户看不到刚保存的成果
      goToBrowserTasks();
    } catch (error) {
      if (error instanceof SyntaxError) {
        toastOnly(false, "任务 JSON 格式有误，请检查后重试");
        return;
      }
      toastOnly(false, extractApiError(error, "保存任务失败"));
    } finally {
      savingTask.value = false;
    }
  }

  // @/api 已在顶部静态导入（aiApi 等），此处动态导入无代码分割意义，改用静态成员
  async function tasksImport(task: Record<string, unknown>): Promise<{ failed?: unknown[] }> {
    return (await tasksApi.import({ tasks: [task] })) as { failed?: unknown[] };
  }

  function formatJson(): void {
    try {
      const obj = JSON.parse(taskJson.value) as Record<string, unknown>;
      taskJson.value = JSON.stringify(obj, null, 2);
      jsonError.value = "";
    } catch (e) {
      jsonError.value = (e as Error).message;
    }
  }

  /** 组件卸载时清理（原 AiTaskView onUnmounted 逻辑）：中止流式请求 + 收起计时器 */
  function dispose(): void {
    // 离开页面即中止流式请求：后端经断连检测取消 LLM 调用，toast 也不再跨页弹出
    streamAbort?.abort();
    streamAbort = null;
    if (streamPhaseTimer) clearTimeout(streamPhaseTimer);
  }

  return {
    extraPrompt,
    generating,
    generateResult,
    taskJson,
    jsonError,
    savingTask,
    streamText,
    streamEvents,
    streamPhase,
    streamIdleMs,
    streamIdleOptions,
    streamIdleValue,
    isGenerateDone,
    generate,
    cancelGenerate,
    copyStream,
    clearStream,
    formatJson,
    saveTask,
    dispose,
  };
}

export type UseAiGenerateReturn = ReturnType<typeof useAiGenerate>;
