/**
 * 运营商选择与自定义关键字的双向映射。
 *
 * 后端仍只保存一个 `isp` 字段：预设值直接保存，自定义模式在关键字为空时
 * 保存“自定义”哨兵。这样清空第二个输入框不会意外切回“不选择”。
 */

import { computed, type ComputedRef, type Ref, type WritableComputedRef } from "vue";
import { CARRIER_OPTIONS } from "@/utils/constants";

const CUSTOM_CARRIER = "自定义";
const carrierPresetValues = new Set(
  CARRIER_OPTIONS.map((option) => option.value).filter(
    (value) => value !== "" && value !== CUSTOM_CARRIER,
  ),
);

export interface CarrierFieldState {
  showCustomCarrier: ComputedRef<boolean>;
  carrierSelection: WritableComputedRef<string>;
  customCarrierValue: WritableComputedRef<string>;
}

/** 将单一 isp 字段映射为稳定的下拉选择和自定义输入状态。 */
export function useCarrierField(isp: Ref<string>): CarrierFieldState {
  const showCustomCarrier = computed(
    () => isp.value === CUSTOM_CARRIER
      || (isp.value !== "" && !carrierPresetValues.has(isp.value)),
  );

  const carrierSelection = computed<string>({
    get: () => showCustomCarrier.value ? CUSTOM_CARRIER : isp.value,
    set: (value) => {
      isp.value = value;
    },
  });

  const customCarrierValue = computed<string>({
    get: () => isp.value === CUSTOM_CARRIER ? "" : isp.value,
    set: (value) => {
      const normalized = value.trim();
      isp.value = normalized || CUSTOM_CARRIER;
    },
  });

  return { showCustomCarrier, carrierSelection, customCarrierValue };
}
