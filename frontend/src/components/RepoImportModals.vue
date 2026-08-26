<script setup lang="ts">
/**
 * 仓库导入双弹窗（导入列表 + 免责声明）。
 * 状态存于 useRepoImport 单例。此前 Modal 仅挂在 TasksView，
 * 导致设置·任务页点「从仓库导入」无反应、切到任务列表页才弹出的错位 bug，
 * 故提取为共享组件，两处入口（TasksView / TasksSettings）各自挂载。
 */
import Modal from "./common/Modal.vue";
import { useRepoImport } from "@/composables/useRepoImport";

const repo = useRepoImport();
</script>

<template>
  <Modal :open="repo.repoImport.value.visible" title="从云端仓库导入任务" size="lg" @close="repo.closeRepoImport">
    <div class="repo-import-source">
      <span class="repo-source-label">源：</span>
      <button class="btn btn-sm" :class="{ active: repo.repoImport.value.source === 'github' }" @click="repo.selectRepoSource('github')">GitHub</button>
      <button class="btn btn-sm" :class="{ active: repo.repoImport.value.source === 'gitee' }" @click="repo.selectRepoSource('gitee')">Gitee</button>
      <button class="btn btn-sm" :class="{ active: repo.repoImport.value.source === 'custom' }" @click="repo.selectRepoSource('custom')">自定义</button>
      <div v-if="repo.repoImport.value.source === 'custom'" class="repo-custom-url">
        <input v-model="repo.repoImport.value.url" type="text" class="input" placeholder="输入远程索引 URL" />
      </div>
    </div>
    <div class="repo-import-action">
      <button class="btn btn-primary btn-sm" @click="repo.fetchRepoIndex()" :disabled="repo.repoImport.value.loading">
        {{ repo.repoImport.value.loading ? '加载中...' : '加载索引' }}
      </button>
    </div>
    <div v-if="repo.repoImport.value.error" class="repo-import-error">{{ repo.repoImport.value.error }}</div>
    <div v-if="repo.repoImport.value.tasks.length > 0" class="repo-import-search">
      <input v-model="repo.repoImport.value.searchQuery" type="text" class="input" placeholder="搜索任务..." />
    </div>
    <div v-if="repo.repoImport.value.tasks.length > 0" class="repo-import-list">
      <div v-for="task in repo.filteredRepoTasks.value" :key="task.name" class="repo-import-item" @click="repo.confirmRepoImport(task)">
        <div class="repo-item-name">{{ task.name }}</div>
        <div class="repo-item-desc">{{ task.description }}</div>
        <div class="repo-item-meta">
          <span v-if="task.author" class="repo-item-author">{{ task.author }}</span>
          <span v-if="task.tags" class="repo-item-tags">{{ task.tags.join(', ') }}</span>
        </div>
      </div>
      <div v-if="repo.filteredRepoTasks.value.length === 0" class="repo-import-empty">无匹配</div>
    </div>
    <div v-else-if="!repo.repoImport.value.loading" class="repo-import-hint">
      <p>点击「加载索引」从远程仓库获取任务列表。</p>
      <p>你也可以 <a :href="repo.repoImport.value.url" target="_blank" rel="noopener">直接查看仓库</a>。</p>
    </div>
  </Modal>

  <!-- 免责弹窗：必须显式确认/取消，禁用遮罩关闭 -->
  <Modal :open="!!repo.repoImport.value.disclaimer" title="免责声明" :close-on-overlay="false" @close="repo.cancelRepoDisclaimer">
    <p>从远程仓库导入的任务由社区成员提供，未经审核验证。</p>
    <p class="repo-disclaimer-warn"><strong>请仔细阅读并确认任务内容后再使用。</strong>任务中填入的账号密码将在执行时提交到第三方网站，请确认目标网站可靠。</p>
    <div class="repo-disclaimer-actions">
      <button class="btn btn-secondary" @click="repo.cancelRepoDisclaimer()">取消</button>
      <button class="btn btn-primary" @click="repo.acceptRepoDisclaimer()">确认导入</button>
    </div>
  </Modal>
</template>

<style scoped>
.repo-import-source { display: flex; gap: 8px; align-items: center; margin-bottom: 12px; flex-wrap: wrap; }
.repo-source-label { font-size: 0.9em; color: var(--text-secondary); }
.repo-import-source .btn.active { background: var(--accent); color: var(--on-accent); }
.repo-custom-url { width: 100%; margin-top: 8px; }
.repo-custom-url .input { width: 100%; }
.repo-import-action { margin-bottom: 12px; }
.repo-import-error { color: var(--error); font-size: 0.9em; margin-bottom: 8px; }
.repo-import-search { margin-bottom: 12px; }
.repo-import-search .input { width: 100%; }
.repo-import-list { flex: 1; overflow-y: auto; display: flex; flex-direction: column; gap: 8px; }
.repo-import-item { padding: 10px 12px; border: 1px solid var(--border); border-radius: 8px; cursor: pointer; transition: background var(--dur-fast) var(--ease-out); }
.repo-import-item:hover { background: var(--bg-hover); }
.repo-item-name { font-weight: 600; }
.repo-item-desc { font-size: 0.85em; color: var(--text-secondary); margin-top: 2px; }
.repo-item-meta { display: flex; gap: 12px; margin-top: 6px; font-size: 0.8em; color: var(--text-tertiary); }
.repo-import-empty { text-align: center; color: var(--text-tertiary); padding: 24px; }
.repo-import-hint { color: var(--text-secondary); font-size: 0.9em; }
.repo-import-hint a { color: var(--accent); }
.repo-disclaimer-warn { color: var(--error); }
.repo-disclaimer-actions { display: flex; gap: 12px; justify-content: flex-end; margin-top: 20px; }
</style>
