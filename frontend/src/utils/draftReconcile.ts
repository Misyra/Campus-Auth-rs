/**
 * 编辑草稿与列表的对账（四个任务面板共用一份口径）。
 *
 * 要解决的问题：正在编辑的任务在别处被删掉——手改磁盘、另一个实例、或列表刷新后发现
 * 它已不在列表里。此时编辑器仍开着，用户每改一处都会 PUT 到一个不存在的 id、拿到 404
 * 并弹一次失败提示，草稿却留在页面上。四个面板此前都是这个行为。
 *
 * **判据是「曾经在列表里、现在不在了」**，而不是「现在不在列表里」。
 * 后者会误关编辑器：新建草稿在"首次落盘成功"与"下一次列表刷新"之间本来就不在列表里，
 * 那一刻若按"不在列表即关闭"处理，刚建好的任务会立刻被踢出编辑器。
 *
 * 本模块只做纯判定，`previousIds` 由调用方持有；副作用（关编辑器/提示）留在各面板。
 */

export interface DraftReconcileState {
  /** 上一次对账时列表里的 id 集合（首轮为调用方看到的初始列表） */
  previousIds: readonly string[];
}

export interface DraftReconcileInput {
  state: DraftReconcileState;
  /** 当前编辑中的草稿 id；无草稿传 null */
  draftId: string | null | undefined;
  /** 列表刚刷新出来的 id 集合 */
  listIds: readonly string[];
}

export interface DraftReconcileResult {
  /** 是否应关闭编辑器（草稿指向的任务已从列表消失） */
  gone: boolean;
  /** 供下一轮使用的状态 */
  state: DraftReconcileState;
}

/** 未开始对账时的初始状态 */
export function initialReconcileState(listIds: readonly string[] = []): DraftReconcileState {
  return { previousIds: listIds };
}

/**
 * 对账一轮。
 *
 * 注意 `state` 每轮都会被替换成新的 `listIds`，无论是否判定为 `gone`——
 * 否则一次"暂时不在列表里"会让后续所有轮次都拿旧的基线比对。
 */
export function reconcileDraft(input: DraftReconcileInput): DraftReconcileResult {
  const { state, draftId, listIds } = input;
  const gone =
    !!draftId && state.previousIds.includes(draftId) && !listIds.includes(draftId);
  return { gone, state: { previousIds: [...listIds] } };
}
