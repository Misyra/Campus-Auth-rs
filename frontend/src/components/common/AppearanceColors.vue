<script setup lang="ts">
/** 通用外观色板：明确选择状态、默认入口与可触控的自定义颜色管理。 */
import { computed } from "vue";
import type { CustomColors } from "@/utils/appearance-types";
import { MONO_ACCENT } from "@/utils/constants";
import { pickOnColor } from "@/utils/formatters";
import { validHex } from "@/utils/appearance";
import { useAppearance } from "@/composables/useAppearance";
import { useCustomColors } from "@/composables/useCustomColors";
import IconApp from "./IconApp.vue";
import SettingsRow from "./SettingsRow.vue";

const props = defineProps<{ type: keyof CustomColors; label: string; defaultLabel?: string }>();
const { appearance, getEffectiveTheme } = useAppearance();
const { getColorList, pickCustomColor, onCustomColorPicked, onColorLongPress } = useCustomColors();
const fields = { accent: "accent_color", bg: "background_color", sidebar: "sidebar_color", sidebar_accent: "sidebar_accent" } as const;
const current = computed({ get: () => appearance[fields[props.type]], set: (value: string) => { appearance[fields[props.type]] = value; } });
const colors = computed(() => getColorList(props.type));
const presets = computed(() => colors.value.filter((color) => !color.custom));
const custom = computed(() => colors.value.filter((color) => color.custom));
// 精简默认色板后仍显示已选的旧颜色，避免用户偏好被改写或失去选中标记。
const extraColors = computed(() => {
  const selected = current.value;
  return validHex(selected) && !colors.value.some((color) => color.value.toLowerCase() === selected.toLowerCase())
    ? [...custom.value, { value: selected, label: "当前颜色", custom: false }]
    : custom.value;
});
function displayColor(value: string): string { return value === MONO_ACCENT ? (getEffectiveTheme() === "light" ? "#000000" : "#ffffff") : value; }
</script>

<template>
  <SettingsRow :label="label" :description="current === MONO_ACCENT ? '日间黑 / 夜间白' : current || defaultLabel">
    <template #label>
      <span>{{ label }}</span>
      <button
        v-if="defaultLabel"
        type="button"
        class="appearance-color-default"
        :class="{ active: !current }"
        :aria-pressed="!current"
        @click="current = ''"
      >
        {{ defaultLabel }}
      </button>
    </template>
    <div class="appearance-colors" role="group" :aria-label="label">
      <button
        v-for="color in presets"
        :key="color.value"
        type="button"
        class="appearance-color-btn"
        :class="{ active: current.toLowerCase() === color.value.toLowerCase(), custom: color.custom }"
        :style="{ background: displayColor(color.value) }"
        :title="color.label"
        :aria-label="`${label}：${color.label}`"
        :aria-pressed="current.toLowerCase() === color.value.toLowerCase()"
        @click="current = color.value"
        @contextmenu.prevent="color.custom && onColorLongPress(type, color.value)"
      >
        <IconApp
          v-if="current.toLowerCase() === color.value.toLowerCase()"
          name="check"
          class="icon-sm"
          :style="{ color: pickOnColor(displayColor(color.value)) }"
        />
      </button>
      <button
        type="button"
        class="appearance-color-btn appearance-color-add"
        @click="pickCustomColor(type)"
        :title="`自定义${label}`"
        :aria-label="`自定义${label}`"
      >
        +
      </button>
    </div>
    <div v-if="extraColors.length" class="appearance-extra-colors" role="group" :aria-label="`${label}其他颜色`">
      <button v-for="color in extraColors" :key="color.value" type="button" class="appearance-color-btn"
        :class="{ active: current.toLowerCase() === color.value.toLowerCase(), custom: color.custom }"
        :style="{ background: color.value }" :title="color.label" :aria-label="`${label}：${color.label}`"
        :aria-pressed="current.toLowerCase() === color.value.toLowerCase()" @click="current = color.value"
        @contextmenu.prevent="color.custom && onColorLongPress(type, color.value)">
        <IconApp v-if="current.toLowerCase() === color.value.toLowerCase()" name="check" class="icon-sm"
          :style="{ color: pickOnColor(color.value) }" />
      </button>
    </div>
    <input
      type="color"
      :data-color-picker="type"
      class="sr-only"
      tabindex="-1"
      :aria-label="`选择自定义${label}`"
      :value="displayColor(current || (getEffectiveTheme() === 'light' ? '#eef2f7' : '#0f172a'))"
      @change="onCustomColorPicked(type, $event)"
    />
    <details v-if="custom.length" class="appearance-custom-manager">
      <summary>管理自定义颜色（{{ custom.length }}）</summary>
      <div class="appearance-custom-list">
        <button
          v-for="color in custom"
          :key="color.value"
          type="button"
          class="appearance-custom-remove"
          @click="onColorLongPress(type, color.value)"
          :aria-label="`删除${label} ${color.value}`"
        >
          <i :style="{ background: color.value }"></i>
          {{ color.value }}
          <IconApp name="close" class="icon-sm" />
        </button>
      </div>
    </details>
  </SettingsRow>
</template>
