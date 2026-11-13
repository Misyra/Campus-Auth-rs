<script setup lang="ts">
// 顶栏（替代原 topbar.html）。
// 展示页面标题、未保存提示、WebSocket 重连状态，右侧动作区固定三件套：
// 搜索（命令面板入口）、通知中心、主题切换；监控开关已移入仪表盘首页
// （「启动检测」按钮），顶栏只承担全局导航动作。

import { computed } from "vue";
import { useRoute, useRouter } from "vue-router";
import AppPopover from "./AppPopover.vue";
import IconApp from "./IconApp.vue";
import type { NotificationAction } from "../../api/types";
import { useConfig } from "../../composables/useConfig";
import { useStatus } from "../../composables/useStatus";
import { useNotifications } from "../../composables/useNotifications";
import { useWebSocket } from "../../composables/useWebSocket";
import { useUi } from "../../composables/useUi";
import { useAppearance } from "../../composables/useAppearance";
import { useCommandPalette } from "../../composables/useCommandPalette";
import { useSettingsSearch } from "../../composables/useSettingsSearch";

const route = useRoute();
const router = useRouter();
const { dirty } = useConfig();
const { busy } = useStatus();
const { notifications, unreadNotifications, showNotifications } = useNotifications();
const { wsReconnecting, wsRetryCount, wsDisconnectReason, retryNow } = useWebSocket();
const { quitApp } = useUi();
const { appearance, getEffectiveTheme } = useAppearance();
const { openPalette } = useCommandPalette();
const { query: settingsQuery } = useSettingsSearch();

/** 设置路由下顶栏搜索即本页过滤器（与设置搜索合并为一个入口） */
const isSettingsRoute = computed(() => String(route.name ?? "").startsWith("settings"));

/** 主题切换按钮展示**目标**主题图标：浅色时显示月亮（点它转深色），反之太阳 */
const isDarkTheme = computed(() => getEffectiveTheme() === "dark");
function toggleTheme(): void {
  // 快捷切换写显式明/暗；「跟随系统」仍在外观设置页选择，避免快捷按钮
  // 在 auto↔某一边之间产生第三种猜测语义
  appearance.theme = isDarkTheme.value ? "light" : "dark";
}
/** 快捷键提示：mac 用 ⌘K，其余平台用 Ctrl K（与全局监听 Ctrl+K 同口径） */
const hotkeyLabel = /mac/i.test(navigator.userAgent) ? "⌘K" : "Ctrl K";

/** 重连条文案：首轮为通用重连中，连续失败后按原因给出检查/重启指引 */
const reconnectHint = computed(() => {
  if (wsDisconnectReason.value === "unreachable") {
    return "后端无响应，请检查后端是否已启动；若刚更新/重启过可点重试";
  }
  if (wsDisconnectReason.value === "unauthorized") {
    return "疑似后端重启，正在刷新凭据重连…";
  }
  return `重连中 (第 ${wsRetryCount.value + 1} 次)`;
});

const pageTitle = computed(() => (route.meta.title as string) || "认证喵");
const compactPageTitle = computed(() => pageTitle.value.split(" · ").pop() || pageTitle.value);
const showDirty = computed(() => dirty.value && String(route.name).startsWith("settings"));

function onActionClick(action: NotificationAction | null): void {
  if (action?.page) {
    router.push({ name: action.page });
    showNotifications.value = false;
  }
}

/**
 * 通知面板开合（E6 收口进 AppPopover/Reka Popover 后本组件不再持监听）：
 * 外部点击、Esc、焦点归还全由库承担；这里只补两件事——
 * 打开时刻清零未读（与原 toggleNotifications 的时机一致，仅打开时清）；
 * 关闭时把面板合上（外部点击/路由跳转后的 update:open(false) 也走这里）。
 */
function onNotifOpen(open: boolean): void {
  showNotifications.value = open;
  if (open) unreadNotifications.value = 0;
}
</script>

<template>
  <header class="top-bar">
    <div class="top-title-area">
      <h1 class="page-title"><span class="page-title-full">{{ pageTitle }}</span><span class="page-title-compact">{{ compactPageTitle }}</span></h1>
      <span v-if="showDirty" class="top-dirty-hint">
        <span class="top-dirty-dot"></span>未保存
      </span>
    </div>
    <div v-if="wsReconnecting" class="ws-reconnect-bar ws-reconnect-inline" :title="reconnectHint">
      <span class="spinner"></span>
      <span class="ws-reconnect-text">{{ reconnectHint }}</span>
      <button class="btn btn-xs" @click="retryNow()" title="跳过退避等待，立即以最短间隔重连">立即重试</button>
    </div>
    <div class="top-actions">
      <!-- 搜索：设置路由下即本页过滤器（与页面搜索合并为一个入口）；
           其余路由打开命令面板（点击与 Ctrl/⌘+K 同效） -->
      <div v-if="isSettingsRoute" class="topbar-search topbar-search--input">
        <IconApp name="search" />
        <input
          v-model="settingsQuery"
          type="text"
          class="topbar-search-input"
          placeholder="搜索设置项、说明或分区标题"
          aria-label="搜索设置"
          autocomplete="off"
        />
        <button
          v-if="settingsQuery"
          type="button"
          class="topbar-search-clear"
          aria-label="清空搜索"
          title="清空搜索"
          @click="settingsQuery = ''"
        >
          <IconApp name="close" />
        </button>
      </div>
      <button v-else type="button" class="topbar-search" title="搜索或执行命令" aria-label="搜索或执行命令" @click="openPalette()">
        <IconApp name="search" />
        <span class="topbar-search-text">搜索</span>
        <kbd class="topbar-search-kbd">{{ hotkeyLabel }}</kbd>
      </button>
      <div class="notification-wrapper">
        <AppPopover :open="showNotifications" class="notification-dropdown" aria-label="通知历史" @update:open="onNotifOpen">
          <template #trigger>
            <button class="btn btn-icon-only" title="通知历史" aria-label="通知历史" @pointerdown.stop>
              <IconApp name="bell" />
              <span v-if="unreadNotifications > 0" class="notification-badge">{{ unreadNotifications > 9 ? "9+" : unreadNotifications }}</span>
            </button>
          </template>
          <!-- 通知历史是信息面板而非命令菜单（子项非 menuitem），dialog 语义才与实际交互一致；
               aria-haspopup=dialog / aria-expanded / role=dialog 由 Reka 输出。 -->
          <div class="notification-header">
            <span>通知历史</span>
            <button class="btn btn-text btn-xs" @click="notifications.splice(0, notifications.length)">清空</button>
          </div>
          <div v-if="!notifications.length" class="empty-state empty-state--sm">暂无通知</div>
          <div
            v-for="n in notifications"
            :key="n.id"
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
        </AppPopover>
      </div>
      <button
        type="button"
        class="btn btn-icon-only"
        :title="isDarkTheme ? '切换到浅色主题' : '切换到深色主题'"
        :aria-label="isDarkTheme ? '切换到浅色主题' : '切换到深色主题'"
        @click="toggleTheme"
      >
        <IconApp :name="isDarkTheme ? 'sun' : 'moon'" />
      </button>
      <button type="button" class="btn btn-icon-only mobile-quit" @click="quitApp" :disabled="busy.monitor" title="退出应用" aria-label="退出应用"><IconApp name="log-out" /></button>
    </div>
  </header>
</template>
