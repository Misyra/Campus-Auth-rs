/**
 * 设置页 schema 槽位组件注册表。
 *
 * schema.ts 只声明槽位**键**（纯数据，可被搜索测试无副作用地消费）；本文件把键
 * 映射到实际组件——settingsSearch 的纯函数测试由此避免整棵组件图入栈。
 * 新增槽位：先在 schema.ts 声明键与搜索文本，再在此注册组件（schema.test.ts
 * 会断言两处键集合一致）。
 */
import type { Component } from "vue";
import AppearanceView from "@/views/AppearanceView.vue";
import MonitorProbeRows from "./panels/MonitorProbeRows.vue";
import MonitorPauseRows from "./panels/MonitorPauseRows.vue";
import ProxyNote from "./panels/ProxyNote.vue";
import BrowserSelectRows from "./panels/BrowserSelectRows.vue";
import BrowserStealthRows from "./panels/BrowserStealthRows.vue";
import BrowserSessionRows from "./panels/BrowserSessionRows.vue";
import BrowserAdvancedRows from "./panels/BrowserAdvancedRows.vue";
import EnvRuntimePanel from "./panels/EnvRuntimePanel.vue";
import TaskOverviewRows from "./panels/TaskOverviewRows.vue";
import TaskRecorderRows from "./panels/TaskRecorderRows.vue";
import OcrRows from "./panels/OcrRows.vue";
import RunModePanel from "./panels/RunModePanel.vue";
import SystemStartupRows from "./panels/SystemStartupRows.vue";
import SystemAccessRows from "./panels/SystemAccessRows.vue";
import SystemLogsExtraRows from "./panels/SystemLogsExtraRows.vue";
import UpdateTipBlock from "./panels/UpdateTipBlock.vue";
import UpdateProxyRows from "./panels/UpdateProxyRows.vue";
import UpdateActionsRows from "./panels/UpdateActionsRows.vue";
import SystemMaintenanceRows from "./panels/SystemMaintenanceRows.vue";
import NotificationEventsRows from "./panels/NotificationEventsRows.vue";
import NotificationQuietRows from "./panels/NotificationQuietRows.vue";
import NotificationChannelsPanel from "./panels/NotificationChannelsPanel.vue";

/** slot 键 → 槽位组件（面板级 `component` 与行级 `slot` 共用同一张表） */
export const SETTINGS_PANEL_SLOTS: Record<string, Component> = {
  // 网络 / 登录
  "monitor-probe": MonitorProbeRows,
  "monitor-pause": MonitorPauseRows,
  "proxy-note": ProxyNote,
  // 浏览器
  "browser-select": BrowserSelectRows,
  "browser-stealth": BrowserStealthRows,
  "browser-session": BrowserSessionRows,
  "browser-advanced": BrowserAdvancedRows,
  // 任务与环境（env-runtime 为 chromeless 整面板）
  "env-runtime": EnvRuntimePanel,
  "task-overview": TaskOverviewRows,
  "task-recorder": TaskRecorderRows,
  ocr: OcrRows,
  // 系统 / 更新与日志
  "run-mode": RunModePanel,
  "system-startup": SystemStartupRows,
  "system-access": SystemAccessRows,
  "system-logs-extra": SystemLogsExtraRows,
  "update-tip": UpdateTipBlock,
  "update-proxy": UpdateProxyRows,
  "update-actions": UpdateActionsRows,
  "system-maintenance": SystemMaintenanceRows,
  // 推送通知（notification-channels 为 chromeless 整面板）
  "notification-events": NotificationEventsRows,
  "notification-quiet": NotificationQuietRows,
  "notification-channels": NotificationChannelsPanel,
  // 外观（整段平移，AppearanceView 原样挂载）
  appearance: AppearanceView,
};
