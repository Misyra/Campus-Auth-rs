<script setup lang="ts">
/** 设置 · 免打扰时段面板体（schema 槽位）：开关 + 嵌套 time 输入（与后端时/分字段互转）。 */
import SettingsRow from "@/components/common/SettingsRow.vue";
import { computed } from "vue";
import { useConfig } from "@/composables/useConfig";

const config = useConfig();
const notification = computed(() => config.config.notification);

// ===== 免打扰时段（type=time 与 后端 时/分 字段互转） =====

function pad2(value: number): string {
  return String(value).padStart(2, "0");
}

const quietStart = computed<string>({
  get: () => `${pad2(notification.value.quiet_start_hour)}:${pad2(notification.value.quiet_start_minute)}`,
  set: (value: string) => {
    const [h, m] = value.split(":").map(Number);
    if (Number.isFinite(h)) notification.value.quiet_start_hour = Math.min(23, Math.max(0, h));
    if (Number.isFinite(m)) notification.value.quiet_start_minute = Math.min(59, Math.max(0, m));
  },
});

const quietEnd = computed<string>({
  get: () => `${pad2(notification.value.quiet_end_hour)}:${pad2(notification.value.quiet_end_minute)}`,
  set: (value: string) => {
    const [h, m] = value.split(":").map(Number);
    if (Number.isFinite(h)) notification.value.quiet_end_hour = Math.min(23, Math.max(0, h));
    if (Number.isFinite(m)) notification.value.quiet_end_minute = Math.min(59, Math.max(0, m));
  },
});
</script>

<template>
  <div>
    <SettingsRow
      help="免打扰期间照常记录事件，只是不推送。开始与结束时间相同视为全天免打扰。"
      description="时段内事件只记日志不推送，支持跨零点（如 22:00–08:00）"
    >
      <template #label>启用免打扰时段</template>
      <label class="toggle setting-row-switch">
        <input
          type="checkbox"
          role="switch"
          :aria-checked="notification.quiet_enabled"
          v-model="notification.quiet_enabled"
        />
        <span class="toggle-slider"></span>
        <span class="sr-only">启用免打扰时段</span>
      </label>
      <template #details>
        <SettingsRow description="使用 24 小时制">
          <template #label>
            <label for="settings-notification-quiet-start">开始时间</label>
          </template>
          <input
            id="settings-notification-quiet-start"
            v-model="quietStart"
            type="time"
            :disabled="!notification.quiet_enabled"
          />
        </SettingsRow>
        <SettingsRow description="支持次日时间，如 22:00 开始、08:00 结束">
          <template #label>
            <label for="settings-notification-quiet-end">结束时间</label>
          </template>
          <input
            id="settings-notification-quiet-end"
            v-model="quietEnd"
            type="time"
            :disabled="!notification.quiet_enabled"
          />
        </SettingsRow>
      </template>
    </SettingsRow>
  </div>
</template>
