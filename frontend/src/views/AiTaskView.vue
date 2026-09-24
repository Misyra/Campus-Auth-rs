<script setup lang="ts">
/**
 * AI 生成任务页：捕获登录页面（截图 + HTML/JS）→ 交由用户配置的视觉 LLM
 * 生成任务 JSON → 校验回显 → 预览编辑后保存为任务。
 * 三段向导按序依赖：保存配置 → 捕获 → 生成 → 保存任务。
 * 流式模式：SSE 增量回显 + 底部同步进度条 + 可刷新空闲超时；后端总时长上限 10 分钟。
 */
import IconApp from "@/components/common/IconApp.vue";
import CustomSelect from "@/components/common/CustomSelect.vue";
import type { SelectOption } from "@/components/common/CustomSelect.vue";
import { aiApi, configApi, tasksApi } from "@/api";
import { extractApiError } from "@/api/client";
import type { AiCaptureResult, AiGenerateResult } from "@/api/types";
import { computed, onMounted, onUnmounted, ref, unref } from "vue";
import { useRouter } from "vue-router";
import { useToast } from "@/composables/useToast";
import { useConfirm } from "@/composables/useConfirm";
import { downloadBlob } from "@/utils/file";
import { fileStamp } from "@/utils/formatters";
import { TUTORIAL_VIDEO_URL } from "@/utils/constants";

const router = useRouter();
const { toastOnly } = useToast();
const { confirm } = useConfirm();

// ---- LLM 配置 ----
// 服务商列表。OpenCode Zen 渠道已下架：其免费档只对 OpenCode 官方客户端开放，
// 第三方客户端（含本程序）即使按官方形态发 `opencode/<version>` UA 与
// `x-opencode-*` 原生头，实测仍一律 403 FreeTierError，等于没有可用模型；
// 后端 `infer_provider` / `validate_provider` 已同步移除该标识，指向 opencode.ai 的
// 老配置在界面上回落为「自定义服务商」（自定义服务不校验域名）。
const PRESETS = [
  { id: "glm", label: "智谱 GLM", hint: "推荐使用视觉模型", base: "https://open.bigmodel.cn/api/paas/v4", model: "glm-5.3-flash", defaultKey: "" },
  { id: "deepseek", label: "DeepSeek", hint: "请填写支持图片的模型", base: "https://api.deepseek.com", model: "", defaultKey: "" },
  { id: "custom", label: "自定义服务商", hint: "需要知道接口地址", base: "", model: "", defaultKey: "" },
] as const;

/** 模型下拉框里的「自定义」项：选中后展开手动输入 */
const CUSTOM_MODEL = "__custom__";

const provider = ref<string>("glm");
const configuredProviders = ref<string[]>([]);
const baseUrl = ref("");
const model = ref("");
/** 服务商 `/models` 返回的模型列表（空 = 未拉取，此时按手动输入处理） */
const modelList = ref<string[]>([]);
/** 是否用手动输入：列表里没有想要的模型时用（拉取失败、私有模型、旧配置里的值） */
const customModel = ref(true);
const loadingModels = ref(false);
const apiKey = ref("");
/**
 * 服务端确认「已保存 Key」的槽位：内置服务商＝标识本身，自定义服务＝base_url 的
 * origin（后端 `LlmSettings::key_slot` 就是这么隔离自定义 Key 的）。
 *
 * 不用一个布尔量记"当前有没有 Key"：`configured_providers` 只报内置服务商，自定义
 * 槽位随地址走，切走再切回时必须能按"当前地址"重新对上，才不至于把已保存的 Key
 * 显示成"尚未保存"。
 */
const savedKeySlot = ref("");

/** API Key 槽位：自定义服务按地址（origin）区分，其余按服务商标识 */
function keySlotOf(id: string, url: string): string {
  if (id !== "custom") return id;
  try {
    const parsed = new URL(url.trim());
    return `${parsed.protocol}//${parsed.host}`;
  } catch {
    // 地址还没填/填得不成形：槽位未知，按"没有已保存 Key"处理
    return "";
  }
}

/** 当前槽位是否已有服务端保存的 Key（决定输入框占位与「清除当前 Key」按钮） */
const hasApiKey = computed(() => {
  if (PRESETS.find((item) => item.id === provider.value)?.defaultKey) return true;
  const slot = keySlotOf(provider.value, baseUrl.value);
  return !!slot && slot === savedKeySlot.value;
});
const maxTokens = ref("16384");
const maxTokenOptions: SelectOption[] = [
  { label: "16K（推荐）", value: "16384" },
  { label: "32K（复杂页面）", value: "32768" },
  { label: "由服务商决定", value: "auto" },
];
const savedSignature = ref("");
const configExpanded = ref(true);
const configSummary = computed(() => {
  if (!baseUrl.value && !model.value) return "未配置";
  return model.value || baseUrl.value;
});
const actualRequestUrl = computed(() => baseUrl.value.trim()
  ? `${baseUrl.value.trim().replace(/\/+$/, "")}/chat/completions`
  : "尚未填写");
/** 模型列表端点（「获取模型列表」实际请求的地址，显示给用户以便排障） */
const modelListUrl = computed(() => baseUrl.value.trim()
  ? `${baseUrl.value.trim().replace(/\/+$/, "")}/models`
  : "尚未填写");
/** 模型下拉项：服务商返回的列表 + 自定义项 */
const modelOptions = computed<SelectOption[]>(() => [
  ...modelList.value.map((id) => ({ label: id, value: id })),
  { label: "自定义（手动输入）", value: CUSTOM_MODEL },
]);
/** 下拉框选中值：当前模型不在列表里（或尚未拉取）时显示为「自定义」 */
const modelChoice = computed<string>({
  get: () => (!customModel.value && modelList.value.includes(model.value) ? model.value : CUSTOM_MODEL),
  set: (value: string) => {
    if (value === CUSTOM_MODEL) {
      customModel.value = true;
      return;
    }
    customModel.value = false;
    model.value = value;
  },
});
const savingConfig = ref(false);
const testingConfig = ref(false);
const configSignature = computed(() => JSON.stringify({
  provider: provider.value,
  base_url: baseUrl.value.trim(),
  model: model.value.trim(),
  max_tokens: maxTokens.value,
}));
const isConfigDone = computed(() => !!baseUrl.value.trim() && !!model.value.trim() && savedSignature.value === configSignature.value);
const configTestHint = computed(() => {
  if (!baseUrl.value.trim() || !model.value.trim()) return "请先填写接口地址和模型名称并保存";
  if (!savedSignature.value) return "请先保存模型配置，再测试连接";
  return "配置有改动，请重新保存后再测试连接";
});

/**
 * 各服务商上一次填写的草稿。
 *
 * 切换服务商只该切换"当前在编辑谁"，不该把已经填好的地址/模型/Key 丢掉：切换前先
 * 把当前表单存进草稿槽，切回时原样恢复。服务器只回 `has_api_key` 与
 * `configured_providers`（从不回 Key 明文），因此 Key 草稿仅活在本次会话内；地址与
 * 模型在 `loadConfig` 时把已保存值种进草稿，所以切走再切回拿到的是"上次保存 / 上次
 * 编辑"的状态，而不是预设的空值。
 */
interface ProviderDraft {
  baseUrl: string;
  model: string;
  apiKey: string;
  modelList: string[];
  customModel: boolean;
}
const drafts = new Map<string, ProviderDraft>();

/** 把当前表单写进当前服务商的草稿槽（切换前与保存后调用） */
function stashDraft(): void {
  // 空表单不入草稿：把"什么都没填"记成该服务商的状态，会让之后切回来拿到空值而不是
  // 预设默认值（配置读取失败、或用户压根没填过时就是这样）。清掉草稿即回落到预设。
  const empty =
    !baseUrl.value.trim() && !model.value.trim() && !apiKey.value.trim() && modelList.value.length === 0;
  if (empty) {
    drafts.delete(provider.value);
    return;
  }
  drafts.set(provider.value, {
    baseUrl: baseUrl.value,
    model: model.value,
    apiKey: apiKey.value,
    modelList: modelList.value,
    customModel: customModel.value,
  });
}

/** 套用预设默认值（该服务商还没有草稿时的回落） */
function applyPreset(id: string): void {
  const p = PRESETS.find((item) => item.id === id);
  baseUrl.value = p?.base ?? "";
  model.value = p?.model ?? "";
  apiKey.value = p?.defaultKey || "";
  // 模型列表按服务商拉取，换服务商即作废；没有列表时退回手动填写
  modelList.value = [];
  customModel.value = true;
}

function selectProvider(id: string): void {
  stashDraft();
  provider.value = id;
  const draft = drafts.get(id);
  if (draft) {
    baseUrl.value = draft.baseUrl;
    model.value = draft.model;
    apiKey.value = draft.apiKey;
    modelList.value = draft.modelList;
    customModel.value = draft.customModel;
  } else {
    applyPreset(id);
  }
  configExpanded.value = true;
}

/**
 * 拉取服务商 `/models` 列表填充下拉框。
 *
 * 未填 Key 时后端会回退到该服务商已保存的 Key，所以可以「选服务商 → 拉列表 →
 * 选模型 → 保存」，不必先存一次配置。拉取结果不会擅自改写当前模型：命中列表才切到
 * 下拉模式，否则保持手动输入（私有模型 / 旧配置里的值都可能不在列表里）。
 */
async function fetchModelList(): Promise<void> {
  if (!baseUrl.value.trim()) {
    toastOnly(false, "请先填写 Base URL");
    return;
  }
  loadingModels.value = true;
  try {
    const data = await aiApi.fetchModels({
      provider: provider.value,
      base_url: baseUrl.value.trim(),
      api_key: apiKey.value.trim() || undefined,
    });
    modelList.value = data.models || [];
    const inList = modelList.value.includes(model.value);
    customModel.value = !inList;
    const tail = inList || !model.value.trim()
      ? ""
      : `；当前模型不在列表中，仍按手动填写处理`;
    toastOnly(true, `已获取 ${modelList.value.length} 个模型${tail}`);
  } catch (error) {
    toastOnly(false, extractApiError(error, "获取模型列表失败"));
  } finally {
    loadingModels.value = false;
  }
}

async function loadConfig(): Promise<void> {
  try {
    const cfg = await aiApi.fetchLlmConfig();
    configuredProviders.value = cfg.configured_providers || [];
    if (!cfg.base_url && !cfg.model) {
      provider.value = "glm";
      applyPreset("glm");
      configExpanded.value = true;
      return;
    }
    baseUrl.value = cfg.base_url || "";
    model.value = cfg.model || "";
    provider.value = cfg.provider || "custom";
    // 已下架的渠道（如 opencode）在预设里没有对应卡片：按「自定义服务商」对待，
    // 否则界面上没有选中项、保存还会被后端的服务商标识校验拒绝
    if (!PRESETS.some((item) => item.id === provider.value)) provider.value = "custom";
    // Key 槽位按"界面认定的服务商 + 地址"记：自定义服务的 Key 存在 origin 槽里，
    // 切回来只要地址还是那个地址，就该重新显示成"已保存"
    savedKeySlot.value = cfg.has_api_key ? keySlotOf(provider.value, cfg.base_url || "") : "";
    maxTokens.value = cfg.max_tokens == null ? "auto" : String(cfg.max_tokens);
    savedSignature.value = configSignature.value;
    if (cfg.base_url && cfg.model) configExpanded.value = false;
    // 已保存的配置种进草稿：切到别的服务商再切回来，恢复的是这份值而不是预设空值
    stashDraft();
  } catch (error) {
    toastOnly(false, extractApiError(error, "读取 LLM 配置失败"));
  }
}

async function saveConfig(): Promise<void> {
  savingConfig.value = true;
  try {
    const payload: { provider: string; base_url: string; model: string; api_key?: string; max_tokens: number | null } = {
      provider: provider.value,
      base_url: baseUrl.value,
      model: model.value,
      max_tokens: maxTokens.value === "auto" ? null : Number(maxTokens.value),
    };
    if (apiKey.value.trim()) payload.api_key = apiKey.value.trim();
    const saved = await aiApi.saveLlmConfig(payload);
    savedKeySlot.value = saved.has_api_key ? keySlotOf(provider.value, baseUrl.value) : "";
    configuredProviders.value = saved.configured_providers || [];
    savedSignature.value = configSignature.value;
    apiKey.value = "";
    configExpanded.value = false;
    // 保存后刷新草稿：Key 输入框已清空，草稿跟着记成"无待保存 Key"
    stashDraft();
    toastOnly(true, "LLM 配置已保存");
  } catch (error) {
    toastOnly(false, extractApiError(error, "保存 LLM 配置失败"));
  } finally {
    savingConfig.value = false;
  }
}

async function clearApiKey(): Promise<void> {
  apiKey.value = "";
  savingConfig.value = true;
  try {
    const saved = await aiApi.saveLlmConfig({
      provider: provider.value,
      base_url: baseUrl.value,
      model: model.value,
      api_key: "",
      max_tokens: maxTokens.value === "auto" ? null : Number(maxTokens.value),
    });
    savedKeySlot.value = saved.has_api_key ? keySlotOf(provider.value, baseUrl.value) : "";
    configuredProviders.value = saved.configured_providers || [];
    savedSignature.value = configSignature.value;
    stashDraft();
    toastOnly(true, "当前服务商的 API Key 已清除");
  } catch (error) {
    toastOnly(false, extractApiError(error, "清除 API Key 失败"));
  } finally {
    savingConfig.value = false;
  }
}

async function testConnection(): Promise<void> {
  if (!isConfigDone.value) {
    toastOnly(false, "请先保存当前配置再测试连接");
    return;
  }
  testingConfig.value = true;
  try {
    const result = await aiApi.testLlmConfig();
    // note 存在表示"通了但回复被测试上限截断"（推理/话痨型模型的常见表现），一并显示
    toastOnly(true, `连接成功，耗时 ${result.latency_ms} ms${result.note ? `；${result.note}` : ""}`);
  } catch (error) {
    toastOnly(false, extractApiError(error, "连接测试失败"));
  } finally {
    testingConfig.value = false;
  }
}

// ---- 捕获 ----
const captureUrl = ref("");
const capturing = ref(false);
const captureResult = ref<AiCaptureResult | null>(null);
const screenshotUrl = ref("");
const savingBundle = ref(false);
const isCaptureDone = computed(() => !!captureResult.value);
/** 当前浏览器渠道与自定义渠道的引擎（决定 CDP 是否可用，进而决定捕获质量） */
const browserChannel = ref("");
const customBrowserEngine = ref("");
/**
 * 渠道是否落在非 Chromium 引擎上。
 *
 * MHTML 完整快照与 CDP 资源快照都只存在于 Chromium：firefox / webkit（含
 * custom 渠道配这两个引擎）下后端会跳过 MHTML，CSS/JS 改用页面枚举 + HTTP
 * 回补抓取。捕获仍可用，所以这里是提示而非禁用；文案与后端 note 保持一致口径。
 */
const captureChannelDegraded = computed(() => {
  const channel = browserChannel.value;
  const engine = channel === "custom" ? customBrowserEngine.value : channel;
  return engine === "firefox" || engine === "webkit";
});

/** 读取浏览器渠道：失败只少了这条提示，不影响捕获流程，静默忽略 */
async function loadBrowserChannel(): Promise<void> {
  try {
    const cfg = await configApi.fetch();
    browserChannel.value = cfg.browser?.browser_channel || "";
    customBrowserEngine.value = cfg.browser?.custom_browser_engine || "";
  } catch {
    browserChannel.value = "";
  }
}

async function capture(): Promise<void> {
  if (!captureUrl.value.trim()) {
    toastOnly(false, "请输入登录页地址");
    return;
  }
  capturing.value = true;
  captureResult.value = null;
  try {
    captureResult.value = await aiApi.capture(captureUrl.value.trim());
    screenshotUrl.value = aiApi.captureScreenshotUrl();
    toastOnly(true, "页面捕获完成");
  } catch (error) {
    toastOnly(false, extractApiError(error, "页面捕获失败"));
  } finally {
    capturing.value = false;
  }
}

async function saveBundle(): Promise<void> {
  if (savingBundle.value) return;
  savingBundle.value = true;
  try {
    const blob = await aiApi.captureBundle();
    const stamp = fileStamp();
    downloadBlob(blob, `campus-auth-capture-${stamp}.zip`, "application/zip");
    toastOnly(true, "页面文件已保存");
  } catch (error) {
    toastOnly(false, extractApiError(error as Error, "保存页面文件失败"));
  } finally {
    savingBundle.value = false;
  }
}

// ---- 生成（流式：有输出会重置前端空闲计时；后端总时长上限 10 分钟） ----
const extraPrompt = ref("");
const generating = ref(false);
const generateResult = ref<AiGenerateResult | null>(null);
const taskJson = ref("");
const jsonError = ref("");
const savingTask = ref(false);
const streamText = ref("");
const streamEvents = ref<Array<Record<string, unknown>>>([]);
const streamPhase = ref("");
const streamIdleMs = ref(600_000);
const streamIdleOptions: SelectOption[] = [
  { label: "2 分钟", value: "120000" },
  { label: "5 分钟", value: "300000" },
  { label: "10 分钟（默认）", value: "600000" },
];
const streamIdleValue = computed({
  get: () => String(streamIdleMs.value),
  set: (v: string) => { streamIdleMs.value = Number(v) || 600_000; },
});
const isGenerateDone = computed(() => !!generateResult.value);
let streamAbort: AbortController | null = null;
// 底部进度条的自动收起计时器（done/error 后延迟清空，避免 sticky 条永久残留）
let streamPhaseTimer: ReturnType<typeof setTimeout> | null = null;

function pushStreamEvent(ev: Record<string, unknown>): void {
  // 芯片行只保留有意义的状态，去重连续 delta
  const typ = String(ev.type ?? "");
  const last = streamEvents.value[streamEvents.value.length - 1];
  if (typ === "delta" && last && String(last.type) === "delta") return;
  streamEvents.value.push(ev);
  if (streamEvents.value.length > 40) streamEvents.value.splice(0, streamEvents.value.length - 40);
  if (typ === "started") streamPhase.value = "模型已开始输出…";
  else if (typ === "attempt_start") {
    // 新一轮尝试 = 上一轮残文不再属于最终结果，清空预览避免多轮输出拼接
    streamText.value = "";
    const a = ev.attempt as number | undefined;
    const m = ev.max as number | undefined;
    streamPhase.value = a ? `第 ${a}/${m ?? 2} 轮生成中…` : "生成中…";
  } else if (typ === "validation_failed") streamPhase.value = "校验未通过，准备重试…";
  else if (typ === "retrying") streamPhase.value = "正在重试…";
  else if (typ === "validated") streamPhase.value = "校验通过，收尾中…";
  else if (typ === "done") dismissStreamPhaseLater("生成完成");
  else if (typ === "error") dismissStreamPhaseLater("生成失败");
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
  if (!captureResult.value) {
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
          const typ = String((ev as Record<string, unknown>).type ?? "");
          if (typ === "started") {
            startedModel = String(ev.model ?? startedModel);
            startedBaseUrl = String(ev.base_url ?? startedBaseUrl);
          }
          if (typ === "delta") {
            const delta = String((ev as Record<string, unknown>).text ?? "");
            streamText.value += delta;
            // 轻量心跳，不堆芯片，仅在需要时更新阶段提示
            if (streamText.value.length % 400 < delta.length) {
              streamPhase.value = `生成中… ${streamText.value.length} 字符`;
            }
            return;
          }
          if (typ === "done") {
            generateResult.value = {
              task: (ev.task as Record<string, unknown>) ?? {},
              attempts: (ev.attempts as number) ?? 1,
              warnings: (ev.warnings as string[]) ?? [],
              model: startedModel,
              base_url: startedBaseUrl,
            } as AiGenerateResult;
            taskJson.value = JSON.stringify(generateResult.value.task, null, 2);
            pushStreamEvent(ev as Record<string, unknown>);
            return;
          }
          if (typ === "error") {
            lastError = String((ev as Record<string, unknown>).message ?? "生成失败");
            pushStreamEvent(ev as Record<string, unknown>);
            return;
          }
          pushStreamEvent(ev as Record<string, unknown>);
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

function extractJsonFromText(text: string): Record<string, unknown> | null {
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
    task.task_id = `ai-${crypto.randomUUID()}`;
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
    void router.push({ name: "tasks-browser" });
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

onMounted(() => {
  void loadConfig();
  void restoreCapture();
  void loadBrowserChannel();
});

onUnmounted(() => {
  // 离开页面即中止流式请求：后端经断连检测取消 LLM 调用，toast 也不再跨页弹出
  streamAbort?.abort();
  streamAbort = null;
  if (streamPhaseTimer) clearTimeout(streamPhaseTimer);
});

/** 页面刷新后恢复捕获状态：后端产物仍在时无需强制重新捕获 */
async function restoreCapture(): Promise<void> {
  try {
    const st = await aiApi.captureStatus();
    if (st?.available) {
      captureResult.value = {
        final_url: st.final_url || "",
        title: st.title,
        structure_summary: st.structure_summary,
      } as AiCaptureResult;
      screenshotUrl.value = aiApi.captureScreenshotUrl();
    }
  } catch {
    // 状态恢复失败不阻塞页面，按未捕获处理即可
  }
}
</script>

<template>
  <div class="ai-task-page">
    <div class="ai-dev-notice">
      <IconApp name="alert-triangle" class="icon-sm" />
      <span>
        当前功能仍在开发，可能不稳定。如果无法生成正确任务，请到
        <router-link to="/settings/tasks">设置 · 任务与环境</router-link>
        下载<router-link to="/settings/tasks">任务录制器</router-link>，点选元素后把提示词交给任意大模型生成；
        也可参考
        <a :href="TUTORIAL_VIDEO_URL" target="_blank" rel="noopener noreferrer">使用教程视频</a>。
      </span>
    </div>

    <div class="ai-stepper" aria-label="步骤进度">
      <div class="ai-step" :class="{ done: isConfigDone, active: !isConfigDone }">
        <span class="ai-step-dot">{{ isConfigDone ? "✓" : "1" }}</span>
        <span class="ai-step-label">配置 LLM</span>
      </div>
      <span class="ai-step-sep" :class="{ done: isConfigDone }"></span>
      <div class="ai-step" :class="{ done: isCaptureDone, active: isConfigDone && !isCaptureDone }">
        <span class="ai-step-dot">{{ isCaptureDone ? "✓" : "2" }}</span>
        <span class="ai-step-label">捕获页面</span>
      </div>
      <span class="ai-step-sep" :class="{ done: isCaptureDone }"></span>
      <div class="ai-step" :class="{ done: isGenerateDone, active: isCaptureDone && !isGenerateDone }">
        <span class="ai-step-dot">{{ isGenerateDone ? "✓" : "3" }}</span>
        <span class="ai-step-label">生成任务</span>
      </div>
    </div>

    <div class="hint ai-steps-hint">
      <b>使用步骤</b>
      <ol>
        <li>先选择模型服务商并保存配置：需要该服务商的 API Key（各自独立加密保存），模型名可在保存前点「获取模型列表」从服务商拉取。</li>
        <li>退出校园网登录后捕获认证页，再连回网络生成任务（总时长最长 10 分钟）。</li>
        <li>在下方预览 JSON，确认后保存为任务。</li>
      </ol>
    </div>

    <div class="ai-task-grid">
      <div class="card" :class="{ 'ai-card-done': isConfigDone }">
        <div class="card-header">
          <h2><IconApp name="sparkles" class="icon-sm" /> 第 1 步 · 配置 LLM 服务</h2>
          <span class="badge badge--sm" :class="isConfigDone ? 'badge--success' : 'badge--warn'">{{ isConfigDone ? "已配置" : "待配置" }}</span>
        </div>
        <div class="card-body">
          <button
            type="button"
            class="ai-config-toggle"
            :aria-expanded="configExpanded"
            @click="configExpanded = !configExpanded"
          >
            <span class="ai-config-toggle-label">模型配置</span>
            <span class="ai-config-summary">{{ configSummary }}</span>
            <IconApp name="chevron-down" class="icon-sm ai-config-chevron" :class="{ open: configExpanded }" />
          </button>
          <div v-show="configExpanded">
            <div class="hint ai-privacy-hint">
              API Key 按服务商分别加密保存在本机。切换服务商时会自动切换到对应 Key，不会把 DeepSeek Key 发给 GLM。
            </div>
            <div class="ai-provider-grid" role="radiogroup" aria-label="模型服务商">
              <button
                v-for="item in PRESETS"
                :key="item.id"
                type="button"
                class="ai-provider-card"
                :class="{ selected: provider === item.id }"
                role="radio"
                :aria-checked="provider === item.id"
                @click="selectProvider(item.id)"
              >
                <span class="ai-provider-signal"></span>
                <span class="ai-provider-copy"><b>{{ item.label }}</b><small>{{ item.hint }}</small></span>
                <span v-if="configuredProviders.includes(item.id) || item.defaultKey" class="ai-key-state">Key 已就绪</span>
              </button>
            </div>
            <div class="form-row form-row--wide">
              <div class="form-group">
                <label for="ai-base-url" class="required">Base URL</label>
                <input id="ai-base-url" v-model="baseUrl" type="text" placeholder="https://open.bigmodel.cn/api/paas/v4" autocomplete="off" spellcheck="false" />
                <span class="hint">实际请求：<code>{{ actualRequestUrl }}</code></span>
              </div>
            </div>
            <!-- 两行两列：地址与模型一行、凭据与输出上限一行。此前这一行放 3 个字段，
                 而 .form-row 只有两列，「最长输出」被挤到第二行首列，右侧空着且与
                 API Key 的说明文字错位。 -->
            <div class="form-row">
              <div class="form-group">
                <label for="ai-model" class="required">模型名（需支持视觉输入）</label>
                <div class="ai-model-row">
                  <CustomSelect
                    v-if="modelList.length"
                    id="ai-model-select"
                    v-model="modelChoice"
                    :options="modelOptions"
                    :disabled="loadingModels"
                    placeholder="请选择模型"
                    aria-label="模型名"
                  />
                  <input
                    v-if="!modelList.length || customModel"
                    id="ai-model"
                    v-model="model"
                    type="text"
                    placeholder="例如 glm-5.3-flash"
                    autocomplete="off"
                    spellcheck="false"
                  />
                  <button
                    type="button"
                    class="btn btn-secondary btn-sm"
                    :disabled="loadingModels || !baseUrl.trim()"
                    :title="`从 ${modelListUrl} 拉取模型列表`"
                    @click="fetchModelList"
                  >
                    <IconApp v-if="loadingModels" name="refresh" class="spin" />
                    {{ loadingModels ? "获取中…" : "获取模型列表" }}
                  </button>
                </div>
                <span v-if="!baseUrl.trim()" class="hint">请先填写 Base URL，再拉取模型列表</span>
                <span v-else-if="!modelList.length" class="hint">
                  可手填；点「获取模型列表」从 <code>{{ modelListUrl }}</code> 拉取后改为下拉选择
                </span>
                <span v-else-if="customModel" class="hint">当前为手动填写，也可从左侧列表中选择</span>
              </div>
              <div class="form-group">
                <label for="ai-max-tokens">最长输出</label>
                <CustomSelect id="ai-max-tokens" v-model="maxTokens" :options="maxTokenOptions" />
              </div>
            </div>
            <div class="form-row">
              <div class="form-group">
                <label for="ai-api-key">API Key</label>
                <input id="ai-api-key" v-model="apiKey" type="password" :placeholder="hasApiKey ? '已保存（留空保持不变）' : 'sk-...'" autocomplete="new-password" />
                <span class="hint">{{ hasApiKey ? `已使用 ${PRESETS.find(p => p.id === provider)?.label || "当前服务"} 的独立 Key` : "当前服务商尚未保存 Key" }}</span>
              </div>
            </div>
            <div class="ai-actions">
              <button class="btn btn-primary" :disabled="savingConfig" @click="saveConfig">
                <IconApp name="save" class="icon-sm" />
                {{ savingConfig ? "保存中…" : "保存配置" }}
              </button>
              <button v-if="hasApiKey && !PRESETS.find(p => p.id === provider)?.defaultKey" class="btn btn-secondary" :disabled="savingConfig" @click="clearApiKey">
                清除当前 Key
              </button>
              <button class="btn btn-secondary" :disabled="testingConfig || !isConfigDone" :title="!isConfigDone ? configTestHint : '测试当前已保存的模型配置'" @click="testConnection">
                {{ testingConfig ? "测试中…" : "测试连接" }}
              </button>
              <span v-if="!isConfigDone" class="hint">{{ configTestHint }}</span>
            </div>
          </div>
        </div>
      </div>

      <div class="card" :class="{ 'ai-card-done': isCaptureDone }">
        <div class="card-header">
          <h2><IconApp name="image" class="icon-sm" /> 第 2 步 · 捕获登录页面</h2>
          <span class="badge badge--sm" :class="isCaptureDone ? 'badge--success' : 'badge--warn'">{{ isCaptureDone ? "已捕获" : "待捕获" }}</span>
        </div>
        <div class="card-body">
          <div class="hint">
            请在<b>未登录校园网</b>状态下捕获（已认证时不会跳转到登录页）。页面内容与截图将发送给你配置的 LLM 服务商。
          </div>
          <div v-if="captureChannelDegraded" class="ai-capture-warn">
            <span>
              当前浏览器渠道 <b>{{ browserChannel }}</b> 不支持 CDP：捕获仍可进行，但 <b>MHTML 完整布局快照不可用</b>，CSS/JS 改为联网回补抓取。
              需要完整快照请到「浏览器设置」把浏览器渠道切换为 Chromium / Chrome / Edge 后重新捕获。
            </span>
          </div>
          <div class="form-group">
            <label for="ai-capture-url" class="required">登录页地址</label>
            <input id="ai-capture-url" v-model="captureUrl" type="text" placeholder="例如 http://10.x.x.x 或任意网址（未登录时自动跳转到认证页）" autocomplete="off" spellcheck="false" @keyup.enter="capture" />
            <span class="hint">请填写登录页或可触发校园网跳转的地址。</span>
          </div>
          <div class="ai-actions">
            <button class="btn btn-primary" :disabled="capturing" @click="capture">
              <IconApp name="zoom-in" class="icon-sm" />
              {{ capturing ? "捕获中…" : "开始捕获" }}
            </button>
            <span v-if="capturing" class="hint">正在导航并抓取页面资源…</span>
          </div>

          <template v-if="captureResult">
            <div class="ai-capture-meta">
              <div>落地地址：<code>{{ captureResult.final_url }}</code></div>
              <div v-if="captureResult.title">页面标题：{{ captureResult.title }}</div>
              <div>资源快照：{{ captureResult.resources_count ?? 0 }} 个文件<span v-if="captureResult.note">（{{ captureResult.note }}）</span></div>
            </div>
            <div v-if="captureResult.structure_summary" class="ai-structure-summary" aria-label="结构化页面扫描结果">
              <span><b>{{ captureResult.structure_summary.frames }}</b> 个页面层级</span>
              <span><b>{{ captureResult.structure_summary.forms }}</b> 个表单</span>
              <span><b>{{ captureResult.structure_summary.controls }}</b> 个控件</span>
              <span><b>{{ captureResult.structure_summary.captcha_candidates }}</b> 个验证码候选</span>
              <small>生成时优先读取结构信息，并始终附带脱敏局部 HTML。</small>
            </div>
            <div class="ai-actions">
              <button class="btn btn-secondary btn-sm" :disabled="savingBundle" @click="saveBundle" title="下载离线页面文件：MHTML（Chromium 渠道）+ 原始 HTML + 离线副本 + CSS/JS 资源 + 截图">
                <IconApp name="download" class="icon-sm" />
                {{ savingBundle ? "打包中…" : "保存页面文件" }}
              </button>
            </div>
            <img v-if="screenshotUrl" class="ai-screenshot" :src="screenshotUrl" alt="登录页截图" />
          </template>
        </div>
      </div>

      <div class="card" :class="{ 'ai-card-done': isGenerateDone }">
        <div class="card-header">
          <h2><IconApp name="code" class="icon-sm" /> 第 3 步 · 生成并保存任务</h2>
          <span class="badge badge--sm" :class="isGenerateDone ? 'badge--success' : generating ? 'badge--primary' : 'badge--warn'">{{ isGenerateDone ? "已生成" : generating ? "生成中" : "待生成" }}</span>
        </div>
        <div class="card-body">
          <div class="form-group">
            <label for="ai-extra">补充说明（可选）</label>
            <textarea id="ai-extra" v-model="extraPrompt" rows="2" placeholder="例如：运营商选择「中国电信」；验证码每 60 秒刷新一次" :disabled="generating"></textarea>
          </div>

          <div class="ai-generate-bar">
            <div class="ai-idle-group">
              <label for="ai-idle" class="ai-idle-label">前端空闲超时</label>
              <CustomSelect v-model="streamIdleValue" :options="streamIdleOptions" class="ai-idle-select" compact />
            </div>
            <span class="hint ai-idle-hint">有输出会刷新空闲计时；服务端单次生成总时长仍不超过 10 分钟</span>
            <span class="ai-generate-hint hint" v-if="streamPhase && !generateResult">{{ streamPhase }}</span>
            <span class="ai-generate-hint hint" v-else-if="generateResult">第 {{ generateResult.attempts }} 轮通过校验 · {{ generateResult.model }}</span>
          </div>

          <div class="ai-actions">
            <button class="btn btn-primary" :disabled="generating || !isCaptureDone" title="需先完成第 2 步捕获" @click="generate">
              <IconApp name="sparkles" class="icon-sm" />
              {{ generating ? "生成中…" : "生成任务" }}
            </button>
            <button v-if="generating" class="btn btn-secondary" @click="cancelGenerate">取消</button>
            <span v-if="!isCaptureDone && !generating" class="hint">请先完成页面捕获</span>
          </div>

          <div v-if="generating || streamText || streamEvents.length" class="ai-stream-card">
            <div class="ai-stream-header">
              <span class="ai-stream-title"><IconApp name="activity" class="icon-sm" /> 流式输出</span>
              <span class="hint">{{ streamText.length ? `${streamText.length} 字符` : "等待输出…" }} · {{ streamPhase || "准备中" }}</span>
              <span class="ai-stream-actions">
                <button class="btn btn-secondary btn-sm" :disabled="!streamText" @click="copyStream">复制</button>
                <button class="btn btn-secondary btn-sm" :disabled="!streamText && !streamEvents.length" @click="clearStream">清空</button>
              </span>
            </div>
            <pre id="ai-stream-preview" class="ai-stream-preview">{{ streamText || "等待模型输出…" }}</pre>
            <div v-if="streamEvents.length" class="ai-stream-events">
              <span v-for="(ev, i) in streamEvents.slice(-6)" :key="i" class="ai-stream-chip" :class="'chip-' + String(ev.type)">{{ String(ev.type) }}</span>
            </div>
          </div>

          <ul v-if="generateResult?.warnings?.length" class="ai-warnings">
            <li v-for="(w, i) in generateResult.warnings" :key="i">{{ w }}</li>
          </ul>

          <div v-if="taskJson" class="form-group">
            <label for="ai-task-json">任务 JSON（可编辑）</label>
            <textarea id="ai-task-json" v-model="taskJson" rows="14" spellcheck="false" class="ai-json"></textarea>
            <div v-if="jsonError" class="ai-json-error">JSON 格式错误：{{ jsonError }}</div>
          </div>
          <div v-if="taskJson" class="ai-actions">
            <button class="btn btn-secondary btn-sm" @click="formatJson">格式化</button>
            <button class="btn btn-primary" :disabled="savingTask" @click="saveTask">
              <IconApp name="file-check" class="icon-sm" />
              {{ savingTask ? "保存中…" : "保存为任务" }}
            </button>
          </div>
        </div>
      </div>
    </div>

    <div v-if="generating || streamPhase" id="ai-bottom-progress" class="ai-bottom-progress" role="status" aria-live="polite">
      <div class="ai-bottom-progress-inner">
        <span class="ai-bottom-dot" :class="{ active: generating }"></span>
        <span class="ai-bottom-text">{{ streamPhase || "准备中…" }}</span>
        <span class="hint ai-bottom-meta">{{ streamText.length ? `${streamText.length} 字符` : "" }} · 空闲 {{ Math.round(streamIdleMs/60000) }} 分钟（有输出续命）</span>
        <button v-if="generating" class="btn btn-secondary btn-sm ai-bottom-cancel" @click="cancelGenerate">取消</button>
      </div>
      <div class="ai-bottom-bar"><div class="ai-bottom-bar-fill" :class="{ running: generating }"></div></div>
    </div>
  </div>
</template>
