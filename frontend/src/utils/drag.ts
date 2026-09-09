/**
 * 列表拖拽排序组合式。
 * 从 legacy js/methods/drag.js 迁移：实时交换模式 + 防抖 + 顺序持久化。
 */

import { ref } from "vue";
import type { Ref } from "vue";
import type { TaskItem } from "../api/types";
import { tasksApi } from "../api";
import { TIMING } from "./constants";
import { frontendLogger } from "./logger";

interface DragState {
  taskId: string;
  currentIndex: number;
}

interface DragSortOptions {
  /** 浏览器任务 id 全量序列（持久化到 order.all） */
  tasks: Ref<{ id: string }[]>;
  /** 脚本 id 全量序列（持久化到 order.scripts） */
  scripts: Ref<{ id: string }[]>;
}

/**
 * @param list 拖拽重排的目标列表（本视图持有的列表）
 * @param order 顺序持久化用的全量清单。B1：后端 order 接口会整体替换两组顺序，
 *   漏传的一组会被清空，任务与脚本两个视图都必须互传全量。
 */
export function useDragSort(list: Ref<TaskItem[]>, order: DragSortOptions) {
  const dragging = ref(false);
  let dragState: DragState | null = null;
  let allowDrag = false;
  let swapCooldown = false;
  // F10：拖拽期间发生过交换（onDragOver 即时生效），供 onDragEnd 补持久化
  let orderDirty = false;

  function onHandleMouseDown(e: MouseEvent): void {
    allowDrag = true;
    const item = (e.currentTarget as HTMLElement).closest("[data-draggable-list]");
    if (item) item.setAttribute("draggable", "true");
  }

  function onHandleMouseUp(e: MouseEvent): void {
    const item = (e.currentTarget as HTMLElement).closest("[data-draggable-list]");
    if (item) item.removeAttribute("draggable");
  }

  function handleDragStart(e: DragEvent, index: number): void {
    if (!allowDrag) {
      e.preventDefault();
      return;
    }
    const items = list.value;
    if (!items[index]) return;
    dragState = { taskId: items[index].id, currentIndex: index };
    if (e.dataTransfer) {
      e.dataTransfer.effectAllowed = "move";
      e.dataTransfer.setData("text/plain", "");
    }
    (e.currentTarget as HTMLElement).classList.add("dragging");
  }

  function onDragOver(e: DragEvent, index: number): void {
    e.preventDefault();
    if (e.dataTransfer) e.dataTransfer.dropEffect = "move";
    if (!dragState || swapCooldown) return;
    const items = list.value;
    if (!items[index] || items[index].id === dragState.taskId) return;
    const rect = (e.currentTarget as HTMLElement).getBoundingClientRect();
    const midY = rect.top + rect.height / 2;
    const crossed =
      (dragState.currentIndex < index && e.clientY > midY) ||
      (dragState.currentIndex > index && e.clientY < midY);
    if (!crossed) return;
    swapCooldown = true;
    setTimeout(() => {
      swapCooldown = false;
    }, TIMING.DRAG_SWAP_COOLDOWN);
    // splice 会原地修改 items（它与 list.value 是同一个数组），因此必须先保存
    // 当前悬停项的 id；否则向下拖动时 index 已指向删除后的下一项，排序会越过一项。
    const targetId = items[index].id;
    const from = list.value.findIndex((t) => t.id === dragState!.taskId);
    if (from === -1) return;
    const item = list.value.splice(from, 1)[0];
    let to = list.value.findIndex((t) => t.id === targetId);
    if (to === -1) {
      // 防御性恢复：同步重渲染期间若目标被外部删除，不能连带丢失被拖拽项。
      list.value.splice(from, 0, item);
      return;
    }
    if (from < index) to++;
    list.value.splice(to, 0, item);
    dragState.currentIndex = to;
    orderDirty = true;
  }

  function onDrop(e: DragEvent, _index: number): void {
    e.preventDefault();
    dragState = null;
    orderDirty = false;
    void persistOrder();
  }

  function onDragEnd(e: DragEvent): void {
    (e.currentTarget as HTMLElement).classList.remove("dragging");
    (e.currentTarget as HTMLElement).removeAttribute("draggable");
    dragState = null;
    allowDrag = false;
    swapCooldown = false;
    document
      .querySelectorAll(".drop-before, .drop-after")
      .forEach((el) => el.classList.remove("drop-before", "drop-after"));
    // F10：拖出列表松手不触发 drop——拖拽期间已生效的交换在此补持久化，
    // 否则内存顺序与后端不一致，刷新后静默回退
    if (orderDirty) {
      orderDirty = false;
      void persistOrder();
    }
  }

  async function persistOrder(): Promise<void> {
    try {
      await tasksApi.order({
        all: order.tasks.value.map((t) => t.id),
        scripts: order.scripts.value.map((s) => s.id),
      });
    } catch (error) {
      frontendLogger.warn("tasks", "保存任务排序失败", error);
    }
  }

  return {
    onHandleMouseDown,
    onHandleMouseUp,
    handleDragStart,
    onDragOver,
    onDrop,
    onDragEnd,
  };
}
