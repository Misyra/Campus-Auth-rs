<script setup lang="ts">
/**
 * 设置行：左侧名称（+ `?` 帮助气泡），右侧控件；附属设置归入同一行，窄屏按阅读顺序排列。
 *
 * 「设置行文字减法」（Demo A 全收版）：行说明不再常驻渲染——`description`（短说明）
 * 与 `help`（详细说明）统一收进标题行的 `?` 帮助气泡（FieldHelp，悬停展开 / 点击钉住），
 * 气泡文本 = help + description 按序换行拼接，只有其一就只显示其一。设置搜索不受影响：
 * schema 的 hint 是纯数据匹配语料（settingsSearch），与渲染层解耦。
 *
 * 此前的 F14 aria-describedby 体系（descriptionId + 常驻说明 <p>）随灰字退场一并移除：
 * 气泡展开期间由 Reka Tooltip 自带 aria-describedby 关联触发器与 role=tooltip 内容。
 */
import { computed } from "vue";
import FieldHelp from "./FieldHelp.vue";

const props = defineProps<{ label?: string; description?: string; help?: string }>();

/** 气泡文本：详细说明（help）在前、短说明（description）在后，换行分段 */
const helpText = computed(() =>
  [props.help, props.description]
    .filter((part) => typeof part === "string" && part.length > 0)
    .join("\n"),
);
</script>

<template>
  <div class="setting-row">
    <div class="setting-row-copy">
      <div class="setting-row-title">
        <slot name="label">{{ label }}</slot>
        <FieldHelp v-if="helpText" :text="helpText" />
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
