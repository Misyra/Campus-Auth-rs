<script setup lang="ts">
/**
 * 内联 SVG 图标统一组件（批次六小尾巴）。
 *
 * 此前同一图标的 `<svg>` 拷贝散落在多个视图（check ×14、close ×6、plus ×4…），
 * stroke-width 与尺寸已开始漂移。收敛为 name → path 字典后：
 * - 图形单点维护，视觉一致；
 * - 模板可读性提升（`<IconApp name="check" />`）。
 *
 * 根元素透传 attrs（class / width / height 等由调用方按需附加，
 * 与原写法等价——原 svg 标签上的非图形属性会原样落到本组件根节点）。
 */
import { computed, type ComponentObjectPropsOptions } from "vue";

/** 图标注册表：name → svg 内部标记（stroke 继承根元素 currentColor） */
const ICONS = {
  check: '<polyline points="20 6 9 17 4 12"/>',
  close: '<line x1="18" y1="6" x2="6" y2="18"/><line x1="6" y1="6" x2="18" y2="18"/>',
  plus: '<line x1="12" y1="5" x2="12" y2="19"/><line x1="5" y1="12" x2="19" y2="12"/>',
  refresh:
    '<path d="M21 12a9 9 0 11-6.219-8.56"/>',
  clock: '<circle cx="12" cy="12" r="10"/><polyline points="12 6 12 12 16 14"/>',
  download:
    '<polyline points="23 4 23 10 17 10"/><path d="M20.49 15a9 9 0 1 1-2.12-9.36L23 10"/>',
  upload:
    '<path d="M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4"/><polyline points="7 10 12 15 17 10"/><line x1="12" y1="15" x2="12" y2="3"/>',
  "chevron-down": '<polyline points="6 9 12 15 18 9" />',
  image:
    '<rect x="3" y="3" width="18" height="18" rx="2"/><circle cx="8.5" cy="8.5" r="1.5"/><polyline points="21 15 16 10 5 21"/>',
  wifi: '<path d="M5 12.55a11 11 0 0 1 14.08 0"/><path d="M1.42 9a16 16 0 0 1 21.16 0"/><path d="M8.53 16.11a6 6 0 0 1 6.95 0"/><line x1="12" y1="20" x2="12.01" y2="20"/>',
  trash:
    '<polyline points="3 6 5 6 21 6"/><path d="M19 6v14a2 2 0 0 1-2 2H7a2 2 0 0 1-2-2V6m3 0V4a2 2 0 0 1 2-2h4a2 2 0 0 1 2 2v2"/>',
  "x-circle":
    '<circle cx="12" cy="12" r="10"/><line x1="15" y1="9" x2="9" y2="15"/><line x1="9" y1="9" x2="15" y2="15"/>',
} as const;

export type IconName = keyof typeof ICONS;

const props = defineProps<{ name: IconName }>();

const markup = computed(() => ICONS[props.name]);

defineOptions({ inheritAttrs: true });
// 供模板类型检查的占位（避免未使用告警）
const _typed: ComponentObjectPropsOptions = {};
void _typed;
</script>

<template>
  <svg
    viewBox="0 0 24 24"
    fill="none"
    stroke="currentColor"
    stroke-width="2"
    aria-hidden="true"
    v-html="markup"
  />
</template>
