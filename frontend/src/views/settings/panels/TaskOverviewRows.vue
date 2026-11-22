<script setup lang="ts">
/** 设置 · 任务概览面板体（schema 槽位）：当前方案绑定任务、任务库/文件导入/列表刷新入口。 */
import SettingsRow from "@/components/common/SettingsRow.vue";
import { computed, onMounted } from "vue";
import { useRouter } from "vue-router";
import { useTasks } from "@/composables/useTasks";
import { useTaskDirectory } from "@/composables/useTaskDirectory";
import { useProfiles } from "@/composables/useProfiles";
import { useRepoImport } from "@/composables/useRepoImport";
import { TASK_REPO_URL } from "@/utils/constants";

const t = useTasks();
const { httpTasks, scripts } = useTaskDirectory();
const { profiles, activeProfileId } = useProfiles();
const repo = useRepoImport();
const router = useRouter();

onMounted(() => { void t.fetchTasks(); });

/**
 * 「当前任务」= 当前方案的**当前渠道**实际会执行的那个任务。
 *
 * 启用任务按方案绑定（切方案即切任务），来源是方案摘要而非设置表单（这些字段属
 * Profile 域，不再随 `GET /api/config` 下发）。
 *
 * 必须按渠道取：三条渠道各绑各的字段，拿浏览器任务的 `active_task` 去代表直连或
 * 脚本渠道，会显示一个登录时**根本不会执行**的任务名——用户据此排查会走错方向。
 * HTTP/脚本显示名称与 ID，自动生成的 HTTP ID 不再单独占据概览主文案。
 */
const activeTaskName = computed(() => {
  const profile = profiles.value[activeProfileId.value];
  if (!profile) return "内置默认任务";
  if (profile.login_channel === "http") {
    const id = profile.active_http_task?.trim();
    if (!id) return "未绑定（HTTP 登录不可用）";
    const name = httpTasks.value.find((task) => task.id === id)?.name;
    return name ? `${name}（${id}）` : id;
  }
  if (profile.login_channel === "script") {
    const id = profile.active_script_task?.trim();
    if (!id) return "未绑定（脚本登录不可用）";
    const name = scripts.value.find((task) => task.id === id)?.name;
    return name ? `${name}（${id}）` : id;
  }
  const id = profile.active_task;
  if (!id) return "内置默认任务";
  const task = t.tasks.value.find((tk) => tk.id === id);
  return task?.name || id;
});
</script>

<template>
  <div>
    <SettingsRow label="当前任务" :description="activeTaskName || '未设置'">
      <button class="btn btn-primary btn-sm" type="button" @click="router.push({ name: 'tasks' })">管理任务</button>
    </SettingsRow>
    <SettingsRow label="现成任务库" description="选择社区已有任务，无需自己编写；导入后绑定到方案即可使用">
      <button class="btn btn-secondary btn-sm" type="button" @click="repo.showRepoImport()">选择现成任务</button>
      <a :href="TASK_REPO_URL" target="_blank" rel="noopener" class="btn btn-ghost btn-sm">任务仓库 →</a>
    </SettingsRow>
    <SettingsRow label="导入任务文件" description="从本地文件导入浏览器任务">
      <button class="btn btn-secondary btn-sm" type="button" @click="t.importTask()">从文件导入</button>
    </SettingsRow>
    <SettingsRow label="刷新任务列表" description="重新读取当前可用任务">
      <button class="btn btn-secondary btn-sm" type="button" @click="t.fetchTasks(true)">刷新列表</button>
    </SettingsRow>
  </div>
</template>
