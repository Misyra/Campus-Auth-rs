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
import RepoImportModals from "./components/RepoImportModals.vue";
import UpdateDialog from "./components/UpdateDialog.vue";
import { onMounted } from "vue";
import { useDebug } from "./composables/useDebug";
import { useUpdateDialog } from "./composables/useUpdateDialog";

const { state } = useUi();
const debug = useDebug();
const updateDialog = useUpdateDialog();

// 启动时恢复服务端仍活跃的调试会话：否则页面刷新后界面"失忆"，
// 用户既看不到会话在跑也没有停止入口，登录会一直撞"Worker 忙"错误
onMounted(() => {
  void debug.restoreIfActive();
  // 注册"后端周期检查命中"的自动弹窗（watch 需组件作用域，故在挂载时建立）
  updateDialog.initAutoOpen();
});
</script>

<template>
  <div id="app-root">
    <!-- 跳到主内容：键盘用户不必逐个 Tab 穿过整条侧栏（样式起见于 base.css 的 .skip-link，
        此前那条规则一直是死代码——没有任何实例） -->
    <a class="skip-link" href="#main">跳到主内容</a>
    <template v-if="!state.showWizard">
      <AppSidebar />
      <div class="main-content">
        <AppTopbar />
        <main id="main" tabindex="-1" class="content-wrapper">
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
    <!-- 仓库导入弹窗全局挂载：任意路由（/tasks 或 /settings/environment）均可触发，避免局部挂载导致切页才弹的错位 -->
    <RepoImportModals />
    <!-- 更新弹窗同样全局挂载：启动/周期检查命中时任意页面都能直接弹出 -->
    <UpdateDialog />

    <div v-if="state.isLoading" class="init-overlay">
      <span class="spinner"></span>
      <span>正在初始化...</span>
    </div>
  </div>
</template>
