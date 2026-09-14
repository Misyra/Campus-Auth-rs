<script setup lang="ts">
/** 任务页容器：以三 Tab 承载「浏览器任务」「脚本」「AI 生成」三个面板。
 * 浏览器任务与脚本同源（GET /api/tasks 与 /api/scripts 返回同一混合列表，前端
 * 按 task_type 拆分）并共享拖拽排序；AI 生成是任务的生产入口，放在同一页免去
 * 「生成完再跳转找任务」的往返。Tab 状态由子路由表达（/tasks、/tasks/scripts、
 * /tasks/ai），刷新与深链均可直达。 */
import { computed } from "vue";
import { useRoute, useRouter } from "vue-router";

const route = useRoute();
const router = useRouter();

/** 当前 Tab：按子路由末段判定（/tasks/ai 不能靠 startsWith('/tasks/scripts') 反推） */
const activeTab = computed(() => {
  const seg = route.path.split("/").filter(Boolean)[1] ?? "";
  if (seg === "scripts") return "scripts";
  if (seg === "ai") return "ai";
  return "browser";
});

const TABS = [
  { id: "browser", label: "浏览器任务", name: "tasks-browser", title: "浏览器自动化步骤序列" },
  { id: "scripts", label: "脚本", name: "tasks-scripts", title: "定时执行的辅助脚本" },
  { id: "ai", label: "AI 生成", name: "tasks-ai", title: "用 AI 描述生成浏览器任务" },
] as const;

function setTab(name: string): void {
  void router.push({ name });
}
</script>

<template>
  <div class="page-content">
    <div class="settings-tabs card tasks-tabs" role="tablist">
      <button
        v-for="tab in TABS" :key="tab.id" type="button"
        class="settings-tab"
        role="tab"
        :aria-selected="activeTab === tab.id"
        :class="{ active: activeTab === tab.id }"
        :title="tab.title"
        @click="setTab(tab.name)"
      >
        <span>{{ tab.label }}</span>
      </button>
    </div>

    <router-view />
  </div>
</template>
