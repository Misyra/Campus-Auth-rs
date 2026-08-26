<script setup lang="ts">
import IconApp from "@/components/common/IconApp.vue";
import { computed } from "vue";
import { useRoute, useRouter } from "vue-router";
import { useConfig } from "@/composables/useConfig";
import { useStatus } from "@/composables/useStatus";
import { useToast } from "@/composables/useToast";
import { SETTINGS_TABS } from "@/utils/constants";

const route = useRoute();
const router = useRouter();
const config = useConfig();
const { busy } = useStatus();
const { toastOnly } = useToast();

// Tab 清单统一由 constants 维护（消除视图内重复定义的三重维护）
const TABS = SETTINGS_TABS;

const activeTab = computed(() => {
  const name = route.name as string;
  return name.replace("settings-", "") || "account";
});

function setTab(tabId: string) {
  router.push({ name: `settings-${tabId}` });
}

// 保存状态
const saveFailed = computed(() => config.saveFailed.value);
// F2：配置加载失败时禁用保存（避免基于降级默认值的修改覆盖服务端配置），并显示重试提示
const configLoadFailed = computed(() => config.configLoadFailed.value);

function handleRetryLoad() {
  void config.fetchConfig();
}

function handleSave() {
  if (!config.dirty.value) {
    toastOnly(true, "配置没有变更，无需保存");
    return;
  }
  void config.saveConfig();
}
</script>

<template>
  <div class="page-content settings-page">
    <!-- Tab 导航 -->
    <div class="settings-tabs card">
      <button
        v-for="tab in TABS" :key="tab.id" type="button"
        class="settings-tab"
        :class="{ active: activeTab === tab.id }"
        :title="tab.hint"
        @click="setTab(tab.id)"
      >
        <span>{{ tab.label }}</span>
      </button>
    </div>

    <!-- 子路由出口 -->
    <!-- F2：配置加载失败顶部提示，避免误以为配置已就绪而保存降级默认值 -->
    <div v-if="configLoadFailed" class="settings-load-failed">
      <span>配置加载失败，当前显示的是默认值。为避免覆盖服务器配置，保存已禁用。</span>
      <button type="button" class="btn btn-sm" @click="handleRetryLoad">重试</button>
    </div>
    <form v-if="activeTab !== 'tasks'" autocomplete="on" class="settings-form">
      <router-view />
    </form>
    <router-view v-else />

    <!-- 保存栏 -->
    <div class="save-bar">
      <button
        class="btn save-btn"
        :class="{
          'save-btn-dirty': !busy.save && !saveFailed && config.dirty.value,
          'save-btn-saving': busy.save,
          'save-btn-failed': saveFailed && !busy.save,
        }"
        @click="handleSave"
        :disabled="busy.save || configLoadFailed"
      >
        <IconApp name="refresh" v-if="busy.save" class="spin" />
        <svg v-else-if="saveFailed" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
          <polyline points="23 4 23 10 17 10"/><path d="M20.49 15a9 9 0 11-2.12-9.36L23 10"/>
        </svg>
        <svg v-else viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
          <path d="M19 21H5a2 2 0 01-2-2V5a2 2 0 012-2h11l5 5v11a2 2 0 01-2 2z"/><polyline points="17 21 17 13 7 13 7 21"/><polyline points="7 3 7 8 15 8"/>
        </svg>
        <span>{{ busy.save ? '保存中' : (saveFailed ? '重试' : '立即保存') }}</span>
      </button>
    </div>
  </div>
</template>
