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
  return name.replace("settings-", "") || "monitor";
});

function setTab(tabId: string) {
  router.push({ name: `settings-${tabId}` });
}

/**
 * 外观页不走本页的保存栏。
 *
 * 外观是纯本机显示偏好，改动即时写入 localStorage 并立即生效（useAppearance 的
 * watcher），没有服务端草稿。若照常显示「立即保存」，用户改完主题点它会得到
 * 「配置没有变更，无需保存」，与眼前已生效的改动相矛盾。
 */
const isAppearanceTab = computed(() => activeTab.value === "appearance");

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
    <!--
      `<form autocomplete="on">` 仅为拿到浏览器自动填充（账号/密码类字段），**没有**标签页
      提交语义。必须拦掉 submit：该 form 内任何缺 `type="button"` 的 `<button>` 按 HTML
      规范默认 `type="submit"`，点击即触发表单提交 → 导航到当前 URL → 整个 SPA 重载
      （实测：token 查询串被 GET 表单覆盖掉，随即丢失鉴权上下文）。

      子页按钮仍应显式写 `type="button"`（本文件守卫只是纵深防御）；本处拦截保证
      即使漏写也只是不提交，而不会刷新页面。preventDefault 放在最前，避免任何
      子页逻辑先跑再被导航打断。
    -->
    <form autocomplete="on" class="settings-form" @submit.prevent>
      <router-view />
    </form>

    <!-- 外观页改的是本机显示偏好（即时生效、存 localStorage），无服务端草稿可提交 -->
    <div v-if="!isAppearanceTab" class="save-bar">
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
