/**
 * 路由定义（Vue Router 4，history 模式）。
 * 替代原 template-loader.js 的页面切换。支持懒加载与未保存配置离开确认。
 */

import { createRouter, createWebHistory } from "vue-router";
import { IS_OPENWRT } from "../utils/platform";
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
    path: "/profiles",
    name: "profiles",
    meta: { title: "配置方案" },
    component: () => import("@/views/ProfilesView.vue"),
  },
  {
    path: "/tasks",
    name: "tasks",
    meta: { title: "任务" },
    component: () => import("@/views/TasksView.vue"),
    redirect: { name: IS_OPENWRT ? "tasks-http" : "tasks-browser" },
    children: [
      { path: "", name: "tasks-browser", meta: { title: "任务 · 浏览器任务" }, component: () => import("@/views/tasks/BrowserTasksPanel.vue") },
      // HTTP 登录任务：字段编辑在任务页，方案只引用一个任务 id（active_http_task），
      // 故与浏览器任务并列为一个 Tab；默认落地仍是 tasks-browser
      { path: "http", name: "tasks-http", meta: { title: "任务 · HTTP 登录" }, component: () => import("../views/tasks/HttpTasksPanel.vue") },
      { path: "scripts", name: "tasks-scripts", meta: { title: "任务 · 脚本" }, component: () => import("@/views/tasks/ScriptsPanel.vue") },
      // 定时任务原为侧栏独立页，并入任务页：同属「可被触发执行的东西」，
      // 放在一处免去「任务在哪、计划又在哪」的往返
      { path: "scheduled", name: "tasks-scheduled", meta: { title: "任务 · 定时任务" }, component: () => import("@/views/tasks/ScheduledTasksPanel.vue") },
      { path: "ai", name: "tasks-ai", meta: { title: "任务 · AI 生成浏览器任务" }, component: () => import("@/views/AiTaskView.vue") },
    ],
  },
  {
    path: "/settings",
    name: "settings",
    meta: { title: "设置" },
    component: () => import("@/views/SettingsView.vue"),
    // 阶段 4③：设置页改单路由 + 分区锚点（/settings#<id>），分区内导航由页内
    // sticky tab 条承担。旧分类子路由全部保留**路由名**并重定向到对应锚点——
    // 书签深链（/settings/browser）与程序化 push({ name: "settings-tasks" })
    // （DashboardView 环境横幅、AboutView 修复入口、useUi 浏览器引导）都经
    // 这批重定向继续可达；重定向在守卫前解析，beforEach 看到的目标已是 settings。
    children: [
      // 账号/认证地址/登录方式已全部移交「配置方案」页（单一入口）
      { path: "account", redirect: { name: "profiles" } },
      { path: "monitor", name: "settings-monitor", redirect: { path: "/settings", hash: "#network" } },
      { path: "browser", name: "settings-browser", redirect: { path: "/settings", hash: "#browser" } },
      { path: "tasks", name: "settings-tasks", redirect: { path: "/settings", hash: "#tasks" } },
      // 旧环境深链继续定位到任务与环境。
      { path: "environment", redirect: { path: "/settings", hash: "#tasks" } },
      { path: "system", name: "settings-system", redirect: { path: "/settings", hash: "#system" } },
      // 旧「网络与更新」Tab 并入「系统与更新」→ 现归「系统」分区
      { path: "network", redirect: { path: "/settings", hash: "#system" } },
      { path: "notification", name: "settings-notification", redirect: { path: "/settings", hash: "#notification" } },
      { path: "appearance", name: "settings-appearance", redirect: { path: "/settings", hash: "#appearance" } },
    ],
  },
  // 旧「AI 生成任务」独立页已并入「任务」页 Tab：保留深链与书签重定向
  { path: "/ai-task", redirect: { name: "tasks-ai" } },
  { path: "/scheduled", redirect: { name: "tasks-scheduled" } },
  // 旧「自定义脚本」页已并入「任务」页的脚本 Tab：保留深链与书签重定向
  { path: "/scripts", redirect: { name: "tasks-scripts" } },
  { path: "/appearance", redirect: { name: "settings-appearance" } },
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
  if (IS_OPENWRT && ["tasks-browser", "tasks-ai"].includes(String(to.name))) {
    return { name: "tasks-http" };
  }
  // OpenWrt 的浏览器/任务分区裁剪已改为 schema 装配期过滤（views/settings/schema.ts），
  // 旧 settings-* 子路由名经重定向解析后不再出现为导航目标，此处无需再拦
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
