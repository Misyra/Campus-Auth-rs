<script setup lang="ts">
// 根组件：整体布局骨架（sidebar + topbar + router-view + 全局浮层）。
// 协议向导显示时隐藏主界面；初始化期间展示加载遮罩。

import { useUi } from "./composables/useUi";
import AppSidebar from "./components/common/AppSidebar.vue";
import AppTopbar from "./components/common/AppTopbar.vue";
import ToastNotification from "./components/common/ToastNotification.vue";
import ConfirmDialog from "./components/common/ConfirmDialog.vue";
import SetupWizard from "./components/common/SetupWizard.vue";
import DebugPanel from "./components/DebugPanel.vue";

const { state } = useUi();
</script>

<template>
  <div id="app-root">
    <template v-if="!state.showWizard">
      <AppSidebar />
      <div class="main-content">
        <AppTopbar />
        <main class="content-wrapper">
          <router-view v-slot="{ Component }">
            <component :is="Component" />
          </router-view>
        </main>
      </div>
    </template>

    <SetupWizard />
    <ToastNotification />
    <ConfirmDialog />
    <DebugPanel />

    <div v-if="state.isLoading" class="init-overlay">
      <span class="spinner"></span>
      <span>正在初始化...</span>
    </div>
  </div>
</template>
