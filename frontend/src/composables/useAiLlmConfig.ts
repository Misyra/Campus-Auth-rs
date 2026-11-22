/**
 * AI 任务页第 1 步：LLM 服务商配置草稿（预设表单 / 服务商切换草稿槽 / 保存·清除·测试·模型列表拉取）。
 *
 * 自 AiTaskView.vue 按职责拆出（设计系统收口 T8-2）：本模块只管「配置草稿」；
 * 页面捕获见 useAiCapture，生成编排与任务落盘见 useAiGenerate。
 * toastOnly 由调用方显式注入（依赖可见、测试可桩），不在模块内部取单例。
 */
import { computed, ref } from "vue";
import type { SelectOption } from "../components/common/CustomSelect.vue";
import { aiApi } from "../api";
import { extractApiError } from "../api/client";

// ---- 服务商预设 ----
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

const maxTokenOptions: SelectOption[] = [
  { label: "16K（推荐）", value: "16384" },
  { label: "32K（复杂页面）", value: "32768" },
  { label: "由服务商决定", value: "auto" },
];

/** API Key 槽位：自定义服务按地址（origin）区分，其余按服务商标识 */
export function keySlotOf(id: string, url: string): string {
  if (id !== "custom") return id;
  try {
    const parsed = new URL(url.trim());
    return `${parsed.protocol}//${parsed.host}`;
  } catch {
    // 地址还没填/填得不成形：槽位未知，按"没有已保存 Key"处理
    return "";
  }
}

export interface UseAiLlmConfigOptions {
  /** 提示回调（成功/失败 toast，与视图共用同一单例语义） */
  toastOnly: (success: boolean, message: string) => void;
}

/** AI 任务页第 1 步的配置草稿状态与操作；入参显式、无模块级可变状态 */
export function useAiLlmConfig(options: UseAiLlmConfigOptions) {
  const { toastOnly } = options;

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

  /** 当前槽位是否已有服务端保存的 Key（决定输入框占位与「清除当前 Key」按钮） */
  const hasApiKey = computed(() => {
    if (PRESETS.find((item) => item.id === provider.value)?.defaultKey) return true;
    const slot = keySlotOf(provider.value, baseUrl.value);
    return !!slot && slot === savedKeySlot.value;
  });
  const maxTokens = ref("16384");
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

  return {
    /** 服务商预设卡片（纯静态数据，模板 v-for 用） */
    PRESETS,
    provider,
    configuredProviders,
    baseUrl,
    model,
    modelList,
    customModel,
    loadingModels,
    apiKey,
    hasApiKey,
    maxTokens,
    maxTokenOptions,
    configExpanded,
    configSummary,
    actualRequestUrl,
    modelListUrl,
    modelOptions,
    modelChoice,
    savingConfig,
    testingConfig,
    isConfigDone,
    configTestHint,
    selectProvider,
    fetchModelList,
    loadConfig,
    saveConfig,
    clearApiKey,
    testConnection,
  };
}

export type UseAiLlmConfigReturn = ReturnType<typeof useAiLlmConfig>;
