/** 设置页的分区定位请求总线：命令面板等外部入口请求滚动到某分区。 */
import { readonly, shallowRef } from "vue";

const scrollRequest = shallowRef<{ id: string } | null>(null);

/** 每次创建新请求，同一分区在手动滚走后仍可再次点击定位。 */
export function useSettingsNavigation() {
  return {
    scrollRequest: readonly(scrollRequest),
    requestScroll: (id: string) => { scrollRequest.value = { id }; },
  };
}
