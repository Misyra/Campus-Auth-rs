<script setup lang="ts">
import IconApp from "@/components/common/IconApp.vue";
// 自定义下拉选择组件（替代原 components.js 的 CustomSelect）。
// 支持键盘导航（↑↓ Enter Esc 空格）、点击外部关闭、无障碍属性。

import { ref, computed, nextTick, onBeforeUnmount } from "vue";

let selectIdCounter = 0;
const selectUid = `cs-${++selectIdCounter}`;

interface SelectOption {
  value: string;
  label: string;
}

const props = withDefaults(
  defineProps<{
    modelValue: string;
    options: SelectOption[];
    placeholder?: string;
    compact?: boolean;
    disabled?: boolean;
  }>(),
  {
    placeholder: "请选择...",
    compact: false,
    disabled: false,
  },
);
const emit = defineEmits<{
  "update:modelValue": [value: string];
  change: [value: string];
}>();

const open = ref(false);
const activeIndex = ref(-1);
const triggerRef = ref<HTMLButtonElement | null>(null);
const rootRef = ref<HTMLElement | null>(null);

const selectedLabel = computed(() => {
  const opt = props.options.find((o) => o.value === props.modelValue);
  return opt ? opt.label : "";
});

function toggle(): void {
  if (props.disabled) return;
  open.value = !open.value;
  if (open.value) {
    activeIndex.value = props.options.findIndex((o) => o.value === props.modelValue);
    nextTick(() => {
      scrollToActive();
      document.addEventListener("mousedown", onDocClick);
    });
  } else {
    document.removeEventListener("mousedown", onDocClick);
  }
}

function select(opt: SelectOption): void {
  emit("update:modelValue", opt.value);
  emit("change", opt.value);
  open.value = false;
  document.removeEventListener("mousedown", onDocClick);
  triggerRef.value?.focus();
}

function onDocClick(e: MouseEvent): void {
  if (rootRef.value && !rootRef.value.contains(e.target as Node)) {
    open.value = false;
    document.removeEventListener("mousedown", onDocClick);
  }
}

function onKeydown(e: KeyboardEvent): void {
  if (!open.value) {
    if (["ArrowDown", "ArrowUp", "Enter", " "].includes(e.key)) {
      e.preventDefault();
      toggle();
    }
    return;
  }
  switch (e.key) {
    case "ArrowDown":
      e.preventDefault();
      activeIndex.value = Math.min(activeIndex.value + 1, props.options.length - 1);
      scrollToActive();
      break;
    case "ArrowUp":
      e.preventDefault();
      activeIndex.value = Math.max(activeIndex.value - 1, 0);
      scrollToActive();
      break;
    case "Enter":
    case " ":
      e.preventDefault();
      if (activeIndex.value >= 0 && activeIndex.value < props.options.length) {
        select(props.options[activeIndex.value]);
      }
      break;
    case "Escape":
      e.preventDefault();
      open.value = false;
      document.removeEventListener("mousedown", onDocClick);
      triggerRef.value?.focus();
      break;
  }
}

function scrollToActive(): void {
  nextTick(() => {
    const el = rootRef.value?.querySelector(".custom-select-option.active");
    el?.scrollIntoView({ block: "nearest" });
  });
}

onBeforeUnmount(() => {
  document.removeEventListener("mousedown", onDocClick);
});
</script>

<template>
  <div ref="rootRef" class="custom-select" :class="{ open, compact, disabled }">
    <button
      ref="triggerRef"
      type="button"
      class="custom-select-trigger"
      role="combobox"
      :aria-expanded="open"
      aria-haspopup="listbox"
      :aria-activedescendant="open && activeIndex >= 0 ? selectUid + '-opt-' + activeIndex : undefined"
      @click="toggle"
      @keydown="onKeydown"
    >
      <span v-if="!selectedLabel" class="custom-select-placeholder">{{ placeholder }}</span>
      <span v-else>{{ selectedLabel }}</span>
      <IconApp name="chevron-down" class="custom-select-arrow" stroke-linecap="round" stroke-linejoin="round" />
    </button>
    <div v-if="open" class="custom-select-dropdown" role="listbox">
      <div
        v-for="(opt, i) in options"
        :id="selectUid + '-opt-' + i"
        :key="opt.value"
        class="custom-select-option"
        role="option"
        :aria-selected="opt.value === modelValue"
        :class="{ selected: opt.value === modelValue, active: i === activeIndex }"
        @mousedown.prevent="select(opt)"
        @mouseenter="activeIndex = i"
      >
        {{ opt.label }}
      </div>
    </div>
  </div>
</template>
