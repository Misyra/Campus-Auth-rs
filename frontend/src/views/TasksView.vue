<script setup lang="ts">
/** 任务页容器：以双 Tab 承载「浏览器任务」与「自定义脚本」两个面板。
 * 两者同源（GET /api/tasks 与 /api/scripts 返回同一混合列表，前端按 task_type 拆分）
 * 并共享拖拽排序与「设为活动任务」语义，故合并为一个页面而非两个导航项。
 * Tab 状态由子路由表达（/tasks 与 /tasks/scripts），刷新与深链均可直达。 */
import IconApp from "@/components/common/IconApp.vue";
import { computed } from "vue";
import { useRoute, useRouter } from "vue-router";

const route = useRoute();
const router = useRouter();

/** 当前 Tab：子路由 /tasks/scripts 为脚本，其余为浏览器任务 */
const activeTab = computed(() => (route.path.startsWith("/tasks/scripts") ? "scripts" : "browser"));

const TABS = [
  { id: "browser", label: "浏览器任务", name: "tasks-browser", title: "浏览器自动化步骤序列" },
  { id: "scripts", label: "脚本", name: "tasks-scripts", title: "定时执行的辅助脚本" },
] as const;

function setTab(name: string): void {
  void router.push({ name });
}
</script>

<template>
  <div class="page-content">
    <div class="settings-tabs card tasks-tabs">
      <button
        v-for="tab in TABS" :key="tab.id" type="button"
        class="settings-tab"
        :class="{ active: activeTab === tab.id }"
        :title="tab.title"
        @click="setTab(tab.name)"
      >
        <span>{{ tab.label }}</span>
      </button>
      <router-link :to="{ name: 'ai-task' }" class="btn btn-sm tasks-ai-link" title="用 AI 描述生成浏览器任务">
        <IconApp name="sparkles" class="icon-sm" />
        AI 生成任务
      </router-link>
    </div>

    <router-view />
  </div>
</template>
