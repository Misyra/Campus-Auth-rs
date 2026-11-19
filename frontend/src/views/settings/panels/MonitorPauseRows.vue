<script setup lang="ts">
/** 设置 · 暂停时段面板体（schema 槽位）：开关 + 嵌套的开始/结束时刻行（受开关禁用联动）。 */
import SettingsRow from "@/components/common/SettingsRow.vue";
import FieldHelp from "@/components/common/FieldHelp.vue";
import { useConfig } from "@/composables/useConfig";

const config = useConfig();
</script>

<template>
  <div>
    <SettingsRow description="每天在指定时段暂停检测和自动登录">
      <template #label>
        启用暂停时段
        <FieldHelp text="启用后在该时段内暂停检测与登录，适用于定时断网时段。" />
      </template>
      <label class="toggle setting-row-switch">
        <input type="checkbox" v-model="config.config.pause.enabled" />
        <span class="toggle-slider"></span>
        <span class="sr-only">启用暂停时段</span>
      </label>
      <template #details>
        <SettingsRow description="使用 24 小时制，范围 0–23">
          <template #label>
            <label for="settings-pause-start">开始时间（时）</label>
          </template>
          <input
            id="settings-pause-start"
            v-model.number="config.config.pause.start_hour"
            type="number"
            min="0"
            max="23"
            :disabled="!config.config.pause.enabled"
          />
        </SettingsRow>
        <SettingsRow description="支持跨天，如 22 点开始、次日 6 点结束">
          <template #label>
            <label for="settings-pause-end">结束时间（时）</label>
          </template>
          <input
            id="settings-pause-end"
            v-model.number="config.config.pause.end_hour"
            type="number"
            min="0"
            max="23"
            :disabled="!config.config.pause.enabled"
          />
        </SettingsRow>
      </template>
    </SettingsRow>
  </div>
</template>
