<script setup lang="ts">
/**
 * 设置单页：顶部 sticky tab 条 + 8 分区连续表单，schema 驱动渲染（见 views/settings/schema.ts）。
 *
 * 结构：tab 条（图标+文字按钮，active 走 --accent-soft 浅底）+ 全分区连续挂载的表单。
 * 分区内导航由 tab 承担——点击平滑滚动到分区锚点并同步 URL hash（/settings#<id>），
 * 滚动时由 IntersectionObserver scrollspy 反向点亮 tab（F12：替代 getBoundingClientRect
 * 逐帧实测方案）；旧 /settings/<name> 子路由在 router 里重定向到对应 hash。
 * 搜索在顶栏（useSettingsSearch 单例），本页消费查询词并按 schema 匹配结果过滤分区。
 */
import IconApp from "@/components/common/IconApp.vue";
import SchemaRow from "@/views/settings/SchemaRow.vue";
import { computed, nextTick, onBeforeUnmount, onMounted, ref, watch } from "vue";
import { useRoute, useRouter } from "vue-router";
import { useConfig } from "@/composables/useConfig";
import { useStatus } from "@/composables/useStatus";
import { useToast } from "@/composables/useToast";
import { useSettingsNavigation } from "@/composables/useSettingsNavigation";
import { useSettingsSearch } from "@/composables/useSettingsSearch";
import { SETTINGS_SECTIONS } from "@/views/settings/schema";
import { SETTINGS_PANEL_SLOTS } from "@/views/settings/panels";
import { searchSettings, applySettingsSearch } from "@/utils/settingsSearch";

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
const sections = SETTINGS_SECTIONS;

// ===== 设置搜索（schema 数据驱动，见 utils/settingsSearch.ts） =====
// 输入框在顶栏（useSettingsSearch 单例），本页只消费查询词并应用过滤
const { query: searchQuery } = useSettingsSearch();
/** 匹配是纯数据计算（schema 不含 DOM），随查询词即时重算；DOM 类名按帧应用 */
const searchResult = computed(() => searchSettings(sections, searchQuery.value));
/** 关键词非空且没有任何分区命中时提示空态 */
const searchEmpty = computed(() => searchQuery.value.trim() !== "" && searchResult.value.matchedSections === 0);

let searchFrame: number | null = null;
watch(searchQuery, () => {
  if (searchFrame !== null) cancelAnimationFrame(searchFrame);
  searchFrame = requestAnimationFrame(() => {
    searchFrame = null;
    applySettingsSearch(settingsForm.value, searchResult.value);
  });
});

// ===== 分区元素缓存与 scrollspy（IntersectionObserver） =====

/** 分区元素缓存：分区列表静态且全程挂载，挂载后各查一次 */
const sectionElements = new Map<string, HTMLElement>();
/** 阅读带内可见的分区（id → 视口顶距离）：scrollspy 的活集 */
const intersectingTops = new Map<string, number>();
/** 当前 tab 高亮（点击与 spy 双向驱动） */
const activeSection = ref(sections[0]?.id ?? "");

function cacheSectionElements(): void {
  sectionElements.clear();
  const form = settingsForm.value;
  if (!form) return;
  for (const section of sections) {
    const element = form.querySelector<HTMLElement>(`#settings-section-${section.id}`);
    if (element) sectionElements.set(section.id, element);
  }
}

function setActiveSection(id: string): void {
  if (activeSection.value !== id) activeSection.value = id;
}

/**
 * scrollspy 判定线与锚定落点同源：tab 条高约 50px，分区 scroll-margin-top 76px +
 * content-wrapper scroll-padding 20px = 96px 落点。阅读带取 [96px, 40% 高度]，
 * 带内「最靠下」（最新进入阅读带）的分区即正在阅读的分区。
 */
const SPY_ROOT_MARGIN = "-96px 0px -60% 0px";
let sectionObserver: IntersectionObserver | null = null;

function handleIntersections(entries: IntersectionObserverEntry[]): void {
  for (const entry of entries) {
    const id = entry.target.id.slice("settings-section-".length);
    if (entry.isIntersecting) intersectingTops.set(id, entry.boundingClientRect.top);
    else intersectingTops.delete(id);
  }
  let best: string | null = null;
  let bestTop = -Infinity;
  // 阅读带内取 top 最大者：向下滚动时新分区进入带内即接管高亮，向上滚回时旧分区保留
  for (const [id, top] of intersectingTops) {
    if (top > bestTop) { bestTop = top; best = id; }
  }
  if (best) setActiveSection(best);
}

/** 滚动到底时最后分区可能够不到阅读带：命中底部即选中最后一个可见分区（与旧版一致） */
function handleScrollBottom(): void {
  const container = scrollContainer.value;
  if (!container) return;
  if (container.scrollTop + container.clientHeight < container.scrollHeight - 2) return;
  for (const section of [...sections].reverse()) {
    const element = sectionElements.get(section.id);
    if (element && element.offsetParent !== null) { setActiveSection(section.id); break; }
  }
}

function setupScrollSpy(): void {
  const container = scrollContainer.value;
  if (!container || typeof IntersectionObserver === "undefined") return;
  sectionObserver = new IntersectionObserver(handleIntersections, {
    root: container,
    rootMargin: SPY_ROOT_MARGIN,
    threshold: 0,
  });
  for (const element of sectionElements.values()) sectionObserver.observe(element);
  container.addEventListener("scroll", handleScrollBottom, { passive: true });
}

// ===== 分区定位（点击 tab / 旧深链 hash / 命令面板请求共用） =====

let resizeObserver: ResizeObserver | null = null;
let anchoredSection: string | null = null;

/** 搜索隐藏的分区不参与定位（display:none 时 offsetParent 为 null） */
function visibleSection(id: string): HTMLElement | null {
  const element = sectionElements.get(id) ?? null;
  return element && element.offsetParent === null ? null : element;
}

async function scrollToSection(id: string, behavior: ScrollBehavior = "smooth"): Promise<void> {
  await nextTick();
  const section = visibleSection(id);
  if (!section) return;
  anchoredSection = id;
  setActiveSection(id);
  // 滚动容器是外层 .content-wrapper（页面整体滚动），不手工换算 scrollTop：
  // 落点由分区的 scroll-margin-top（list.css）与容器 scroll-padding-block（layout.css）
  // 经 scrollIntoView 对齐，容器与内边距改动无需回这里重调常数
  section.scrollIntoView({ behavior, block: "start" });
}

function selectSection(id: string): void {
  void scrollToSection(id);
  // 同步 URL 锚点：replace 不产生历史项；hash 已一致时无需动作
  if (route.hash !== `#${id}`) void router.replace({ hash: `#${id}` });
}

/** 路由 hash（旧深链重定向 / 命令面板 / 站点内入口）驱动分区定位；vue-router 交付的 hash 已解码 */
watch(() => route.hash, (hash) => {
  const id = hash.replace(/^#/, "");
  if (!id || !sections.some((section) => section.id === id)) return;
  // tab 点击已先滚并 setActive，watch 里同一分区不再重复滚动
  if (id !== activeSection.value) void scrollToSection(id, "auto");
});

watch(navigation.scrollRequest, (request) => { if (request) void scrollToSection(request.id); });

/** 用户开始阅读或编辑后，让浏览器自然保持滚动，不再校准目录定位。 */
function releaseAnchor(): void { anchoredSection = null; }

// 滚动容器在 App.vue（<main class="content-wrapper">），本组件无法用模板事件绑定，
// 挂载后经 closest 解析再挂监听：wheel/touch/pointer/keyboard 任一用户输入即解除
// 锚定。挂在外层容器而非本页根上，列外空白区的滚轮也计入。
// passive 与旧模板修饰符一致（都不 preventDefault），卸载成对解绑。
function bindScrollContainer(): void {
  const container = scrollContainer.value;
  if (!container) return;
  container.addEventListener("wheel", releaseAnchor, { passive: true });
  container.addEventListener("touchstart", releaseAnchor, { passive: true });
  container.addEventListener("pointerdown", releaseAnchor);
  container.addEventListener("keydown", releaseAnchor);
}

function unbindScrollContainer(): void {
  const container = scrollContainer.value;
  if (!container) return;
  container.removeEventListener("wheel", releaseAnchor);
  container.removeEventListener("touchstart", releaseAnchor);
  container.removeEventListener("pointerdown", releaseAnchor);
  container.removeEventListener("keydown", releaseAnchor);
}

function panelSlot(key: string | undefined) {
  return key ? SETTINGS_PANEL_SLOTS[key] : undefined;
}

onMounted(() => {
  // 解析真正的滚动容器（App.vue 的 content-wrapper）：从本页表单向上取最近命中，
  // 不跨组件传 ref；解析必须先于首次定位与监听绑定
  scrollContainer.value = settingsForm.value?.closest<HTMLElement>(".content-wrapper") ?? null;
  bindScrollContainer();
  cacheSectionElements();
  applySettingsSearch(settingsForm.value, searchResult.value);
  setupScrollSpy();
  // 初始定位：URL hash（旧深链经 router 重定向）优先，否则落首个分区；即时滚动避免入场动画
  const initialId = route.hash.replace(/^#/, "");
  void scrollToSection(sections.some((section) => section.id === initialId) ? initialId : (sections[0]?.id ?? ""), "auto");
  // 环境状态和默认配置异步加载会改变前面区域的高度，首次定位随之校准。
  resizeObserver = new ResizeObserver(() => {
    if (anchoredSection) void scrollToSection(anchoredSection, "auto");
  });
  if (settingsForm.value) resizeObserver.observe(settingsForm.value);
  if (scrollContainer.value) resizeObserver.observe(scrollContainer.value);
});

onBeforeUnmount(() => {
  unbindScrollContainer();
  sectionObserver?.disconnect();
  resizeObserver?.disconnect();
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
  <div class="page-content settings-page page-in">
    <!-- 分区内导航：sticky 顶部 tab 条（窄屏横向滚动），替代旧 settings-mobile-jump select -->
    <nav class="settings-tabs" aria-label="设置分区导航">
      <button
        v-for="section in sections"
        :key="section.id"
        type="button"
        class="settings-tab"
        :class="{ active: activeSection === section.id }"
        :aria-current="activeSection === section.id ? 'location' : undefined"
        :title="section.title"
        @click="selectSection(section.id)"
      >
        <IconApp :name="section.icon" class="settings-tab-icon" />
        <span>{{ section.label }}</span>
      </button>
    </nav>
    <!-- 搜索输入框已合并进顶栏（设置路由下顶栏搜索即本页过滤器，见
         useSettingsSearch）；本页只保留无命中的空态提示。 -->
    <p v-if="searchEmpty" class="settings-search-empty" role="status">无匹配的设置项</p>
    <!-- 仅为承载水平内缩与表单间距；滚动已交给外层 content-wrapper，
         scroll/spy 监听在 onMounted 里挂到该容器（见 setupScrollSpy） -->
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

        子面板按钮仍应显式写 `type="button"`（本文件守卫只是纵深防御）；本处拦截保证
        即使漏写也只是不提交，而不会刷新页面。preventDefault 放在最前，避免任何
        子面板逻辑先跑再被导航打断。
      -->
      <form ref="settingsForm" autocomplete="on" class="settings-form" @submit.prevent>
        <section
          v-for="section in sections"
          :key="section.id"
          :id="`settings-section-${section.id}`"
          class="settings-category"
          :aria-labelledby="`settings-heading-${section.id}`"
        >
          <h2 :id="`settings-heading-${section.id}`" class="settings-category-heading">{{ section.label }}</h2>
          <div class="settings-list-page" :class="section.bodyClass">
            <template v-for="panel in section.panels" :key="panel.id">
              <!-- chromeless 面板：槽位组件自绘整面板（折叠卡 / 动态卡头） -->
              <component :is="panelSlot(panel.component)" v-if="panel.chromeless" />
              <section v-else class="settings-panel">
                <div class="settings-card-header">
                  <h2>{{ panel.title }}</h2>
                </div>
                <div class="card-body">
                  <template v-for="entry in panel.entries" :key="entry.id">
                    <component
                      v-if="'slot' in entry"
                      :is="panelSlot(entry.slot)"
                      :data-settings-unit="entry.id"
                    />
                    <SchemaRow v-else :row="entry" :data-settings-row="entry.id" />
                  </template>
                </div>
              </section>
            </template>
          </div>
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
