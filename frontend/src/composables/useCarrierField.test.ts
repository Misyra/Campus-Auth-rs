import { describe, expect, it } from "vitest";
import { ref } from "vue";
import { useCarrierField } from "./useCarrierField";

describe("useCarrierField", () => {
  it("清空自定义关键字后仍保持自定义选项", () => {
    const isp = ref("");
    const state = useCarrierField(isp);

    state.carrierSelection.value = "自定义";
    expect(state.showCustomCarrier.value).toBe(true);
    expect(state.customCarrierValue.value).toBe("");

    state.customCarrierValue.value = "校园专网";
    expect(isp.value).toBe("校园专网");
    expect(state.carrierSelection.value).toBe("自定义");

    state.customCarrierValue.value = "";
    expect(isp.value).toBe("自定义");
    expect(state.carrierSelection.value).toBe("自定义");
    expect(state.showCustomCarrier.value).toBe(true);
  });

  it("显式选择不选择或预设运营商时退出自定义模式", () => {
    const isp = ref("校园专网");
    const state = useCarrierField(isp);

    state.carrierSelection.value = "";
    expect(isp.value).toBe("");
    expect(state.showCustomCarrier.value).toBe(false);

    state.carrierSelection.value = "移动";
    expect(isp.value).toBe("移动");
    expect(state.carrierSelection.value).toBe("移动");
    expect(state.showCustomCarrier.value).toBe(false);
  });
});
