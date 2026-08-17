<script setup lang="ts">
import { computed, onMounted } from "vue";
import { useTasks } from "@/composables/useTasks";
import { useRepoImport } from "@/composables/useRepoImport";
import { useDragSort } from "@/utils/drag";
import { useDebug } from "@/composables/useDebug";
import Modal from "@/components/common/Modal.vue";
import { useRouter } from "vue-router";

const t = useTasks();
const repo = useRepoImport();
const debug = useDebug();
const router = useRouter();
// 拖拽排序：复用 useDragSort（历史遗留：已实现未接入）
const drag = useDragSort(t.tasks);

onMounted(() => { void t.fetchTasks(); });

// browserTasks 是实际列表
const browserTasks = computed(() => t.tasks.value);

function closeEditor() { t.editingTask.value = null; }
</script>

<template>
  <div class="page-content">
    <div class="tasks-grid">
      <div class="card">
        <div class="card-header">
          <h2>任务列表</h2>
          <div class="flex-row gap-sm">
            <button class="btn btn-sm" @click="t.importTask()" title="从文件导入任务">
              <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" class="icon-sm">
                <path d="M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4"/><polyline points="7 10 12 15 17 10"/><line x1="12" y1="15" x2="12" y2="3"/>
              </svg>
              导入
            </button>
            <button class="btn btn-sm" @click="repo.showRepoImport()" title="从云端仓库导入">
              <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" class="icon-sm">
                <path d="M21 12a9 9 0 0 1-9 9m9-9a9 9 0 0 0-9-9m9 9H3m9 9a9 9 0 0 1-9-9m9 9c1.657 0 3-4.03 3-9s-1.343-9-3-9m0 18c-1.657 0-3-4.03-3-9s1.343-9 3-9m-9 9a9 9 0 0 1 9-9"/>
              </svg>
              仓库导入
            </button>
            <a href="https://github.com/Misyra/Campus-Auth" target="_blank" rel="noopener" class="btn btn-sm" title="分享你的适配方案">
              <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" class="icon-sm">
                <circle cx="18" cy="5" r="3"/><circle cx="6" cy="12" r="3"/><circle cx="18" cy="19" r="3"/>
                <line x1="8.59" y1="13.51" x2="15.42" y2="17.49"/><line x1="15.41" y1="6.51" x2="8.59" y2="10.49"/>
              </svg>
              分享适配
            </a>
            <button class="btn btn-sm btn-primary" @click="t.showTaskEditor(null)">
              <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" class="icon-sm">
                <line x1="12" y1="5" x2="12" y2="19"/><line x1="5" y1="12" x2="19" y2="12"/>
              </svg>
              新建任务
            </button>
          </div>
        </div>
        <div class="card-body">
          <div v-if="!browserTasks.length" class="empty-state">
            <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.5"><rect x="3" y="3" width="18" height="18" rx="2"/><path d="M9 3v18M3 9h18"/></svg>
            <span>暂无任务配置</span>
          </div>
          <div v-else class="task-list">
            <div v-for="(task, index) in browserTasks" :key="task.id" class="task-item hover-lift"
              data-draggable-list :class="{ active: t.activeTaskId.value === task.id }"
              @dragstart="drag.handleDragStart($event, index)"
              @dragover="drag.onDragOver($event, index)"
              @drop="drag.onDrop($event, index)"
              @dragend="drag.onDragEnd($event)">
              <div class="task-drag-handle" title="拖拽排序"
                @mousedown="drag.onHandleMouseDown($event)"
                @mouseup="drag.onHandleMouseUp($event)">
                <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" class="icon-sm">
                  <line x1="8" y1="6" x2="16" y2="6"/><line x1="8" y1="12" x2="16" y2="12"/><line x1="8" y1="18" x2="16" y2="18"/>
                </svg>
              </div>
              <div class="task-info">
                <h3>{{ task.name }}</h3>
                <p v-if="task.description" class="task-desc">{{ task.description }}</p>
              </div>
              <div class="task-actions">
                <button class="btn btn-sm" @click="t.setActiveTask(task.id)" :disabled="t.activeTaskId.value === task.id">
                  {{ t.activeTaskId.value === task.id ? '使用中' : '使用' }}
                </button>
                <button class="btn btn-sm" @click="t.showTaskEditor(task.id)">编辑</button>
                <button class="btn btn-sm" @click="t.executeTask(task.id)" :disabled="t.executingIds.has(task.id)" title="立即执行（打卡/签到）">
                  {{ t.executingIds.has(task.id) ? '执行中...' : '执行' }}
                </button>
                <button class="btn btn-sm" @click="t.duplicateTask(task.id)" :disabled="t.duplicatingIds.has(task.id)" title="复制为新任务">
                  {{ t.duplicatingIds.has(task.id) ? '复制中...' : '复制' }}
                </button>
                <button class="btn btn-sm" @click="t.exportTask(task.id)" :disabled="t.exportingIds.has(task.id)" title="导出为JSON文件">
                  {{ t.exportingIds.has(task.id) ? '导出中...' : '导出' }}
                </button>
                <button class="btn btn-sm btn-danger" @click="t.deleteTask(task.id)" :disabled="task.id === 'default'">删除</button>
              </div>
            </div>
          </div>
        </div>
      </div>

      <!-- 任务编辑器 -->
      <div v-if="t.editingTask.value" class="card task-editor">
        <div class="card-header">
          <h2>{{ t.editingTask.value._isNew ? '新建任务' : '编辑任务' }}</h2>
          <button class="btn btn-icon-only" @click="closeEditor">
            <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
              <line x1="18" y1="6" x2="6" y2="18"/><line x1="6" y1="6" x2="18" y2="18"/>
            </svg>
          </button>
        </div>
        <div class="card-body">
          <div class="form-group">
            <label for="task-id">任务ID</label>
            <input id="task-id" v-model="t.editingTask.value.id" type="text" placeholder="task_id" :disabled="!t.editingTask.value._isNew" />
            <span class="hint">必须以字母开头，且只能包含字母、数字和下划线</span>
          </div>
          <div class="form-group">
            <label for="task-name">任务名称</label>
            <input id="task-name" v-model="t.editingTask.value.name" type="text" placeholder="我的登录任务" @input="t.syncMetaToJson()" />
          </div>
          <div class="form-group">
            <label for="task-desc">描述</label>
            <input id="task-desc" v-model="t.editingTask.value.description" type="text" placeholder="任务描述" @input="t.syncMetaToJson()" />
          </div>
          <div class="form-group">
            <label for="task-url">认证地址</label>
            <input id="task-url" v-model="t.editingTask.value.url" type="text" placeholder="不填则使用系统设置的认证地址" />
            <span class="hint">留空默认使用系统认证地址</span>
          </div>
          <div class="form-group">
            <label for="task-json">JSON 配置</label>
            <textarea id="task-json" v-model="t.editingTask.value.json" rows="12"
              placeholder="任务JSON配置" @input="t.validateJson(); t.syncJsonToMeta()"
              :class="{ 'json-invalid': t.jsonError.value, 'json-valid': t.editingTask.value.json && (t.editingTask.value.json as string).trim() && !t.jsonError.value }"></textarea>
            <div v-if="t.jsonError.value" class="json-error">{{ t.jsonError.value }}</div>
            <span v-else class="hint">编辑完整的任务配置JSON</span>
          </div>
          <div class="task-editor-actions">
            <button class="btn btn-secondary" @click="t.loadTemplate('default')">加载默认模板</button>
            <button class="btn btn-secondary" @click="t.formatJson()">格式化</button>
            <button class="btn btn-secondary" @click="debug.startDebug(t.editingTask.value.id)" :disabled="!t.editingTask.value.id.trim()" title="单步调试当前任务">调试</button>
          </div>
        </div>
        <div class="card-footer">
          <button class="btn btn-secondary" @click="closeEditor">取消</button>
          <button class="btn btn-primary" @click="t.saveTask()" :disabled="!!t.jsonError.value">保存任务</button>
        </div>
      </div>

      <!-- 帮助说明 -->
      <div v-else class="card">
        <div class="card-header"><h2>JSON 配置说明</h2></div>
        <div class="card-body">
          <div class="help-content">
            <h4>支持的步骤类型</h4>
            <ul>
              <li><code>input</code> - 输入文本</li>
              <li><code>click</code> - 点击元素</li>
              <li><code>click_select</code> - 点击并选择</li>
              <li><code>select</code> - 选择下拉框</li>
              <li><code>wait</code> - 等待元素出现</li>
              <li><code>wait_url</code> - 等待URL匹配</li>
              <li><code>eval</code> / <code>evaluate</code> - 执行 JS</li>
              <li><code>screenshot</code> - 保存截图</li>
              <li><code>sleep</code> - 等待指定时间</li>
              <li><code>ocr</code> - 验证码识别</li>
              <li><code>navigate</code> / <code>goto</code> - 跳转 URL</li>
              <li><code>assert_text</code> - 断言页面出现文本</li>
            </ul>
            <p class="hint" style="margin-top: 0.5rem;">提示：<code>ocr</code> 步骤需安装 OCR 依赖（ddddocr），未安装时会返回明确错误。</p>
            <h4>可用变量</h4>
            <ul>
              <li><code v-pre>{{USERNAME}}</code> / <code v-pre>{{PASSWORD}}</code> / <code v-pre>{{ISP}}</code> / <code v-pre>{{LOGIN_URL}}</code></li>
            </ul>
            <h4>示例</h4>
            <pre v-pre>{
  "variables": {"username": "{{USERNAME}}", "isp": "{{ISP}}"},
  "navigation_wait": 3,
  "steps": [
    { "type": "input", "selector": "#username", "value": "{{username}}", "description": "输入用户名" }
  ]
}</pre>
          </div>
        </div>
      </div>
    </div>
  </div>

<!-- ==================== 仓库导入弹窗 ==================== -->
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
/* 弹窗容器/遮罩已由公共 Modal.vue 提供，此处仅保留弹窗内容样式 */
.repo-import-source { display: flex; gap: 8px; align-items: center; margin-bottom: 12px; flex-wrap: wrap; }
.repo-source-label { font-size: 0.9em; color: var(--text-secondary); }
.repo-import-source .btn.active { background: var(--accent); color: #fff; }
.repo-custom-url { width: 100%; margin-top: 8px; }
.repo-custom-url .input { width: 100%; }
.repo-import-action { margin-bottom: 12px; }
.repo-import-error { color: var(--danger); font-size: 0.9em; margin-bottom: 8px; }
.repo-import-search { margin-bottom: 12px; }
.repo-import-search .input { width: 100%; }
.repo-import-list { flex: 1; overflow-y: auto; display: flex; flex-direction: column; gap: 8px; }
.repo-import-item { padding: 10px 12px; border: 1px solid var(--border); border-radius: 8px; cursor: pointer; transition: background 0.15s; }
.repo-import-item:hover { background: var(--bg-hover); }
.repo-item-name { font-weight: 600; }
.repo-item-desc { font-size: 0.85em; color: var(--text-secondary); margin-top: 2px; }
.repo-item-meta { display: flex; gap: 12px; margin-top: 6px; font-size: 0.8em; color: var(--text-tertiary); }
.repo-import-empty { text-align: center; color: var(--text-tertiary); padding: 24px; }
.repo-import-hint { color: var(--text-secondary); font-size: 0.9em; }
.repo-import-hint a { color: var(--accent); }
.repo-disclaimer-warn { color: var(--danger); }
.repo-disclaimer-actions { display: flex; gap: 12px; justify-content: flex-end; margin-top: 20px; }
</style>
