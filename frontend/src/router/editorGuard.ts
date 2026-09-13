/**
 * 编辑器离开守卫（FE2-9）。
 *
 * 任务/脚本编辑器是页面内嵌卡片而非遮罩弹层：侧边栏 SPA 导航此前会静默丢弃
 * 编辑中的内容（仅手动关闭按钮有 dirty 确认）。本守卫在路由离开 /tasks、/scripts
 * 页时按 from 路由定向判定——不做"任一全局 dirty 即拦截"，避免单例残留状态干扰
 * 无关页面。
 *
 * 覆盖范围：仅 SPA 路由离开（侧边栏/链接跳转）。**不含**浏览器刷新、关闭标签页、
 * 前进后退键等整页级离开——这些需 `beforeunload`（浏览器原生弹窗文案不可定制，
 * 与页内二选一弹窗交互也不一致），当前未实现；如后续要覆盖，须另接 beforeunload。
 *
 * 确认弹窗为二选一（放弃/继续编辑）：true=放弃草稿放行；false/被抢占（null）
 * 均阻断导航且不清草稿——仅确认不清空会让草稿残留，返回该页仍会出现。
 */

import { useConfirm } from "../composables/useConfirm";
import { useTasks } from "../composables/useTasks";
import { useScripts } from "../composables/useScripts";

export async function guardEditorLeave(
  to: { path: string },
  from: { path: string },
): Promise<boolean> {
  const leftTasks = from.path.startsWith("/tasks") && !to.path.startsWith("/tasks");
  const leftScripts = from.path.startsWith("/scripts") && !to.path.startsWith("/scripts");
  if (!leftTasks && !leftScripts) return true;

  const { confirm } = useConfirm();
  if (leftTasks) {
    const { isTaskDirty, clearTaskDraft } = useTasks();
    if (isTaskDirty()) {
      const discard = await discardConfirm(confirm, "任务");
      if (discard !== true) return false;
      clearTaskDraft();
    }
  }
  if (leftScripts) {
    const { isScriptDirty, clearScriptDraft } = useScripts();
    if (isScriptDirty()) {
      const discard = await discardConfirm(confirm, "脚本");
      if (discard !== true) return false;
      clearScriptDraft();
    }
  }
  return true;
}

/** 二选一确认（确认=放弃草稿）；取消与被抢占统一按"留在编辑页"处理 */
async function discardConfirm(
  confirm: (options: {
    title?: string;
    message: string;
    danger?: boolean;
  }) => Promise<boolean | null>,
  entity: string,
): Promise<boolean | null> {
  return confirm({
    title: "放弃未保存的修改",
    message: `当前${entity}有未保存的修改，确定放弃吗？`,
    danger: true,
  });
}
