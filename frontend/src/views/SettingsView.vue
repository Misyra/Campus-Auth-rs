<script setup lang="ts">
import { computed } from "vue";
import { useRoute, useRouter } from "vue-router";
import { useConfig } from "@/composables/useConfig";
import { useStatus } from "@/composables/useStatus";
import { useToast } from "@/composables/useToast";

const route = useRoute();
const router = useRouter();
const config = useConfig();
const { busy } = useStatus();
const { toastOnly } = useToast();

const TABS = [
  { id: "account", label: "账号" },
  { id: "monitor", label: "监测" },
  { id: "system", label: "系统" },
  { id: "browser", label: "浏览器" },
  { id: "tasks", label: "任务" },
];

const activeTab = computed(() => {
  const name = route.name as string;
  return name.replace("settings-", "") || "account";
});

function setTab(tabId: string) {
  router.push({ name: `settings-${tabId}` });
}

// 保存状态
const saveFailed = computed(() => config.saveFailed.value);

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
        @click="setTab(tab.id)"
      >
        <span>{{ tab.label }}</span>
      </button>
    </div>

    <!-- 子路由出口 -->
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
        :disabled="busy.save"
      >
        <svg v-if="busy.save" class="spin" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
          <path d="M21 12a9 9 0 11-6.219-8.56"/>
        </svg>
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
