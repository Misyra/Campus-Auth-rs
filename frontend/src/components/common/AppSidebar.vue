<script setup lang="ts">
import IconApp from "@/components/common/IconApp.vue";
// 侧边导航栏（替代原 sidebar.html）。
// 路由驱动高亮。导航按「配置对象」组织而非功能清单——每项对应唯一的数据归属：
//   仪表盘 = 状态总览
//   方案   = ProfileData（账号/认证/匹配规则/登录方式，保存走 /api/profiles/{id}）
//   任务   = tasks/（浏览器任务/脚本/定时/AI 生成浏览器任务，保存走 /api/tasks|/api/scripts）
//   设置   = GlobalConfig（检测/浏览器/环境/系统/网络/外观，全局保存栏）
//   关于
// 原「更多」次级菜单已取消：定时任务与外观各自并入「任务」「设置」页，
// 减少一层折叠后不再有需要隐藏的项。

import { computed } from "vue";
import { useRoute, useRouter } from "vue-router";
import { useStatus } from "@/composables/useStatus";
import { useUi } from "@/composables/useUi";

const route = useRoute();
const router = useRouter();
const { status, busy } = useStatus();
const { quitApp } = useUi();

/** 任务页各子路由的名字都以 tasks 开头；设置页同理用 settings 前缀判定高亮 */
const onTasks = computed(() => String(route.name).startsWith("tasks"));
const onSettings = computed(() => String(route.name).startsWith("settings"));

function navigate(name: string): void {
  router.push({ name });
}
</script>

<template>
  <nav class="sidebar">
    <div class="sidebar-header">
      <div class="logo">
        <span class="logo-icon logo-mark" role="img" aria-label="Campus-Auth 校园网认证助手"></span>
        <span class="logo-text">校园网认证</span>
      </div>
    </div>

    <div class="nav-links">
      <button class="nav-item" :class="{ active: route.name === 'dashboard' }" @click="navigate('dashboard')" title="仪表盘">
        <IconApp name="grid" class="nav-icon" />
        <span>仪表盘</span>
      </button>

      <button class="nav-item" :class="{ active: route.name === 'profiles' }" @click="navigate('profiles')" title="配置方案（账号、认证地址、匹配规则、登录方式）">
        <IconApp name="wifi" class="nav-icon" />
        <span>方案</span>
      </button>

      <button class="nav-item" :class="{ active: onTasks }" @click="navigate('tasks')" title="任务（浏览器任务 / 脚本 / 定时任务 / AI 生成浏览器任务）">
        <IconApp name="file-text" class="nav-icon" />
        <span>任务</span>
      </button>

      <button class="nav-item" :class="{ active: onSettings }" @click="navigate('settings')" title="设置（检测 / 浏览器 / 任务与环境 / 系统 / 网络与更新 / 外观）">
        <IconApp name="settings" class="nav-icon" />
        <span>设置</span>
      </button>

      <button class="nav-item" :class="{ active: route.name === 'about' }" @click="navigate('about')" title="关于">
        <IconApp name="info" class="nav-icon" />
        <span>关于</span>
      </button>
    </div>

    <div class="sidebar-footer">
      <div class="status-badge" :class="status.monitoring ? 'online' : 'offline'">
        <span class="status-dot"></span>
        {{ status.monitoring ? "运行中" : "已停止" }}
      </div>
      <button class="btn quit-btn" @click="quitApp" :disabled="busy.monitor" title="退出应用">
        <IconApp name="log-out" />
        退出
      </button>
    </div>
  </nav>
</template>
