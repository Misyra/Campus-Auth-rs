<script setup lang="ts">
// 顶栏（替代原 topbar.html）。
// 展示页面标题、未保存提示、WebSocket 重连状态、通知中心、监控开关。

import { computed } from "vue";
import { useRoute, useRouter } from "vue-router";
import IconApp from "./IconApp.vue";
import type { NotificationAction } from "../../api/types";
import { useConfig } from "../../composables/useConfig";
import { useStatus } from "../../composables/useStatus";
import { useNotifications } from "../../composables/useNotifications";
import { useWebSocket } from "../../composables/useWebSocket";
import { useUi } from "../../composables/useUi";

const route = useRoute();
const router = useRouter();
const { dirty } = useConfig();
const { status, busy } = useStatus();
const { notifications, unreadNotifications, showNotifications, toggleNotifications } = useNotifications();
const { wsReconnecting, wsRetryCount } = useWebSocket();
const { toggleMonitor } = useUi();

const pageTitle = computed(() => (route.meta.title as string) || "校园网认证");
const showDirty = computed(() => dirty.value && String(route.name).startsWith("settings"));

function onActionClick(action: NotificationAction | null): void {
  if (action?.page) {
    router.push({ name: action.page });
    showNotifications.value = false;
  }
}
</script>

<template>
  <header class="top-bar">
    <div class="top-title-area">
      <h1 class="page-title">{{ pageTitle }}</h1>
      <span v-if="showDirty" class="top-dirty-hint">
        <span class="top-dirty-dot"></span>未保存
      </span>
    </div>
    <div class="top-actions">
      <div v-if="wsReconnecting" class="ws-reconnect-bar ws-reconnect-inline">
        <span class="spinner"></span>
        重连中 (第 {{ wsRetryCount + 1 }} 次)
      </div>
      <div class="notification-wrapper">
        <button
          class="btn btn-icon-only"
          @click="toggleNotifications"
          title="通知历史"
          aria-haspopup="true"
          :aria-expanded="showNotifications"
        >
          <IconApp name="bell" />
          <span v-if="unreadNotifications > 0" class="notification-badge">{{ unreadNotifications > 9 ? "9+" : unreadNotifications }}</span>
        </button>
        <div v-if="showNotifications" class="notification-dropdown" role="menu">
          <div class="notification-header">
            <span>通知历史</span>
            <button class="btn btn-text btn-xs" @click="notifications.splice(0, notifications.length)">清空</button>
          </div>
          <div v-if="!notifications.length" class="notification-empty">暂无通知</div>
          <div
            v-for="n in notifications"
            :key="n.time + n.message"
            class="notification-item"
            :class="n.success ? 'notify-success' : 'notify-error'"
          >
            <div class="notification-item-header">
              <span v-if="n.label" class="notify-label">{{ n.label }}</span>
              <span class="time">{{ n.time }}</span>
            </div>
            <div class="notification-item-body">
              {{ n.message }}
              <a v-if="n.action" class="notify-action" @click.prevent="onActionClick(n.action)">{{ n.action.label }}</a>
            </div>
          </div>
        </div>
      </div>
      <button
        class="btn btn-primary"
        @click="toggleMonitor"
        :disabled="busy.monitor"
        :title="status.monitoring ? '停止网络监控和自动登录' : '开始监控网络，断网时自动登录'"
      >
        <span v-if="busy.monitor" class="spinner" style="border-top-color: var(--on-accent)"></span>
        <IconApp v-else class="btn-icon" :name="status.monitoring ? 'pause' : 'play'" />
        {{ busy.monitor ? "处理中..." : (status.monitoring ? "停止监控" : "启动监控") }}
      </button>
    </div>
  </header>
</template>
