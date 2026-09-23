<script setup lang="ts">
/** 任务页容器：承载「浏览器任务」「直连任务」「脚本」「定时任务」「AI 生成浏览器任务」五个面板。
 * 浏览器任务与脚本同源（GET /api/tasks 与 /api/scripts 返回同一混合列表，前端
 * 按 task_type 拆分）并共享拖拽排序；AI 生成是任务的生产入口，放在同一页免去
 * 「生成完再跳转找任务」的往返；定时任务原本是侧栏独立页，同属「可被触发执行的
 * 东西」故并入，减少一处导航项；直连任务是登录渠道之一（方案引用一个任务 id），
 * 与浏览器任务并列才能一眼看清两种登录方式的实现。
 *
 * 五个面板的切换在**左侧主侧栏**的「任务」分组里（见 AppSidebar + utils/navTree）：
 * 此前是页内一张竖排导航卡，在 2280px 视口下离左边缘 400px 有余、四周全是留白，
 * 像被丢在页面中间的一张卡片，而正文又与顶栏页标题不在同一列上。收编进侧栏后本
 * 组件只剩正文，且不再需要"导航 + 正文一起限宽居中"来避免割裂感。
 *
 * 路由名/路径一律不变（/tasks、/tasks/http、/tasks/scripts、/tasks/scheduled、
 * /tasks/ai）：深链、router/editorGuard.ts 的 `/tasks` 前缀判定、AppSidebar 的
 * `startsWith("tasks")` 高亮都依赖它们；当前面板仍由子路由表达，刷新与深链均可直达。 */
import IconApp from "@/components/common/IconApp.vue";
import { computed } from "vue";
import { useRoute, useRouter } from "vue-router";
import { activeChildId, TASK_NAV_CHILDREN } from "@/utils/navTree";

const route = useRoute();
const router = useRouter();

/** 当前子项：供窄屏 pill 行点亮（宽屏由侧栏点亮） */
const activeChild = computed(() => activeChildId(TASK_NAV_CHILDREN, String(route.name)));

function setTab(name: string): void {
  void router.push({ name });
}
</script>

<template>
  <div class="page-content tasks-page">
    <!-- 窄屏兜底导航：≤768px 时主侧栏只剩 64px 图标，子项文字放不下，侧栏里的二级
         导航整块隐藏（见 responsive.css），这里退化为横向可换行的 pill 行——否则窄屏
         只能靠手输地址到达其余四个面板。>768px 整块 display:none，不占位。
         数据与侧栏同源（TASK_NAV_CHILDREN），不复制第二份路由表。 -->
    <nav class="tasks-narrow-nav" aria-label="任务分类">
      <button
        v-for="child in TASK_NAV_CHILDREN"
        :key="child.id"
        type="button"
        class="tasks-narrow-nav-item"
        :class="{ active: activeChild === child.id }"
        :aria-current="activeChild === child.id ? 'page' : undefined"
        :title="child.title"
        @click="setTab(child.name)"
      >
        <IconApp :name="child.icon" class="tasks-narrow-nav-icon" />
        <span>{{ child.label }}</span>
      </button>
    </nav>

    <router-view />
  </div>
</template>
