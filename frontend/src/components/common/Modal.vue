<script setup lang="ts">
// 通用模态框。使用 Teleport 挂载到 body，支持标题、尺寸、footer 插槽、
// ESC 关闭和基础 Focus Trap（Tab 键在模态框内循环）。

import { ref, watch, onBeforeUnmount, nextTick } from "vue";
import IconApp from "./IconApp.vue";

const props = withDefaults(
  defineProps<{
    open: boolean;
    title?: string;
    size?: "default" | "lg" | "xl";
    /** 是否允许点击遮罩关闭（默认 true；免责声明等需显式操作的场景设为 false） */
    closeOnOverlay?: boolean;
    /** 是否允许 ESC 键关闭（默认 true；调试面板等误触代价高的场景设为 false） */
    closeOnEsc?: boolean;
    /** 沉浸预览模式：加深遮罩（截图放大/背景预览等大图场景） */
    preview?: boolean;
  }>(),
  {
    title: "",
    size: "default",
    closeOnOverlay: true,
    closeOnEsc: true,
    preview: false,
  },
);

const emit = defineEmits<{ close: [] }>();

const containerRef = ref<HTMLElement | null>(null);

function onClose(): void {
  emit("close");
}

function onOverlayClick(): void {
  if (props.closeOnOverlay) onClose();
}

// ESC 键关闭
function onKeydown(e: KeyboardEvent): void {
  if (e.key === "Escape") {
    e.stopPropagation();
    if (props.closeOnEsc) onClose();
  }
}

// 基础 Focus Trap：Tab / Shift+Tab 在模态框内循环
function onTrapKeydown(e: KeyboardEvent): void {
  if (e.key !== "Tab") return;
  const container = containerRef.value;
  if (!container) return;
  const focusable = container.querySelectorAll<HTMLElement>(
    'button:not([disabled]), [href], input:not([disabled]), select:not([disabled]), textarea:not([disabled]), [tabindex]:not([tabindex="-1"])',
  );
  if (focusable.length === 0) return;
  const first = focusable[0];
  const last = focusable[focusable.length - 1];
  if (e.shiftKey) {
    if (document.activeElement === first) {
      e.preventDefault();
      last.focus();
    }
  } else {
    if (document.activeElement === last) {
      e.preventDefault();
      first.focus();
    }
  }
}

// 打开时聚焦第一个可聚焦元素 + 锁定背景滚动（多弹窗叠加时计数，全部关闭才还原）
let modalOpenCount = 0;

function lockBodyScroll(): void {
  modalOpenCount += 1;
  if (modalOpenCount === 1) document.body.style.overflow = "hidden";
}

function unlockBodyScroll(): void {
  modalOpenCount = Math.max(0, modalOpenCount - 1);
  if (modalOpenCount === 0) document.body.style.overflow = "";
}

// 打开时聚焦第一个可聚焦元素
watch(
  () => props.open,
  async (val) => {
    if (val) {
      lockBodyScroll();
      await nextTick();
      const firstFocusable = containerRef.value?.querySelector<HTMLElement>(
        'button:not([disabled]), [href], input:not([disabled]), select:not([disabled]), textarea:not([disabled])',
      );
      firstFocusable?.focus();
    } else {
      unlockBodyScroll();
    }
  },
  { immediate: true },
);

onBeforeUnmount(() => {
  if (props.open) unlockBodyScroll();
});
</script>

<template>
  <Teleport to="body">
    <Transition name="modal-fade">
      <div v-if="open" class="modal-overlay" :class="{ 'modal-overlay--preview': preview }" @click.self="onOverlayClick" @keydown="onKeydown">
        <div
          ref="containerRef"
          class="modal-container"
          :class="{ 'modal-lg': size === 'lg', 'modal-xl': size === 'xl' }"
          role="dialog"
          aria-modal="true"
          @keydown="onTrapKeydown"
        >
          <div class="modal-header">
            <h3>{{ title }}</h3>
            <button class="btn btn-icon-only" @click="onClose" aria-label="关闭" title="关闭">
              <IconApp name="close" />
            </button>
          </div>
          <div class="modal-body">
            <slot />
          </div>
          <div v-if="$slots.footer" class="modal-footer">
            <slot name="footer" />
          </div>
        </div>
      </div>
    </Transition>
  </Teleport>
</template>
