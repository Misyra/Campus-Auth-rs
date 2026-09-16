<script setup lang="ts">
/** 任务页容器：承载「浏览器任务」「脚本」「定时任务」「AI 生成浏览器任务」四个面板。
 * 浏览器任务与脚本同源（GET /api/tasks 与 /api/scripts 返回同一混合列表，前端
 * 按 task_type 拆分）并共享拖拽排序；AI 生成是任务的生产入口，放在同一页免去
 * 「生成完再跳转找任务」的往返；定时任务原本是侧栏独立页，同属「可被触发执行的
 * 东西」故并入，减少一处导航项。Tab 状态由子路由表达（/tasks、/tasks/scripts、
 * /tasks/scheduled、/tasks/ai），刷新与深链均可直达。 */
import { computed } from "vue";
import { useRoute, useRouter } from "vue-router";

const route = useRoute();
const router = useRouter();

/** 当前 Tab：按子路由末段判定（/tasks/ai 不能靠 startsWith('/tasks/scripts') 反推） */
const activeTab = computed(() => {
  const seg = route.path.split("/").filter(Boolean)[1] ?? "";
  if (seg === "scripts") return "scripts";
  if (seg === "scheduled") return "scheduled";
  if (seg === "ai") return "ai";
  return "browser";
});

const TABS = [
  { id: "browser", label: "浏览器任务", name: "tasks-browser", title: "浏览器自动化步骤序列" },
  { id: "scripts", label: "脚本", name: "tasks-scripts", title: "定时执行的辅助脚本" },
  { id: "scheduled", label: "定时任务", name: "tasks-scheduled", title: "按时间或启动时机自动执行" },
  // 标签写明产物而非只写"AI 生成"：与左侧「浏览器任务」并列时，只说"AI 生成"
  // 看不出生成的是什么（脚本？定时任务？），补全宾语后一眼可知这一栏产出的是
  // 浏览器任务——它也正是登录实际执行的那一类。
  { id: "ai", label: "AI 生成浏览器任务", name: "tasks-ai", title: "用自然语言描述生成浏览器任务" },
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
