/** 设置长页的滚动位置与定位请求，供主侧栏和页内目录共用。 */
import { readonly, ref, shallowRef } from "vue";

const activeSection = ref("monitor");
const scrollRequest = shallowRef<{ id: string } | null>(null);

/** 每次创建新请求，同一分类在手动滚走后仍可再次点击定位。 */
export function useSettingsNavigation() {
  return {
    activeSection: readonly(activeSection),
    scrollRequest: readonly(scrollRequest),
    setActiveSection: (id: string) => { activeSection.value = id; },
    requestScroll: (id: string) => { scrollRequest.value = { id }; },
  };
}
