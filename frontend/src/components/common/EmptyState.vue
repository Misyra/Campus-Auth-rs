<script setup lang="ts">
/**
 * 空状态占位（base 组件）：样式复用全局 .empty-state 系（styles/components/misc.css），
 * 本组件只做结构封装——图标 + 主文案 + 说明 + 动作区的排布单点化。
 *
 * 此前同一角色在 10 个文件里手写了 13 份 `div.empty-state`：主文案行有的挂
 * .empty-title（强调排版）、有的只是裸 <span>/<p>（跟随容器弱化排版），同一视觉
 * 角色两套渲染，切页时标题忽明忽暗。组件化后主文案恒走 .empty-title 口径
 * （--text-base / --text-primary），这是本次收口有意的排版归一。
 *
 * 变体与工具类沿用 misc.css：dashed → .empty-state--dashed（可拖入/待填充）、
 * small → .empty-state--sm（通知下拉等小容器）；说明文字恒为 .empty-desc、
 * 动作区恒为 .empty-actions（flex 居中 + 上间距），无需调用方手拼类名。
 */
import IconApp from "./IconApp.vue";
import type { IconName } from "./IconApp.vue";

withDefaults(
  defineProps<{
    /** 主图标（IconApp 名称）；不传且未提供 #icon 插槽时不渲染图标位 */
    icon?: IconName;
    /** 图标线条粗细，透传 IconApp（默认 2，空状态装饰位常用 1.5） */
    strokeWidth?: number;
    /** 主文案；复杂内容（含内联标记）用 #title 插槽 */
    title?: string;
    /** 次级说明文字；复杂内容用 #description 插槽 */
    description?: string;
    /** 虚线框变体（可拖入 / 待填充场景） */
    dashed?: boolean;
    /** 紧凑变体（通知下拉等小容器场景） */
    small?: boolean;
  }>(),
  { dashed: false, small: false, icon: undefined, strokeWidth: undefined, title: undefined, description: undefined },
);
</script>

<template>
  <div class="empty-state" :class="{ 'empty-state--dashed': dashed, 'empty-state--sm': small }">
    <slot name="icon">
      <IconApp v-if="icon" :name="icon" :stroke-width="strokeWidth" />
    </slot>
    <strong v-if="title || $slots.title" class="empty-title">
      <slot name="title">{{ title }}</slot>
    </strong>
    <span v-if="description || $slots.description" class="empty-desc">
      <slot name="description">{{ description }}</slot>
    </span>
    <!-- 默认插槽兜底：纯文本占位（通知下拉）或结构特例（自定义图标位等）直排，
         位于标题/说明之后、动作区之前，与既有手写版 DOM 顺序一致 -->
    <slot />
    <div v-if="$slots.actions" class="empty-actions">
      <slot name="actions" />
    </div>
  </div>
</template>
