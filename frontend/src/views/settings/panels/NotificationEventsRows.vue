<script setup lang="ts">
/** 设置 · 触发事件面板体（schema 槽位）：事件类型 chip 多选组（checkbox 数组语义）。 */
import { computed } from "vue";
import { useConfig } from "@/composables/useConfig";
import type { NotificationEventType } from "@/api/types";

const config = useConfig();
const notification = computed(() => config.config.notification);

const EVENT_OPTIONS: { type: NotificationEventType; label: string; hint: string }[] = [
  { type: "login_succeeded", label: "登录成功", hint: "每次自动/手动登录成功" },
  { type: "login_failed", label: "登录失败", hint: "推荐开启：登录尝试失败（含原因）" },
  { type: "network_down", label: "网络断开", hint: "推荐开启：探测由通转断" },
  { type: "network_up", label: "网络恢复", hint: "探测由断转通" },
  { type: "task_finished", label: "任务完成", hint: "自定义任务执行成功" },
  { type: "task_failed", label: "任务失败", hint: "自定义任务执行失败" },
  { type: "update_available", label: "发现更新", hint: "检查到新版本" },
  { type: "environment_ready", label: "环境就绪", hint: "Python/uv 环境引导完成" },
  { type: "profile_switched", label: "档案切换", hint: "自动/手动切换活跃方案" },
];

/** 复选框数组 v-model 的成员判定/增删（checkbox 多选语义） */
function hasEvent(type: NotificationEventType): boolean {
  return notification.value.events.includes(type);
}

function toggleEvent(type: NotificationEventType, checked: boolean): void {
  const events = notification.value.events;
  if (checked && !events.includes(type)) events.push(type);
  if (!checked) {
    const index = events.indexOf(type);
    if (index >= 0) events.splice(index, 1);
  }
}
</script>

<template>
  <div>
    <p class="hint">勾选需要推送的事件类型；全不勾选等于不推送任何通知。</p>
    <div class="notif-event-chips" role="group" aria-label="触发事件类型">
      <label
        v-for="event in EVENT_OPTIONS"
        :key="event.type"
        class="notif-event-chip"
        :class="{ 'notif-event-chip--on': hasEvent(event.type) }"
        :title="event.hint"
      >
        <input
          type="checkbox"
          :checked="hasEvent(event.type)"
          :aria-label="event.label"
          @change="toggleEvent(event.type, ($event.target as HTMLInputElement).checked)"
        />
        <span>{{ event.label }}</span>
      </label>
    </div>
  </div>
</template>
