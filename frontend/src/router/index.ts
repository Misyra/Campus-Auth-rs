/**
 * 路由定义（Vue Router 4，history 模式）。
 * 替代原 template-loader.js 的页面切换。支持懒加载与未保存配置离开确认。
 */

import { createRouter, createWebHistory } from "vue-router";
import { useConfig } from "../composables/useConfig";
import { useConfirm } from "../composables/useConfirm";

// 路由元信息类型增强：title 用于顶栏展示
declare module "vue-router" {
  interface RouteMeta {
    title?: string;
  }
}

const routes = [
  { path: "/", name: "dashboard", meta: { title: "仪表盘" }, component: () => import("@/views/DashboardView.vue") },
  {
    path: "/settings",
    name: "settings",
    meta: { title: "设置" },
    component: () => import("@/views/SettingsView.vue"),
    redirect: { name: "settings-account" },
    children: [
      { path: "", redirect: { name: "settings-account" } },
      { path: "account", name: "settings-account", meta: { title: "设置 · 账号" }, component: () => import("@/views/settings/AccountSettings.vue") },
      { path: "monitor", name: "settings-monitor", meta: { title: "设置 · 网络监测" }, component: () => import("@/views/settings/MonitorSettings.vue") },
      { path: "system", name: "settings-system", meta: { title: "设置 · 系统" }, component: () => import("@/views/settings/SystemSettings.vue") },
      { path: "browser", name: "settings-browser", meta: { title: "设置 · 浏览器" }, component: () => import("@/views/settings/BrowserSettings.vue") },
      { path: "tasks", name: "settings-tasks", meta: { title: "设置 · 任务" }, component: () => import("@/views/settings/TasksSettings.vue") },
    ],
  },
  { path: "/profiles", name: "profiles", meta: { title: "配置方案" }, component: () => import("@/views/ProfilesView.vue") },
  { path: "/tasks", name: "tasks", meta: { title: "任务管理" }, component: () => import("@/views/TasksView.vue") },
  { path: "/ai-task", name: "ai-task", meta: { title: "AI 生成任务" }, component: () => import("@/views/AiTaskView.vue") },
  { path: "/scheduled", name: "scheduled", meta: { title: "定时任务" }, component: () => import("@/views/ScheduledTasksView.vue") },
  { path: "/scripts", name: "scripts", meta: { title: "自定义脚本" }, component: () => import("@/views/ScriptsView.vue") },
  { path: "/appearance", name: "appearance", meta: { title: "外观" }, component: () => import("@/views/AppearanceView.vue") },
  { path: "/about", name: "about", meta: { title: "关于" }, component: () => import("@/views/AboutView.vue") },
  // 兜底 404 路由：匹配所有未定义路径
  { path: "/:pathMatch(.*)*", name: "not-found", meta: { title: "页面未找到" }, component: () => import("@/views/NotFoundView.vue") },
];

export const router = createRouter({
  history: createWebHistory(),
  routes,
});

// 离开设置页且存在未保存修改时，确认是否放弃
router.beforeEach(async (to, from) => {
  const { dirty, fetchConfig, saveFailed } = useConfig();
  // 仅当真正离开设置区域（含子路由）才拦截；设置页内部切换不打扰用户
  if (dirty.value && from.path.startsWith("/settings") && !to.path.startsWith("/settings")) {
    const { confirm } = useConfirm();
    const ok = await confirm({
      title: "未保存的修改",
      message: "当前设置有未保存的修改，确定要离开吗？离开后未保存的修改将丢失。",
    });
    // 仅 true 才放行离开；取消/被抢占（null）都阻止导航，保留 dirty 现状（A10）
    if (!ok) return false;
    await fetchConfig();
    saveFailed.value = false;
  }
  return true;
});
