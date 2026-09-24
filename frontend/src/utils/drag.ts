/**
 * 列表拖拽排序组合式。
 * 从 legacy js/methods/drag.js 迁移：实时交换模式 + 防抖 + 顺序持久化。
 */

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
  /** 浏览器任务 id 全量序列（持久化到 `order.all`） */
  tasks: Ref<{ id: string }[]>;
  /** 脚本 id 全量序列（持久化到 `order.scripts`） */
  scripts: Ref<{ id: string }[]>;
  /** 直连任务 id 全量序列（持久化到 `order.http`） */
  http: Ref<{ id: string }[]>;
}

/**
 * 顺序持久化载荷：三组**全量**互传。
 *
 * 抽成纯函数是为了能被 vitest 直接盯住——后端 `order_tasks` 是整体替换
 * `.order.json`（先 clear 再 extend），漏传一组等于把那一组的顺序清空，
 * 而"漏传"的表现是**静默**的：排序看起来生效了，刷新另一类任务后才发现顺序乱了。
 * 三个面板都必须传齐三组，故 `DragSortOptions` 的字段全为必填（不设默认值）。
 */
export function orderPayload(
  tasks: readonly { id: string }[],
  scripts: readonly { id: string }[],
  http: readonly { id: string }[],
): { all: string[]; scripts: string[]; http: string[] } {
  return {
    all: tasks.map((t) => t.id),
    scripts: scripts.map((s) => s.id),
    http: http.map((h) => h.id),
  };
}

/**
 * @param list 拖拽重排的目标列表（本视图持有的列表）
 * @param order 顺序持久化用的全量清单。B1：后端 order 接口会整体替换三组顺序，
 *   漏传的一组会被清空，因此三个视图都必须互传全量。
 */
export function useDragSort(list: Ref<TaskItem[]>, order: DragSortOptions) {
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
    // F10：拖出列表松手不触发 drop——拖拽期间已生效的交换在此补持久化，
    // 否则内存顺序与后端不一致，刷新后静默回退
    if (orderDirty) {
      orderDirty = false;
      void persistOrder();
    }
  }

  async function persistOrder(): Promise<void> {
    try {
      await tasksApi.order(
        orderPayload(order.tasks.value, order.scripts.value, order.http.value),
      );
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
