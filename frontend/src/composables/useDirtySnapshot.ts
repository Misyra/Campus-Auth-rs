/**
 * 编辑器 dirty 快照三件套（快照保存 / 脏判定 / 丢弃确认）的通用实现。
 * 收敛 useTasks / useScripts / useProfiles 三处逐字重复的快照模式：
 * 打开编辑器时记录草稿的 JSON 全量序列化基准，脏判定为当前草稿与基准不一致，
 * 关闭/切换前经确认对话框放行放弃操作（A10 语义：被抢占≠用户放弃）。
 */

import type { Ref } from "vue";
import { useConfirm } from "./useConfirm";

export interface DirtySnapshotOptions {
  /** 确认弹窗正文中的实体名（如 "任务" / "脚本" / "配置方案"），用于拼装确认文案 */
  entityName: string;
}

/**
 * 基于 JSON 全量序列化的草稿脏快照管理。
 * 草稿 ref 由调用方持有（类型各异），本组合式函数只维护基准快照与判定逻辑。
 */
export function useDirtySnapshot<T>(draft: Ref<T | null>, options: DirtySnapshotOptions) {
  const { confirm } = useConfirm();

  // 基准快照：打开/重置编辑器时写入，是 dirty 判定的对照基准
  let snapshot = "";

  /** 计算草稿的快照基准（JSON 全量序列化，草稿字段均为小体量标量） */
  function snapshotOf(value: T | null): string {
    return value ? JSON.stringify(value) : "";
  }

  /** 统一的草稿写入入口：赋值并同步刷新 dirty 基准快照。 */
  function setDraft(value: T): void {
    draft.value = value;
    snapshot = snapshotOf(value);
  }

  /** 仅刷新基准快照（草稿已在外部赋值、只需重置 dirty 基准的场景）。 */
  function refreshSnapshot(): void {
    snapshot = snapshotOf(draft.value);
  }

  /** 清空基准快照（草稿清空/保存成功等不再需要 dirty 判定的场景）。 */
  function resetSnapshot(): void {
    snapshot = "";
  }

  /** 当前编辑器是否有未保存改动。 */
  function isDirty(): boolean {
    return draft.value !== null && snapshotOf(draft.value) !== snapshot;
  }

  /**
   * 若存在未保存改动，弹窗确认是否放弃；无改动则直接放行。
   * 返回 true 才允许继续（放弃修改）；false（用户取消）与 null（被新对话框抢占）
   * 一律不放行——保留现状、不丢弃数据（A10 语义：被抢占≠用户放弃）。
   */
  async function confirmDiscardIfDirty(): Promise<boolean | null> {
    if (!isDirty()) return true;
    return confirm({
      title: "放弃未保存的修改",
      message: `当前${options.entityName}有未保存的修改，确定放弃吗？`,
      danger: true,
    });
  }

  return { setDraft, refreshSnapshot, resetSnapshot, isDirty, confirmDiscardIfDirty };
}
