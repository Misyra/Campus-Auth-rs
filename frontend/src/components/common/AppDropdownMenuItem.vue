<script setup lang="ts">
/**
 * 行操作菜单项：包一层 Reka DropdownMenuItem 并以 `as="button"` 渲染——
 * 关键在让现有 `.tsk-menu button` 选择器（内边距/悬停/禁用态）原样命中，
 * 共享样式不用改。Reka 负责标 role="menuitem" 并响应 Enter/Space 选中；
 * 选中后菜单自动关闭（透传给根的 update:open(false)），宿主只管业务动作。
 *
 * 危险项仍由宿主手动挂 `class="tsk-menu-danger"`（CSS 选择器
 * `.tsk-menu .tsk-menu-danger` 不需改）。
 */
import { DropdownMenuItem } from "reka-ui";

defineProps<{
  /** 禁用项（如进行中的复制/导出）：落到 button 的 disabled 属性上 */
  disabled?: boolean;
}>();

const emit = defineEmits<{ select: [] }>();
</script>

<template>
  <DropdownMenuItem as="button" type="button" :disabled="disabled" @select="emit('select')">
    <slot />
  </DropdownMenuItem>
</template>
