<script setup lang="ts">
// 开关组件（替代原 components.js 的 ToggleSwitch）。

const props = defineProps<{
  modelValue: boolean;
  label?: string;
  description?: string;
  disabled?: boolean;
}>();
const emit = defineEmits<{ "update:modelValue": [value: boolean] }>();

function toggle(): void {
  if (props.disabled) return;
  emit("update:modelValue", !props.modelValue);
}
</script>

<template>
  <div class="toggle-row" :class="{ disabled }" @click="toggle">
    <div class="toggle-content">
      <span v-if="label" class="toggle-label">{{ label }}</span>
      <span v-if="description" class="toggle-desc">{{ description }}</span>
    </div>
    <button
      type="button"
      role="switch"
      :aria-checked="modelValue"
      :disabled="disabled"
      class="toggle-switch"
      :class="{ active: modelValue }"
      @click.stop="toggle"
    >
      <span class="toggle-knob"></span>
    </button>
  </div>
</template>
