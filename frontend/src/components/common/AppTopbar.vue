<script setup lang="ts">
// 顶栏（替代原 topbar.html）。
// 展示页面标题、未保存提示、WebSocket 重连状态、通知中心、监控开关。

import { computed, onBeforeUnmount, onMounted, ref } from "vue";
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
const { wsReconnecting, wsRetryCount, wsDisconnectReason, retryNow } = useWebSocket();
const { toggleMonitor } = useUi();

/** 重连条文案：首轮为通用重连中，连续失败后按原因给出检查/重启指引 */
const reconnectHint = computed(() => {
  if (wsDisconnectReason.value === "unreachable") {
    return "后端无响应，请检查后端是否已启动；若刚更新/重启过可点重试";
  }
  if (wsDisconnectReason.value === "unauthorized") {
    return "疑似后端重启，正在刷新凭证重连…";
  }
  return `重连中 (第 ${wsRetryCount.value + 1} 次)`;
});

const pageTitle = computed(() => (route.meta.title as string) || "校园网认证");
const showDirty = computed(() => dirty.value && String(route.name).startsWith("settings"));

function onActionClick(action: NotificationAction | null): void {
  if (action?.page) {
    router.push({ name: action.page });
    showNotifications.value = false;
  }
}

// 通知面板点击外部/路由跳转后关闭（此前只能再次点铃铛，容易残留遮挡）
const notificationWrapperRef = ref<HTMLElement | null>(null);

function onDocClick(e: MouseEvent): void {
  if (!showNotifications.value) return;
  const wrapper = notificationWrapperRef.value;
  if (wrapper && e.target instanceof Node && !wrapper.contains(e.target)) {
    showNotifications.value = false;
  }
}

onMounted(() => document.addEventListener("click", onDocClick));
onBeforeUnmount(() => document.removeEventListener("click", onDocClick));
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
      <div v-if="wsReconnecting" class="ws-reconnect-bar ws-reconnect-inline" :title="reconnectHint">
        <span class="spinner"></span>
        <span class="ws-reconnect-text">{{ reconnectHint }}</span>
        <button class="btn btn-xs" @click="retryNow()" title="不清退避计时，立即重连一次">立即重试</button>
      </div>
      <div ref="notificationWrapperRef" class="notification-wrapper">
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
          <div v-if="!notifications.length" class="empty-state empty-state--sm">暂无通知</div>
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
        :title="status.monitoring ? '停止网络检测和自动登录' : '开始检测网络，断网时自动登录'"
      >
        <span v-if="busy.monitor" class="spinner spinner-on-accent"></span>
        <IconApp v-else class="btn-icon" :name="status.monitoring ? 'pause' : 'play'" />
        {{ busy.monitor ? "处理中..." : (status.monitoring ? "停止检测" : "启动检测") }}
      </button>
    </div>
  </header>
</template>
