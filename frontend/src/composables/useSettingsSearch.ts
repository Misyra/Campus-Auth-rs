/**
 * 设置页搜索词单例：输入框在顶栏（AppTopbar），过滤在 SettingsView。
 *
 * 两处共享同一个 ref——顶栏在设置路由下把「搜索」按钮切换为输入框（与命令面板
 * 入口合并为一个搜索位置），SettingsView 只负责对表单应用过滤与空态统计，
 * 不再自绘第二个搜索框。
 */
import { ref } from "vue";

const query = ref("");

export function useSettingsSearch() {
  return { query };
}
