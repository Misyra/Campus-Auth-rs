<script setup lang="ts">
/** 设置行：左侧名称与说明，右侧控件；附属设置归入同一行，窄屏按阅读顺序排列。 */
defineProps<{ label?: string; description?: string }>();
</script>

<template>
  <div class="setting-row">
    <div class="setting-row-copy">
      <div class="setting-row-title">
        <slot name="label">{{ label }}</slot>
      </div>
      <div v-if="description || $slots.description" class="setting-row-description">
        <p v-if="description">{{ description }}</p>
        <slot name="description" />
      </div>
    </div>
    <div v-if="$slots.default" class="setting-row-control form-group">
      <slot />
    </div>
    <div v-if="$slots.details" class="setting-row-details">
      <slot name="details" />
    </div>
  </div>
</template>
