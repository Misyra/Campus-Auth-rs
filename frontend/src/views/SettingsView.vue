<script setup lang="ts">
/** 设置长页：分类目录定位连续表单，统一保留未保存内容与保存操作。 */
import IconApp from "@/components/common/IconApp.vue";
import MonitorSettings from "@/views/settings/MonitorSettings.vue";
import BrowserSettings from "@/views/settings/BrowserSettings.vue";
import TaskEnvironmentSettings from "@/views/settings/TaskEnvironmentSettings.vue";
import SystemSettings from "@/views/settings/SystemSettings.vue";
import AppearanceView from "@/views/AppearanceView.vue";
import { computed, nextTick, onBeforeUnmount, onMounted, ref, watch } from "vue";
import { useRoute, useRouter } from "vue-router";
import { useConfig } from "@/composables/useConfig";
import { useStatus } from "@/composables/useStatus";
import { useToast } from "@/composables/useToast";
import { useSettingsNavigation } from "@/composables/useSettingsNavigation";
import { activeChildId, SETTINGS_NAV_CHILDREN } from "@/utils/navTree";

const route = useRoute();
const router = useRouter();
const config = useConfig();
const { busy } = useStatus();
const { toastOnly } = useToast();

const navigation = useSettingsNavigation();
const scrollContainer = ref<HTMLElement | null>(null);
const settingsForm = ref<HTMLElement | null>(null);
const sectionComponents = [MonitorSettings, BrowserSettings, TaskEnvironmentSettings, SystemSettings, AppearanceView];
const sections = SETTINGS_NAV_CHILDREN.map((child, index) => ({ ...child, component: sectionComponents[index] }));
let resizeObserver: ResizeObserver | null = null;
let anchoredSection: string | null = null;

/** 旧分类深链仍可定位；所有区域保持挂载，目录跳转不会重建输入框。 */
async function scrollToSection(id: string): Promise<void> {
  await nextTick();
  const container = scrollContainer.value;
  const section = settingsForm.value?.querySelector<HTMLElement>(`#settings-section-${id}`);
  if (!container || !section) return;
  anchoredSection = id;
  container.scrollTop += section.getBoundingClientRect().top - container.getBoundingClientRect().top - 16;
  navigation.setActiveSection(id);
}

function selectSection(id: string): void {
  const child = sections.find((section) => section.id === id);
  if (!child) return;
  if (route.name === child.name) void scrollToSection(id);
  else void router.push({ name: child.name });
}

/** 以视口顶部正在阅读的区域高亮目录，滚动到底时选中最后一类。 */
function updateActiveSection(): void {
  const container = scrollContainer.value;
  if (!container) return;
  const top = container.getBoundingClientRect().top + 32;
  let active = sections[0].id;
  for (const section of sections) {
    const element = settingsForm.value?.querySelector<HTMLElement>(`#settings-section-${section.id}`);
    if (element && element.getBoundingClientRect().top <= top) active = section.id;
  }
  if (container.scrollTop + container.clientHeight >= container.scrollHeight - 2) active = sections[sections.length - 1].id;
  navigation.setActiveSection(active);
}

/** 用户开始阅读或编辑后，让浏览器自然保持滚动，不再校准目录定位。 */
function releaseAnchor(): void { anchoredSection = null; }

watch(() => route.name, (name) => {
  void scrollToSection(activeChildId(SETTINGS_NAV_CHILDREN, String(name)) ?? "monitor");
});
watch(navigation.scrollRequest, (request) => { if (request) void scrollToSection(request.id); });
onMounted(() => {
  void scrollToSection(activeChildId(SETTINGS_NAV_CHILDREN, String(route.name)) ?? "monitor");
  // 环境状态和默认配置异步加载会改变前面区域的高度，首次定位随之校准。
  resizeObserver = new ResizeObserver(() => {
    if (anchoredSection) void scrollToSection(anchoredSection);
    else updateActiveSection();
  });
  if (settingsForm.value) resizeObserver.observe(settingsForm.value);
  if (scrollContainer.value) resizeObserver.observe(scrollContainer.value);
});
onBeforeUnmount(() => { resizeObserver?.disconnect(); });

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
    <div class="settings-mobile-jump form-group">
      <label for="settings-jump">跳转到</label>
      <select id="settings-jump" :value="navigation.activeSection.value"
        @change="selectSection(($event.target as HTMLSelectElement).value)">
        <option v-for="section in sections" :key="section.id" :value="section.id">{{ section.label }}</option>
      </select>
    </div>
    <div ref="scrollContainer" class="settings-scroll" @scroll.passive="updateActiveSection"
      @wheel.passive="releaseAnchor" @touchstart.passive="releaseAnchor" @pointerdown="releaseAnchor" @keydown="releaseAnchor">
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
      <form ref="settingsForm" autocomplete="on" class="settings-form" @submit.prevent>
        <section v-for="section in sections" :key="section.id" :id="`settings-section-${section.id}`"
          class="settings-category" :aria-labelledby="`settings-heading-${section.id}`">
          <h2 :id="`settings-heading-${section.id}`" class="settings-category-heading">{{ section.label }}</h2>
          <component :is="section.component" />
        </section>
      </form>
    </div>
    <!-- 只由服务端草稿决定是否显示；滚到即时生效的外观区域也保留已有修改的保存入口。 -->
    <div
      v-if="!configLoadFailed && (config.dirty.value || saveFailed || busy.save)"
      class="save-bar"
    >
      <span class="settings-save-status" :class="{ 'settings-save-status--error': saveFailed && !busy.save }" role="status">{{ busy.save ? '正在保存…' : saveFailed ? '保存失败，请重试' : '有未保存的修改' }}</span>
      <button
        type="button"
        class="btn btn-primary save-btn"
        @click="handleSave"
        :disabled="busy.save || configLoadFailed"
      >
        <IconApp name="refresh" v-if="busy.save" class="spin" />
        <span>{{ busy.save ? '保存中' : (saveFailed ? '重试' : '保存') }}</span>
      </button>
    </div>
  </div>
</template>
