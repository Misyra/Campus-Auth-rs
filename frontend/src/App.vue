<script setup lang="ts">
// 根组件：整体布局骨架（sidebar + topbar + router-view + 全局浮层）。
// 协议向导显示时隐藏主界面；初始化期间展示加载遮罩。

import { useUi } from "./composables/useUi";
import AppSidebar from "./components/common/AppSidebar.vue";
import AppTopbar from "./components/common/AppTopbar.vue";
import ToastNotification from "./components/common/ToastNotification.vue";
import ConfirmDialog from "./components/common/ConfirmDialog.vue";
import { defineAsyncComponent, onMounted, onUnmounted, ref, watch } from "vue";
import type { Ref } from "vue";
import { useDebug } from "./composables/useDebug";
import { useRepoImport } from "./composables/useRepoImport";
import { useUpdateDialog } from "./composables/useUpdateDialog";
import { useCommandPalette, installCommandPaletteHotkey } from "./composables/useCommandPalette";
import AuthBootstrapGuide from "./components/AuthBootstrapGuide.vue";
import { hasUsableAuthToken } from "./api/client";

// 五个低频浮层（向导 / 调试面板 / 仓库导入 / 更新弹窗 / 命令面板）异步分包：首屏 JS 只保留
// 常驻布局与高频弹层（toast / 确认框 / 顶栏搜索入口），浮层首次打开时才拉取对应 chunk。
// 浮层不常驻，chunk 加载完成前的短暂等待即可接受，故不设 loading 占位。
const SetupWizard = defineAsyncComponent(() => import("./components/common/SetupWizard.vue"));
const DebugPanel = defineAsyncComponent(() => import("./components/DebugPanel.vue"));
const RepoImportModals = defineAsyncComponent(() => import("./components/RepoImportModals.vue"));
const UpdateDialog = defineAsyncComponent(() => import("./components/UpdateDialog.vue"));
const CommandPalette = defineAsyncComponent(() => import("./components/common/CommandPalette.vue"));

const { state } = useUi();
const debug = useDebug();
const updateDialog = useUpdateDialog();
const repoImport = useRepoImport().repoImport;
const palette = useCommandPalette();

// 模板中嵌套在普通对象里的 ref 不会自动解包，浮层开态先解构出来
const { visible: debugPanelVisible } = debug;

/**
 * 浮层「开过即挂载」标记：defineAsyncComponent 只在组件真正挂载时才拉 chunk，
 * 若直接拿开态做 v-if，关闭瞬间 v-if 会把整个组件卸载，Modal 的离场渐隐动画
 * 与浮层内部状态（向导步进、导入筛选现场等）都会丢失。改为首次打开即转真并
 * 保持——此后组件常驻但 Modal 仍由 open 控制渲染，与改造前（同步常驻挂载）
 * 的 DOM 行为一致；从未打开过的会话则完全不加载对应 chunk。
 */
function onceOpened(source: () => boolean): Ref<boolean> {
  const opened = ref(false);
  watch(source, (open) => {
    if (open) opened.value = true;
  }, { immediate: true });
  return opened;
}
const wizardOpened = onceOpened(() => state.showWizard);
const debugPanelOpened = onceOpened(() => debugPanelVisible.value);
const repoImportOpened = onceOpened(() => repoImport.value.visible);
const updateDialogOpened = onceOpened(() => updateDialog.state.open);
const paletteOpened = onceOpened(() => palette.open.value);

/**
 * 鉴权引导态：首检完成（isLoading 翻转为 false）仍无可用 token 时接管界面。
 * URL fragment 的 bootstrap 握手在首个 API 请求前的 ensureAuthToken 内完成，
 * 到此刻已有结论——此处只区分「换到了 token」与「需要引导页」两种结局。
 */
const bootstrapNeeded = ref(false);
watch(
  () => state.isLoading,
  (loading) => {
    if (!loading) bootstrapNeeded.value = !hasUsableAuthToken();
  },
  { immediate: true },
);

// 启动时恢复服务端仍活跃的调试会话：否则页面刷新后界面"失忆"，
// 用户既看不到会话在跑也没有停止入口，登录会一直撞"Worker 忙"错误
let uninstallHotkey: (() => void) | null = null;
onMounted(() => {
  void debug.restoreIfActive();
  // 注册"后端周期检查命中"的自动弹窗（watch 需组件作用域，故在挂载时建立）
  updateDialog.initAutoOpen();
  // 命令面板全局快捷键 Ctrl+K / Cmd+K（返回卸载函数，卸载时解绑）
  uninstallHotkey = installCommandPaletteHotkey();
});
onUnmounted(() => uninstallHotkey?.());
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

    <!-- 以下浮层均为异步分包组件，由 onceOpened 标记控制首挂载时机；
         挂载后的可见性仍由各组件内部 Modal 的 open 状态决定 -->
    <SetupWizard v-if="wizardOpened" />
    <ToastNotification />
    <ConfirmDialog />
    <DebugPanel v-if="debugPanelOpened" />
    <!-- 仓库导入弹窗全局挂载：任意路由（/tasks 或 /settings/environment）均可触发，避免局部挂载导致切页才弹的错位 -->
    <RepoImportModals v-if="repoImportOpened" />
    <!-- 更新弹窗同样全局挂载：启动/周期检查命中时任意页面都能直接弹出 -->
    <UpdateDialog v-if="updateDialogOpened" />

    <!-- 命令面板：入口统一在顶栏搜索（点击与 Ctrl+K / Cmd+K 同效），
         面板本体懒加载，首开时才拉取 chunk -->
    <CommandPalette v-if="paletteOpened" />

    <!-- 鉴权引导页：无 token 且无可用秘钥/密码时全屏接管（样式见组件 scoped） -->
    <AuthBootstrapGuide v-if="bootstrapNeeded" />

    <div v-if="state.isLoading" class="init-overlay">
      <span class="spinner"></span>
      <span>正在初始化...</span>
    </div>
  </div>
</template>
