/**
 * useAiLlmConfig 单测：LLM 配置草稿的关键分支。
 * 覆盖：Key 槽位推导（内置/自定义 origin/畸形地址）、loadConfig 空配置回落预设与
 * 已保存配置种草稿、已下架渠道回落 custom、selectProvider 草稿暂存/恢复/空表单不落草稿、
 * fetchModelList 前置校验/命中与未命中/api_key 传递/失败兜底、saveConfig 载荷映射
 * （auto→null、api_key 条件携带）与保存后状态翻转、clearApiKey 清除载荷、
 * testConnection 未保存拦截/成功文案（含 note）/失败兜底、configTestHint 分支。
 */
import { describe, it, expect, vi, beforeEach } from "vitest";

const { aiApiMock, toastOnlyMock } = vi.hoisted(() => ({
  aiApiMock: {
    fetchLlmConfig: vi.fn(),
    saveLlmConfig: vi.fn(),
    testLlmConfig: vi.fn(),
    fetchModels: vi.fn(),
  },
  toastOnlyMock: vi.fn(),
}));

vi.mock("../api", () => ({ aiApi: aiApiMock }));

const { useAiLlmConfig, keySlotOf } = await import("./useAiLlmConfig");

function make() {
  return useAiLlmConfig({ toastOnly: toastOnlyMock });
}

beforeEach(() => {
  vi.clearAllMocks();
});

describe("keySlotOf", () => {
  it("内置服务商按标识取槽位，自定义按 URL origin，畸形地址回落空串", () => {
    expect(keySlotOf("glm", "https://ignored.example.com")).toBe("glm");
    expect(keySlotOf("custom", "  https://a.example.com/v1?x=1 ")).toBe("https://a.example.com");
    expect(keySlotOf("custom", "not-a-url")).toBe("");
  });
});

describe("loadConfig", () => {
  it("空配置回落 glm 预设并保持展开", async () => {
    aiApiMock.fetchLlmConfig.mockResolvedValue({
      provider: "glm", base_url: "", model: "", has_api_key: false, configured_providers: [], max_tokens: null,
    });
    const ai = make();
    await ai.loadConfig();
    expect(ai.provider.value).toBe("glm");
    expect(ai.baseUrl.value).toBe("https://open.bigmodel.cn/api/paas/v4");
    expect(ai.model.value).toBe("glm-5.3-flash");
    expect(ai.configExpanded.value).toBe(true);
    expect(ai.isConfigDone.value).toBe(false);
  });

  it("已保存配置回填表单、Key 槽位、max_tokens 与保存签名", async () => {
    aiApiMock.fetchLlmConfig.mockResolvedValue({
      provider: "glm", base_url: "https://api.example.com/v4", model: "vis-1",
      has_api_key: true, configured_providers: ["glm"], max_tokens: 16384,
    });
    const ai = make();
    await ai.loadConfig();
    expect(ai.maxTokens.value).toBe("16384");
    // hasApiKey 为真等价于 savedKeySlot 与当前槽位对上（savedKeySlot 未直接暴露）
    expect(ai.hasApiKey.value).toBe(true);
    expect(ai.configExpanded.value).toBe(false);
    expect(ai.isConfigDone.value).toBe(true);
  });

  it("max_tokens 为 null 映射为 auto，且 auto 计入保存签名", async () => {
    aiApiMock.fetchLlmConfig.mockResolvedValue({
      provider: "deepseek", base_url: "https://api.example.com", model: "m1",
      has_api_key: false, configured_providers: [], max_tokens: null,
    });
    const ai = make();
    await ai.loadConfig();
    expect(ai.maxTokens.value).toBe("auto");
    expect(ai.isConfigDone.value).toBe(true);
  });

  it("已下架渠道（预设无对应项）回落自定义服务商，Key 槽位按 origin 对上", async () => {
    aiApiMock.fetchLlmConfig.mockResolvedValue({
      provider: "opencode", base_url: "https://legacy.example.com/v1", model: "free",
      has_api_key: true, configured_providers: [], max_tokens: null,
    });
    const ai = make();
    await ai.loadConfig();
    expect(ai.provider.value).toBe("custom");
    expect(ai.hasApiKey.value).toBe(true);
  });

  it("读取失败走失败提示", async () => {
    aiApiMock.fetchLlmConfig.mockRejectedValue(new Error("boom"));
    const ai = make();
    await ai.loadConfig();
    expect(toastOnlyMock).toHaveBeenCalledWith(false, "boom");
  });
});

describe("selectProvider（草稿槽）", () => {
  async function loadSaved() {
    aiApiMock.fetchLlmConfig.mockResolvedValue({
      provider: "glm", base_url: "https://api.example.com/v4", model: "vis-1",
      has_api_key: true, configured_providers: ["glm"], max_tokens: 16384,
    });
    const ai = make();
    await ai.loadConfig();
    return ai;
  }

  it("切走暂存当前表单，切回恢复编辑值而不是预设", async () => {
    const ai = await loadSaved();
    ai.baseUrl.value = "https://edited.example.com/v4";
    ai.model.value = "edited-model";
    ai.selectProvider("deepseek");
    expect(ai.baseUrl.value).toBe("https://api.deepseek.com");
    expect(ai.model.value).toBe("");
    expect(ai.customModel.value).toBe(true);
    expect(ai.configExpanded.value).toBe(true);
    ai.selectProvider("glm");
    expect(ai.baseUrl.value).toBe("https://edited.example.com/v4");
    expect(ai.model.value).toBe("edited-model");
  });

  it("空表单不落草稿：切走再切回回落预设默认值", () => {
    const ai = make();
    ai.selectProvider("deepseek");
    expect(ai.baseUrl.value).toBe("https://api.deepseek.com");
    ai.selectProvider("glm");
    expect(ai.baseUrl.value).toBe("https://open.bigmodel.cn/api/paas/v4");
    expect(ai.model.value).toBe("glm-5.3-flash");
  });
});

describe("fetchModelList", () => {
  it("未填 Base URL 直接提示且不发请求", async () => {
    const ai = make();
    await ai.fetchModelList();
    expect(toastOnlyMock).toHaveBeenCalledWith(false, "请先填写 Base URL");
    expect(aiApiMock.fetchModels).not.toHaveBeenCalled();
    expect(ai.loadingModels.value).toBe(false);
  });

  it("拉取成功且当前模型命中列表：切下拉模式", async () => {
    aiApiMock.fetchModels.mockResolvedValue({ count: 2, models: ["m1", "m2"] });
    const ai = make();
    ai.baseUrl.value = "https://api.example.com";
    ai.model.value = "m1";
    ai.apiKey.value = " sk-1 ";
    await ai.fetchModelList();
    expect(aiApiMock.fetchModels).toHaveBeenCalledWith({
      provider: "glm", base_url: "https://api.example.com", api_key: "sk-1",
    });
    expect(ai.modelList.value).toEqual(["m1", "m2"]);
    expect(ai.customModel.value).toBe(false);
    expect(toastOnlyMock).toHaveBeenCalledWith(true, "已获取 2 个模型");
    expect(ai.loadingModels.value).toBe(false);
  });

  it("当前模型不在列表中：保持手动输入并附带提示尾巴", async () => {
    aiApiMock.fetchModels.mockResolvedValue({ count: 1, models: ["m1"] });
    const ai = make();
    ai.baseUrl.value = "https://api.example.com";
    ai.model.value = "private-model";
    await ai.fetchModelList();
    expect(ai.customModel.value).toBe(true);
    expect(toastOnlyMock).toHaveBeenCalledWith(true, "已获取 1 个模型；当前模型不在列表中，仍按手动填写处理");
  });

  it("未填 Key 时 api_key 缺省（后端回退已保存 Key）", async () => {
    aiApiMock.fetchModels.mockResolvedValue({ count: 0, models: [] });
    const ai = make();
    ai.baseUrl.value = "https://api.example.com";
    await ai.fetchModelList();
    expect(aiApiMock.fetchModels).toHaveBeenCalledWith({
      provider: "glm", base_url: "https://api.example.com", api_key: undefined,
    });
    // 空列表：手动输入分支（customModel 为 true）
    expect(ai.customModel.value).toBe(true);
  });

  it("拉取失败走失败提示并复位 loading", async () => {
    aiApiMock.fetchModels.mockRejectedValue(new Error("boom"));
    const ai = make();
    ai.baseUrl.value = "https://api.example.com";
    await ai.fetchModelList();
    expect(toastOnlyMock).toHaveBeenCalledWith(false, "boom");
    expect(ai.loadingModels.value).toBe(false);
  });
});

describe("saveConfig / clearApiKey", () => {
  it("载荷映射：max_tokens 数值化、api_key 仅在有值时携带，保存后状态翻转", async () => {
    aiApiMock.saveLlmConfig.mockResolvedValue({ has_api_key: true, configured_providers: ["glm"] });
    const ai = make();
    ai.baseUrl.value = "https://api.example.com";
    ai.model.value = "m1";
    ai.maxTokens.value = "16384";
    ai.apiKey.value = "sk-9";
    await ai.saveConfig();
    expect(aiApiMock.saveLlmConfig).toHaveBeenCalledWith({
      provider: "glm", base_url: "https://api.example.com", model: "m1", api_key: "sk-9", max_tokens: 16384,
    });
    expect(ai.apiKey.value).toBe("");
    expect(ai.configExpanded.value).toBe(false);
    expect(ai.hasApiKey.value).toBe(true);
    expect(ai.isConfigDone.value).toBe(true);
    expect(toastOnlyMock).toHaveBeenCalledWith(true, "LLM 配置已保存");
    expect(ai.savingConfig.value).toBe(false);
  });

  it("auto 映射为 null，空 Key 不携带 api_key 字段", async () => {
    aiApiMock.saveLlmConfig.mockResolvedValue({ has_api_key: false, configured_providers: [] });
    const ai = make();
    ai.baseUrl.value = "https://api.example.com";
    ai.model.value = "m1";
    ai.maxTokens.value = "auto";
    await ai.saveConfig();
    expect(aiApiMock.saveLlmConfig).toHaveBeenCalledWith({
      provider: "glm", base_url: "https://api.example.com", model: "m1", max_tokens: null,
    });
    expect(ai.hasApiKey.value).toBe(false);
    expect(ai.isConfigDone.value).toBe(true);
  });

  it("保存失败走失败提示并复位 savingConfig", async () => {
    aiApiMock.saveLlmConfig.mockRejectedValue(new Error("boom"));
    const ai = make();
    await ai.saveConfig();
    expect(toastOnlyMock).toHaveBeenCalledWith(false, "boom");
    expect(ai.savingConfig.value).toBe(false);
    // 保存失败后签名未落，配置不算完成
    expect(ai.isConfigDone.value).toBe(false);
  });

  it("clearApiKey 以空串 api_key 保存并复位 Key 槽位", async () => {
    aiApiMock.saveLlmConfig.mockResolvedValue({ has_api_key: false, configured_providers: [] });
    const ai = make();
    ai.baseUrl.value = "https://api.example.com";
    ai.model.value = "m1";
    ai.maxTokens.value = "16384";
    await ai.clearApiKey();
    expect(aiApiMock.saveLlmConfig).toHaveBeenCalledWith({
      provider: "glm", base_url: "https://api.example.com", model: "m1", api_key: "", max_tokens: 16384,
    });
    expect(toastOnlyMock).toHaveBeenCalledWith(true, "当前服务商的 API Key 已清除");
    expect(ai.hasApiKey.value).toBe(false);
  });
});

describe("testConnection / configTestHint", () => {
  it("配置未保存时拦截测试请求", async () => {
    const ai = make();
    await ai.testConnection();
    expect(toastOnlyMock).toHaveBeenCalledWith(false, "请先保存当前配置再测试连接");
    expect(aiApiMock.testLlmConfig).not.toHaveBeenCalled();
  });

  it("测试成功：文案含耗时与截断 note", async () => {
    aiApiMock.fetchLlmConfig.mockResolvedValue({
      provider: "glm", base_url: "https://api.example.com", model: "m1",
      has_api_key: false, configured_providers: [], max_tokens: null,
    });
    aiApiMock.testLlmConfig.mockResolvedValue({ connected: true, latency_ms: 42, request_url: "https://x", note: "回复被测试上限截断" });
    const ai = make();
    await ai.loadConfig();
    await ai.testConnection();
    expect(toastOnlyMock).toHaveBeenCalledWith(true, "连接成功，耗时 42 ms；回复被测试上限截断");
    expect(ai.testingConfig.value).toBe(false);
  });

  it("测试失败走失败提示", async () => {
    aiApiMock.fetchLlmConfig.mockResolvedValue({
      provider: "glm", base_url: "https://api.example.com", model: "m1",
      has_api_key: false, configured_providers: [], max_tokens: null,
    });
    aiApiMock.testLlmConfig.mockRejectedValue(new Error("boom"));
    const ai = make();
    await ai.loadConfig();
    await ai.testConnection();
    expect(toastOnlyMock).toHaveBeenCalledWith(false, "boom");
  });

  it("configTestHint 三分支：缺字段 / 未保存 / 有改动", async () => {
    const ai = make();
    expect(ai.configTestHint.value).toBe("请先填写接口地址和模型名称并保存");
    ai.baseUrl.value = "https://api.example.com";
    ai.model.value = "m1";
    expect(ai.configTestHint.value).toBe("请先保存模型配置，再测试连接");
    // 已保存签名落在表单值上：改动表单后签名错位 → 有改动分支
    aiApiMock.fetchLlmConfig.mockResolvedValue({
      provider: "glm", base_url: "https://api.example.com", model: "m1",
      has_api_key: false, configured_providers: [], max_tokens: null,
    });
    await ai.loadConfig();
    expect(ai.configTestHint.value).toBe("配置有改动，请重新保存后再测试连接");
  });
});
