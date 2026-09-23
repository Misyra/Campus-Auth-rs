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
    redirect: { name: "tasks-browser" },
    children: [
      { path: "", name: "tasks-browser", meta: { title: "任务 · 浏览器任务" }, component: () => import("@/views/tasks/BrowserTasksPanel.vue") },
      // 直连任务：字段编辑在任务页，方案只引用一个任务 id（active_http_task），
      // 故与浏览器任务并列为一个 Tab；默认落地仍是 tasks-browser
      { path: "http", name: "tasks-http", meta: { title: "任务 · 直连任务" }, component: () => import("../views/tasks/HttpTasksPanel.vue") },
      { path: "scripts", name: "tasks-scripts", meta: { title: "任务 · 脚本" }, component: () => import("@/views/tasks/ScriptsPanel.vue") },
      // 定时任务原为侧栏独立页，并入任务页：同属「可被触发执行的东西」，
      // 放在一处免去「任务在哪、计划又在哪」的往返
      { path: "scheduled", name: "tasks-scheduled", meta: { title: "任务 · 定时任务" }, component: () => import("@/views/ScheduledTasksView.vue") },
      { path: "ai", name: "tasks-ai", meta: { title: "任务 · AI 生成浏览器任务" }, component: () => import("@/views/AiTaskView.vue") },
    ],
  },
  {
    path: "/settings",
    name: "settings",
    meta: { title: "设置" },
    component: () => import("@/views/SettingsView.vue"),
    redirect: { name: "settings-monitor" },
    children: [
      { path: "", redirect: { name: "settings-monitor" } },
      // 账号/认证地址/登录方式已全部移交「配置方案」页（单一入口）：保留
      // 旧深链与书签的重定向，避免用户手上的 /settings/account 变成 404
      { path: "account", redirect: { name: "profiles" } },
      { path: "monitor", name: "settings-monitor", meta: { title: "设置 · 网络检测" }, component: () => import("@/views/settings/MonitorSettings.vue") },
      { path: "browser", name: "settings-browser", meta: { title: "设置 · 浏览器" }, component: () => import("@/views/settings/BrowserSettings.vue") },
      { path: "tasks", name: "settings-tasks", meta: { title: "设置 · 任务与环境" }, component: () => import("@/views/settings/TaskEnvironmentSettings.vue") },
      // 旧「环境」Tab 深链与书签重定向到合并后的 Tab
      { path: "environment", redirect: { name: "settings-tasks" } },
      { path: "system", name: "settings-system", meta: { title: "设置 · 系统" }, component: () => import("@/views/settings/SystemSettings.vue") },
      { path: "network", name: "settings-network", meta: { title: "设置 · 网络与更新" }, component: () => import("@/views/settings/NetworkSettings.vue") },
      // 外观原为侧栏独立页，并入设置页：它是纯本机显示偏好，与其余设置同类
      { path: "appearance", name: "settings-appearance", meta: { title: "设置 · 外观" }, component: () => import("@/views/AppearanceView.vue") },
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

// FE2-9：离开任务/脚本编辑页且存在未保存草稿时，确认是否放弃
// （动态导入：editorGuard 依赖的 composable 链可能回指 router，避免求值期循环依赖）
router.beforeEach(async (to, from) => {
  const { guardEditorLeave } = await import("./editorGuard");
  return guardEditorLeave(to, from);
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
