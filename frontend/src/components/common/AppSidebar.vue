<script setup lang="ts">
import IconApp from "@/components/common/IconApp.vue";
// 侧边导航栏（替代原 sidebar.html）。
// 路由驱动高亮；"更多"菜单在位于子页面时自动展开。

import { ref, computed } from "vue";
import { useRoute, useRouter } from "vue-router";
import { useStatus } from "../../composables/useStatus";
import { useUi } from "../../composables/useUi";

const route = useRoute();
const router = useRouter();
const { status, busy } = useStatus();
const { quitApp } = useUi();

const MORE_PAGES = ["profiles", "scripts", "scheduled", "appearance"];
const moreActive = computed(() => MORE_PAGES.includes(String(route.name)));
const showMoreNav = ref(false);
const expanded = computed(() => showMoreNav.value || moreActive.value);

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

      <button
        class="nav-item"
        :class="{ active: String(route.name).startsWith('settings') }"
        @click="navigate('settings')"
        title="设置"
      >
        <IconApp name="settings" class="nav-icon" />
        <span>设置</span>
      </button>

      <button class="nav-item" :class="{ active: route.name === 'tasks' }" @click="navigate('tasks')" title="任务管理">
        <IconApp name="file-text" class="nav-icon" />
        <span>任务管理</span>
      </button>

      <button class="nav-item" :class="{ active: route.name === 'about' }" @click="navigate('about')" title="关于">
        <IconApp name="info" class="nav-icon" />
        <span>关于</span>
      </button>

      <div class="nav-more">
        <button
          class="nav-item nav-more-trigger"
          :class="{ active: moreActive }"
          @click="showMoreNav = !showMoreNav"
          title="更多"
        >
          <IconApp name="more-vertical" class="nav-icon" />
          <span>更多</span>
          <IconApp name="chevron-down" class="nav-more-arrow" :class="{ expanded }" aria-hidden="true" />
        </button>

        <transition name="nav-more">
          <div class="nav-more-menu" v-show="expanded">
            <button class="nav-item" :class="{ active: route.name === 'profiles' }" @click="navigate('profiles')" title="配置方案">
              <IconApp name="wifi" class="nav-icon" />
              <span>配置方案</span>
            </button>

            <button class="nav-item" :class="{ active: route.name === 'scripts' }" @click="navigate('scripts')" title="自定义脚本">
              <IconApp name="code" class="nav-icon" />
              <span>自定义脚本</span>
            </button>

            <button class="nav-item" :class="{ active: route.name === 'scheduled' }" @click="navigate('scheduled')" title="定时任务">
              <IconApp name="clock" class="nav-icon" />
              <span>定时任务</span>
            </button>

            <button class="nav-item" :class="{ active: route.name === 'appearance' }" @click="navigate('appearance')" title="外观">
              <IconApp name="palette" class="nav-icon" />
              <span>外观</span>
            </button>
          </div>
        </transition>
      </div>
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
