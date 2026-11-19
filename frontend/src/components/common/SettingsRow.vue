<script setup lang="ts">
/**
 * 设置行：左侧名称与说明，右侧控件；附属设置归入同一行，窄屏按阅读顺序排列。
 *
 * 说明文字与控件的编程关联（F14 aria-describedby 体系）：description prop 渲染
 * 的 <p> 支持经 `description-id` prop 指定 id，控件槽内的控件据此挂
 * aria-describedby；#description 插槽内容由调用方自带 id（互斥分支可共用同一
 * id，缺失分支被屏读器忽略）。说明只是补充文案时不必关联——不强制全量接线。
 */
defineProps<{ label?: string; description?: string; descriptionId?: string }>();
</script>

<template>
  <div class="setting-row">
    <div class="setting-row-copy">
      <div class="setting-row-title">
        <slot name="label">{{ label }}</slot>
      </div>
      <div v-if="description || $slots.description" class="setting-row-description">
        <p v-if="description" :id="descriptionId">{{ description }}</p>
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
