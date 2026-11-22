<script setup lang="ts">
// 开关组件（替代原 components.js 的 ToggleSwitch）。
// 可访问名：有 label 时经 aria-labelledby 指到标签 span（useId 保证多实例唯一）；
// 无 label 的行内用法由调用方传 aria-label，这里显式抽到按钮上（attrs 其余成员
// 仍按默认规则落在根 div，data-testid 挂祖先的既有选择器不受影响）。
import { computed, useAttrs, useId } from "vue";

const props = defineProps<{
  modelValue: boolean;
  label?: string;
  description?: string;
  disabled?: boolean;
}>();
const emit = defineEmits<{ "update:modelValue": [value: boolean] }>();

const attrs = useAttrs();
const labelId = useId();
const fallbackLabel = computed(() => attrs["aria-label"] as string | undefined);

function toggle(): void {
  if (props.disabled) return;
  emit("update:modelValue", !props.modelValue);
}
</script>

<template>
  <!-- 行级 @click 只是指针命中区扩展（点文本也能切）；键盘激活由内层原生
       button role="switch" 承担（Enter/Space 原生激活，@click.stop 防与行级
       叠加双发）。容器若再挂键盘监听属冗余且易与按钮激活事件失序，故
       no-static/click-events 两条在此按误报豁免 -->
  <!-- eslint-disable-next-line vuejs-accessibility/no-static-element-interactions, vuejs-accessibility/click-events-have-key-events -->
  <div class="toggle-row" :class="{ disabled }" @click="toggle">
    <div class="toggle-content">
      <span v-if="label" :id="labelId" class="toggle-label">{{ label }}</span>
      <span v-if="description" class="toggle-desc">{{ description }}</span>
    </div>
    <button
      type="button"
      role="switch"
      :aria-checked="modelValue"
      :aria-labelledby="label ? labelId : undefined"
      :aria-label="label ? undefined : fallbackLabel"
      :disabled="disabled"
      class="toggle-switch"
      :class="{ active: modelValue }"
      @click.stop="toggle"
    >
      <span class="toggle-knob"></span>
    </button>
  </div>
</template>
