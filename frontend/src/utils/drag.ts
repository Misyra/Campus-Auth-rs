/**
 * 列表拖拽排序组合式。
 * 从 legacy js/methods/drag.js 迁移：实时交换模式 + 防抖 + 顺序持久化。
 */

import { ref } from "vue";
import type { Ref } from "vue";
import type { TaskItem } from "../api/types";
import { tasksApi } from "../api";
import { TIMING } from "./constants";

interface DragState {
  taskId: string;
  currentIndex: number;
}

/**
 * @param tasks 完整任务列表（拖拽在此列表上重排）
 * @param scripts 脚本列表（仅用于持久化顺序，可选）
 */
export function useDragSort(tasks: Ref<TaskItem[]>, scripts?: Ref<{ id: string }[]>) {
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
    const list = tasks.value;
    if (!list[index]) return;
    dragState = { taskId: list[index].id, currentIndex: index };
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
    const list = tasks.value;
    if (!list[index] || list[index].id === dragState.taskId) return;
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
    const from = tasks.value.findIndex((t) => t.id === dragState!.taskId);
    if (from === -1) return;
    const item = tasks.value.splice(from, 1)[0];
    let to = tasks.value.findIndex((t) => t.id === list[index].id);
    if (dragState.currentIndex < index) to++;
    tasks.value.splice(to, 0, item);
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
        all: tasks.value.map((t) => t.id),
        scripts: (scripts?.value || []).map((s) => s.id),
      });
    } catch {
      /* 静默处理 */
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
