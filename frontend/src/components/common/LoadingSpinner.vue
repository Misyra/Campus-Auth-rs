<script setup lang="ts">
// 加载指示器。配合 .loading-state 布局使用（替代原 spinner 内联节点）。

withDefaults(
  defineProps<{
    label?: string;
    size?: "sm" | "md" | "lg";
  }>(),
  {
    label: "",
    size: "md",
  },
);
</script>

<template>
  <div class="loading-state">
    <span class="spinner" :class="{ 'spinner-lg': size === 'lg' }"></span>
    <span v-if="label" class="loading-label">{{ label }}</span>
  </div>
</template>
