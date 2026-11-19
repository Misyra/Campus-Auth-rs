<script setup lang="ts">
/** 设置长页：分类目录定位连续表单，统一保留未保存内容与保存操作。 */
import IconApp from "@/components/common/IconApp.vue";
import MonitorSettings from "@/views/settings/MonitorSettings.vue";
import BrowserSettings from "@/views/settings/BrowserSettings.vue";
import TaskEnvironmentSettings from "@/views/settings/TaskEnvironmentSettings.vue";
import SystemSettings from "@/views/settings/SystemSettings.vue";
import NotificationSettings from "@/views/settings/NotificationSettings.vue";
import AppearanceView from "@/views/AppearanceView.vue";
import { computed, nextTick, onBeforeUnmount, onMounted, ref, watch } from "vue";
import { useRoute, useRouter } from "vue-router";
import { useConfig } from "@/composables/useConfig";
import { useStatus } from "@/composables/useStatus";
import { useToast } from "@/composables/useToast";
import { useSettingsNavigation } from "@/composables/useSettingsNavigation";
import { useSettingsSearch } from "@/composables/useSettingsSearch";
import { activeChildId, SETTINGS_NAV_CHILDREN } from "@/utils/navTree";
import { applySettingsSearch, type SettingsSearchResult } from "@/utils/settingsSearch";

const route = useRoute();
const router = useRouter();
const config = useConfig();
const { busy } = useStatus();
const { toastOnly } = useToast();

const navigation = useSettingsNavigation();
// 实际滚动容器是 App.vue 的 .content-wrapper（页面整体滚动），挂载后经 closest 解析
// 赋值，不是模板 ref
const scrollContainer = ref<HTMLElement | null>(null);
const settingsForm = ref<HTMLElement | null>(null);
const sectionComponents: Record<string, typeof MonitorSettings> = {
  monitor: MonitorSettings, browser: BrowserSettings, tasks: TaskEnvironmentSettings,
  system: SystemSettings, notification: NotificationSettings, appearance: AppearanceView,
};
const sections = SETTINGS_NAV_CHILDREN.map((child) => ({ ...child, component: sectionComponents[child.id] }));
let resizeObserver: ResizeObserver | null = null;
let anchoredSection: string | null = null;
let scrollFrame: number | null = null;
let searchFrame: number | null = null;

// ===== 设置搜索（纯前端过滤，见 utils/settingsSearch.ts） =====
// 输入框在顶栏（useSettingsSearch 单例），本页只消费查询词并应用过滤
const { query: searchQuery } = useSettingsSearch();
const searchStats = ref<SettingsSearchResult>({ totalSections: 0, matchedSections: 0, matchedRows: 0 });
/** 关键词非空且没有任何分区命中时提示空态 */
const searchEmpty = computed(() => searchQuery.value.trim() !== "" && searchStats.value.matchedSections === 0);

/** 关键词变化后扫分区可见性；空关键词即恢复完整列表。扫描只切换静态表单上的
 *  class（applySettingsSearch 先清后标、幂等），无需等 Vue flush。 */
function applySearch(): void {
  searchStats.value = applySettingsSearch(settingsForm.value, searchQuery.value);
}

// 快速连击按帧合并：一帧最多全量扫一次（6 分区 textContent + 12 组选择器），
// 打字时的按键延迟由此收口；卸载取消在途帧
watch(searchQuery, () => {
  if (searchFrame !== null) cancelAnimationFrame(searchFrame);
  searchFrame = requestAnimationFrame(() => {
    searchFrame = null;
    applySearch();
  });
});

/** 分区元素缓存：分区列表静态且全程挂载，挂载后各查一次，滚动/定位不再反复
 *  querySelector；搜索隐藏的分区视同不存在（display:none 时 offsetParent 为 null），
 *  可见性每次现判，不进缓存。 */
const sectionElements = new Map<string, HTMLElement>();

function cacheSectionElements(): void {
  sectionElements.clear();
  const form = settingsForm.value;
  if (!form) return;
  for (const section of sections) {
    const element = form.querySelector<HTMLElement>(`#settings-section-${section.id}`);
    if (element) sectionElements.set(section.id, element);
  }
}

function visibleSection(id: string): HTMLElement | null {
  const element = sectionElements.get(id) ?? null;
  return element && element.offsetParent === null ? null : element;
}

/** 旧分类深链仍可定位；所有区域保持挂载，目录跳转不会重建输入框。 */
async function scrollToSection(id: string): Promise<void> {
  await nextTick();
  const section = visibleSection(id);
  if (!section) return;
  anchoredSection = id;
  // 滚动容器是外层 .content-wrapper（页面整体滚动），不再手工换算 scrollTop：
  // 落点由分区的 scroll-margin-top（list.css）与容器 scroll-padding-block（layout.css）
  // 经 scrollIntoView 对齐，容器与内边距改动无需回这里重调常数
  section.scrollIntoView({ block: "start" });
  navigation.setActiveSection(id);
}

function selectSection(id: string): void {
  const child = sections.find((section) => section.id === id);
  if (!child) return;
  if (route.name === child.name) void scrollToSection(id);
  else void router.push({ name: child.name });
}

/** 以视口顶部正在阅读的区域高亮目录，滚动到底时选中最后一类；搜索隐藏的分区不参与。 */
function updateActiveSection(): void {
  const container = scrollContainer.value;
  if (!container) return;
  // 判定线（56 = 40 + 16）必须高于锚定落点（content-wrapper scroll-padding 20 + 分区
  // scroll-margin 20 = 40px）：刚锚定的分区要立即算作「正在阅读」，低于落点会在
  // 下一次滚动事件里回跳到上一分区
  const top = container.getBoundingClientRect().top + 56;
  let active = sections[0].id;
  for (const section of sections) {
    const element = visibleSection(section.id);
    if (element && element.getBoundingClientRect().top <= top) active = section.id;
  }
  if (container.scrollTop + container.clientHeight >= container.scrollHeight - 2) {
    for (const section of [...sections].reverse()) {
      if (visibleSection(section.id)) { active = section.id; break; }
    }
  }
  navigation.setActiveSection(active);
}

/** scroll 事件按帧合并：一帧最多一次目录高亮计算，卸载取消在途帧。 */
function scheduleActiveSectionUpdate(): void {
  if (scrollFrame !== null) return;
  scrollFrame = requestAnimationFrame(() => {
    scrollFrame = null;
    updateActiveSection();
  });
}

/** 用户开始阅读或编辑后，让浏览器自然保持滚动，不再校准目录定位。 */
function releaseAnchor(): void { anchoredSection = null; }

// 滚动容器在 App.vue（<main class="content-wrapper">），本组件无法用模板事件绑定，
// 挂载后经 closest 解析再挂监听：scroll 驱动目录高亮；wheel/touch/pointer/keyboard
// 任一用户输入即解除锚定。挂在外层容器而非本页根上，列外空白区的滚轮也计入。
// passive 与旧模板修饰符一致（都不 preventDefault），卸载成对解绑。
function bindScrollContainer(): void {
  const container = scrollContainer.value;
  if (!container) return;
  container.addEventListener("scroll", scheduleActiveSectionUpdate, { passive: true });
  container.addEventListener("wheel", releaseAnchor, { passive: true });
  container.addEventListener("touchstart", releaseAnchor, { passive: true });
  container.addEventListener("pointerdown", releaseAnchor);
  container.addEventListener("keydown", releaseAnchor);
}

function unbindScrollContainer(): void {
  const container = scrollContainer.value;
  if (!container) return;
  container.removeEventListener("scroll", scheduleActiveSectionUpdate);
  container.removeEventListener("wheel", releaseAnchor);
  container.removeEventListener("touchstart", releaseAnchor);
  container.removeEventListener("pointerdown", releaseAnchor);
  container.removeEventListener("keydown", releaseAnchor);
}

watch(() => route.name, (name) => {
  void scrollToSection(activeChildId(SETTINGS_NAV_CHILDREN, String(name)) ?? "monitor");
});
watch(navigation.scrollRequest, (request) => { if (request) void scrollToSection(request.id); });
onMounted(() => {
  // 解析真正的滚动容器（App.vue 的 content-wrapper）：从本页表单向上取最近命中，
  // 不跨组件传 ref；解析必须先于首次 scrollToSection 与监听绑定
  scrollContainer.value = settingsForm.value?.closest<HTMLElement>(".content-wrapper") ?? null;
  bindScrollContainer();
  cacheSectionElements();
  applySearch();
  void scrollToSection(activeChildId(SETTINGS_NAV_CHILDREN, String(route.name)) ?? "monitor");
  // 环境状态和默认配置异步加载会改变前面区域的高度，首次定位随之校准。
  resizeObserver = new ResizeObserver(() => {
    if (anchoredSection) void scrollToSection(anchoredSection);
    else updateActiveSection();
  });
  if (settingsForm.value) resizeObserver.observe(settingsForm.value);
  if (scrollContainer.value) resizeObserver.observe(scrollContainer.value);
});
onBeforeUnmount(() => {
  unbindScrollContainer();
  resizeObserver?.disconnect();
  if (scrollFrame !== null) cancelAnimationFrame(scrollFrame);
  if (searchFrame !== null) cancelAnimationFrame(searchFrame);
  // 离开设置页即清空共享查询词：顶栏输入框只在设置路由渲染，
  // 残留过滤会让下次进入时表单缺分区且无输入框可清
  searchQuery.value = "";
});

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
    <!-- 搜索输入框已合并进顶栏（设置路由下顶栏搜索即本页过滤器，见
         useSettingsSearch）；本页只保留无命中的空态提示。 -->
    <p v-if="searchEmpty" class="settings-search-empty" role="status">无匹配的设置项</p>
    <!-- 仅为承载水平内缩与表单间距；滚动已交给外层 content-wrapper，
         scroll/输入监听在 onMounted 里挂到该容器（见 bindScrollContainer） -->
    <div class="settings-scroll">
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
