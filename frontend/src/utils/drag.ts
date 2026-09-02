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
    const from = list.value.findIndex((t) => t.id === dragState!.taskId);
    if (from === -1) return;
    const item = list.value.splice(from, 1)[0];
    let to = list.value.findIndex((t) => t.id === items[index].id);
    if (dragState.currentIndex < index) to++;
    list.value.splice(to, 0, item);
    dragState.currentIndex = to;
  }

  function onDrop(e: DragEvent, _index: number): void {
    e.preventDefault();
    dragState = null;
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
    dragging,
    onHandleMouseDown,
    onHandleMouseUp,
    handleDragStart,
    onDragOver,
    onDrop,
    onDragEnd,
    persistOrder,
  };
}
