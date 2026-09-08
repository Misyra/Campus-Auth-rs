<script setup lang="ts">
/** 设置页骨架：标签导航与保存栏等公共交互，具体表单由 settings-* 子页渲染 */
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

const activeTab = computed(() => {
  const name = route.name as string;
  return name.replace("settings-", "") || "account";
});

function setTab(tabId: string) {
  router.push({ name: `settings-${tabId}` });
}

const saveFailed = computed(() => config.saveFailed.value);
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
    <div class="settings-tabs card">
      <button
        v-for="tab in SETTINGS_TABS" :key="tab.id" type="button"
        class="settings-tab"
        :class="{ active: activeTab === tab.id }"
        :title="tab.hint"
        @click="setTab(tab.id)"
      >
        <span>{{ tab.label }}</span>
      </button>
    </div>

    <div v-if="configLoadFailed" class="settings-load-failed">
      <span>配置加载失败，当前显示的是默认值。为避免覆盖服务器配置，保存已禁用。</span>
      <button type="button" class="btn btn-sm" @click="handleRetryLoad">重试</button>
    </div>
    <form autocomplete="on" class="settings-form">
      <router-view />
    </form>

    <div class="save-bar">
      <button
        class="btn btn-primary save-btn"
        :class="{
          'save-btn-dirty': !busy.save && !saveFailed && config.dirty.value,
          'save-btn-saving': busy.save,
          'save-btn-failed': saveFailed && !busy.save,
        }"
        @click="handleSave"
        :disabled="busy.save || configLoadFailed"
      >
        <IconApp name="refresh" v-if="busy.save" class="spin" />
        <IconApp name="refresh-cw" v-else-if="saveFailed" />
        <IconApp name="save" v-else />
        <span>{{ busy.save ? '保存中' : (saveFailed ? '重试' : '立即保存') }}</span>
      </button>
    </div>
  </div>
</template>
