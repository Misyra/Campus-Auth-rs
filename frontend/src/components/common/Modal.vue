<script setup lang="ts">
// 通用模态框（Reka Dialog headless）。开合状态机、Esc、遮罩点击、焦点陷阱、
// 关闭后焦点归还触发器、body 滚动锁全部由 reka-ui Dialog 承担（E1 的手写
// 触发器记录/还原逻辑随之退役）；本组件只保留既有 props/emits
// API 与 .modal-* 外观（样式见 styles/components/modal.css）。
//
// 结构：DialogOverlay(as-child) 包住原 .modal-overlay（遮罩 + flex 居中容器），
// DialogContent(as-child) 落在原 .modal-container 面板上——DOM 与迁移前逐层
// 一致，role/aria 位置不变，遮罩点击=面板外按下，走 reka 的 dismiss 通道。
// force-mount 供 Transition 离场渐隐期间维持 DOM：reka 的 presence 依据 open
// 状态会在关闭瞬间卸载节点，会把还在渐隐的面板/遮罩提前抽走；滚动锁与焦点
// 归还同样依据 open 状态（而非节点存活），时序不受影响。

import { DialogContent, DialogOverlay, DialogPortal, DialogRoot, DialogTitle } from "reka-ui";
import IconApp from "./IconApp.vue";

const props = withDefaults(
  defineProps<{
    open: boolean;
    title?: string;
    size?: "default" | "lg" | "xl" | "xxl";
    /** 是否允许点击遮罩关闭（默认 true；免责声明等需显式操作的场景设为 false） */
    closeOnOverlay?: boolean;
    /** 是否允许 ESC 键关闭（默认 true；调试面板等误触代价高的场景设为 false） */
    closeOnEsc?: boolean;
    /** 保存等关键操作在途时统一禁用关闭入口，避免按钮看似可点却无效。 */
    closeDisabled?: boolean;
    /** 沉浸预览模式：加深遮罩（截图放大/背景预览等大图场景） */
    preview?: boolean;
  }>(),
  {
    title: "",
    size: "default",
    closeOnOverlay: true,
    closeOnEsc: true,
    closeDisabled: false,
    preview: false,
  },
);

const emit = defineEmits<{ close: [] }>();

function onClose(): void {
  if (props.closeDisabled) return;
  emit("close");
}

// reka 的各 dismiss 路径（Esc / 遮罩点击）在受控模式下统一汇为
// update:open(false)，宿主不响应则弹窗保持打开——closeDisabled 在此统一否决
function onOpenChange(value: boolean): void {
  if (value || props.closeDisabled) return;
  emit("close");
}

function onEscapeKeyDown(event: KeyboardEvent): void {
  if (!props.closeOnEsc) event.preventDefault();
}

function onPointerDownOutside(event: Event): void {
  if (!props.closeOnOverlay) event.preventDefault();
}
</script>

<template>
  <DialogRoot :open="open" @update:open="onOpenChange">
    <DialogPortal>
      <Transition name="modal-fade">
        <DialogOverlay v-if="open" force-mount as-child>
          <div class="modal-overlay" :class="{ 'modal-overlay--preview': preview }">
            <DialogContent
              as-child
              force-mount
              aria-modal="true"
              :aria-describedby="undefined"
              @escape-key-down="onEscapeKeyDown"
              @pointer-down-outside="onPointerDownOutside"
            >
              <div
                class="modal-container"
                :class="{ 'modal-lg': size === 'lg', 'modal-xl': size === 'xl', 'modal-xxl': size === 'xxl' }"
              >
                <div class="modal-header">
                  <DialogTitle as="h3">{{ title }}</DialogTitle>
                  <button class="btn btn-icon-only" :disabled="closeDisabled" aria-label="关闭" title="关闭" @click="onClose">
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
            </DialogContent>
          </div>
        </DialogOverlay>
      </Transition>
    </DialogPortal>
  </DialogRoot>
</template>
