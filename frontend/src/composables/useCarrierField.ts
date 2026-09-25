/**
 * 运营商选择与自定义关键字的双向映射。
 *
 * 后端仍只保存一个 `isp` 字段：预设值直接保存；自定义模式下输入框为空时 `isp`
 * 暂存「自定义」哨兵（仅界面回显用），保存校验会拦截该哨兵并提示填写关键字
 * （见 useProfiles 的保存校验），不会带着空关键字落盘。
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
