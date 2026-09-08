<script setup lang="ts">
/** 设置 · 任务页：任务概览快捷入口 + 任务录制器与编写指南。
 * OCR 依赖与验证码识别已收敛至环境页，本页不再重复。 */
import IconApp from "@/components/common/IconApp.vue";
import { computed, onMounted } from "vue";
import { useRouter } from "vue-router";
import { useTasks } from "@/composables/useTasks";
import { useRepoImport } from "@/composables/useRepoImport";

const t = useTasks();
const repo = useRepoImport();
const router = useRouter();

onMounted(() => { void t.fetchTasks(); });

const activeTaskName = computed(() => {
  const id = t.activeTaskId.value;
  const task = t.tasks.value.find((tk) => tk.id === id);
  return task?.name || id;
});
</script>

<template>
  <div class="settings-panel-grid settings-panel-grid--task">
    <!-- 任务概览 -->
    <section class="card task-overview-card">
      <div class="settings-card-header">
        <IconApp name="grid" class="settings-card-icon" />
        <h2>任务概览</h2>
      </div>
      <div class="card-body">
        <div class="task-overview-compact">
          <div class="task-overview-left">
            <span class="task-overview-label">当前任务</span>
            <span class="task-overview-name">{{ activeTaskName || '未设置' }}</span>
          </div>
          <div class="task-overview-right">
            <button class="btn btn-primary btn-sm" type="button" @click="router.push({ name: 'tasks' })">管理任务</button>
          </div>
        </div>
        <div class="task-overview-actions">
          <button class="btn btn-secondary btn-sm" type="button" @click="t.importTask()">从文件导入</button>
          <button class="btn btn-secondary btn-sm" type="button" @click="repo.showRepoImport()">从仓库导入</button>
          <button class="btn btn-secondary btn-sm" type="button" @click="t.fetchTasks(true)">刷新列表</button>
          <a href="https://github.com/Misyra/campus-auth-tasks" target="_blank" rel="noopener" class="btn btn-ghost btn-sm">任务仓库 →</a>
        </div>
      </div>
    </section>

    <!-- 任务录制器 -->
    <section class="card">
      <div class="settings-card-header">
        <IconApp name="target" class="settings-card-icon" />
        <h2>任务录制器</h2>
      </div>
      <div class="card-body">
        <div class="task-recorder-section">
          <p class="task-recorder-desc">在登录页点选账号框、密码框、登录按钮等元素，自动生成任务步骤。</p>
          <div class="task-recorder-actions">
            <a href="/api/tools/task-recorder.user.js" class="btn btn-primary">
              <IconApp name="upload" class="icon-sm" />
              安装录制器脚本
            </a>
            <a href="/api/docs/task-writing-guide" download="task-writing-guide.md" class="btn btn-secondary">
              <IconApp name="file-text" class="icon-sm" />
              导出编写指南
            </a>
          </div>
          <div class="task-recorder-note">需先安装 <a href="https://www.tampermonkey.net/" target="_blank" rel="noopener">Tampermonkey</a> 扩展，再安装录制器脚本；在登录页点击浮动按钮开始录制。</div>
          <div class="task-recorder-note">编写规范见 <a href="/api/docs/task-writing-guide" target="_blank">任务编写指南</a> 与 <a href="/api/docs/task-manual" target="_blank">任务手册</a>。</div>
        </div>
      </div>
    </section>
  </div>
</template>
