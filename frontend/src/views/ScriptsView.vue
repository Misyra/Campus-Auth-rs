<script setup lang="ts">
import { computed, onMounted } from "vue";
import { useScripts } from "@/composables/useScripts";
import { useTasks } from "@/composables/useTasks";
import { useDragSort } from "@/utils/drag";
import CustomSelect from "@/components/common/CustomSelect.vue";
import type { SelectOption } from "@/components/common/CustomSelect.vue";

const {
  scripts,
  availableBinaries,
  editingTask,
  runningIds,
  getBinaryName,
  fetchScripts,
  showScriptEditor,
  closeScriptEditor,
  saveScript,
  deleteScript,
  runScript,
  exportScript,
  importScript,
  setActiveScript,
  loadScriptTemplate,
  onBinarySelectChange,
} = useScripts();

const { activeTaskId, tasks: browserTasks } = useTasks();

onMounted(() => { void fetchScripts(); });

// B1：拖拽排序复用 useDragSort——本列表（脚本）重排，任务与脚本两组顺序
// 均随请求全量持久化（后端 order 接口整体替换，漏传的一组会被清空）
const drag = useDragSort(scripts, { tasks: browserTasks, scripts });

// 关闭编辑器走 composable 的 dirty 确认路径（对齐 ProfilesView 行为）
function closeEditor() { void closeScriptEditor(); }

// ---- 二进制选项 ----
const binaryOptions = computed<SelectOption[]>(() => {
  const opts: SelectOption[] = [{ value: "", label: "Python (项目内解释器)" }];
  for (const b of availableBinaries.value) {
    opts.push({ value: b.path, label: `${b.name} (${b.path})` });
  }
  opts.push({ value: "__custom__", label: "自定义可执行文件 (.exe)" });
  return opts;
});
</script>

<template>
  <div class="page-content">
    <div class="tasks-grid">
      <div class="card">
        <div class="card-header">
          <h2>自定义脚本</h2>
          <div class="flex-row gap-sm">
            <button class="btn btn-sm" @click="importScript" title="从文件导入脚本">
              <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" class="icon-sm">
                <path d="M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4"/>
                <polyline points="7 10 12 15 17 10"/><line x1="12" y1="15" x2="12" y2="3"/>
              </svg>
              导入
            </button>
            <button class="btn btn-sm btn-primary" @click="showScriptEditor(null)">
              <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" class="icon-sm">
                <line x1="12" y1="5" x2="12" y2="19"/><line x1="5" y1="12" x2="19" y2="12"/>
              </svg>
              新建脚本
            </button>
          </div>
        </div>
        <div class="card-body">
          <div v-if="!scripts.length" class="empty-state">
            <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.5" aria-hidden="true">
              <polyline points="16 18 22 12 16 6"/><polyline points="8 6 2 12 8 18"/>
            </svg>
            <span>暂无自定义脚本</span>
            <span class="hint">支持 Python、Shell 或任意可执行程序，无需浏览器，资源占用更低</span>
          </div>
          <div v-else class="task-list">
            <div
              v-for="(script, index) in scripts" :key="script.id"
              class="task-item hover-lift"
              data-draggable-list
              :class="{ active: activeTaskId === script.id }"
              @dragstart="drag.handleDragStart($event, index)"
              @dragover="drag.onDragOver($event, index)"
              @drop="drag.onDrop($event, index)"
              @dragend="drag.onDragEnd($event)"
            >
              <div class="task-drag-handle" title="拖拽排序"
                @mousedown="drag.onHandleMouseDown($event)"
                @mouseup="drag.onHandleMouseUp($event)">
                <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" class="icon-sm">
                  <line x1="8" y1="6" x2="16" y2="6"/><line x1="8" y1="12" x2="16" y2="12"/><line x1="8" y1="18" x2="16" y2="18"/>
                </svg>
              </div>
              <div class="task-info">
                <h3>{{ script.name }}</h3>
                <p class="task-desc">
                  <span v-if="script.binary_path" class="binary-badge">{{ getBinaryName(script.binary_path) }}</span>
                  <span v-else class="binary-badge binary-default">Python</span>
                  <span v-if="script.description"> · {{ script.description }}</span>
                </p>
              </div>
              <div class="task-actions">
                <button class="btn btn-sm" @click="setActiveScript(script.id)" :disabled="activeTaskId === script.id">
                  {{ activeTaskId === script.id ? '使用中' : '使用' }}
                </button>
                <button class="btn btn-sm" @click="showScriptEditor(script.id)">编辑</button>
                <button class="btn btn-sm" @click="runScript(script.id)" :disabled="runningIds.has(script.id)" title="立即执行此脚本">
                  {{ runningIds.has(script.id) ? '运行中...' : '运行' }}
                </button>
                <button class="btn btn-sm" @click="exportScript(script.id)" title="导出脚本文件">导出</button>
                <button class="btn btn-sm btn-danger" @click="deleteScript(script.id)">删除</button>
              </div>
            </div>
          </div>
        </div>
      </div>

      <!-- 编辑器 -->
      <div v-if="editingTask" class="card task-editor">
        <div class="card-header">
          <h2>{{ editingTask._isNew ? '新建脚本' : '编辑脚本' }}</h2>
          <button class="btn btn-icon-only" @click="closeEditor">
            <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
              <line x1="18" y1="6" x2="6" y2="18"/><line x1="6" y1="6" x2="18" y2="18"/>
            </svg>
          </button>
        </div>
        <div class="card-body">
          <div class="form-group">
            <label for="script-id">脚本ID</label>
            <input id="script-id" v-model="editingTask.id" type="text" placeholder="my_script" :disabled="!editingTask._isNew" />
            <span class="hint">必须以字母开头，且只能包含字母、数字和下划线</span>
          </div>
          <div class="form-group">
            <label for="script-name">名称</label>
            <input id="script-name" v-model="editingTask.name" type="text" placeholder="我的登录脚本" />
          </div>
          <div class="form-group">
            <label for="script-desc">描述</label>
            <input id="script-desc" v-model="editingTask.description" type="text" placeholder="脚本描述（可选）" />
          </div>
          <div class="form-group">
            <label for="script-binary">执行程序</label>
            <div class="binary-input-group">
              <CustomSelect v-model="editingTask.binary_path" :options="binaryOptions" @change="onBinarySelectChange" />
              <input v-if="editingTask.binary_path === '__custom__'"
                v-model="editingTask._customBinary" type="text"
                placeholder="输入可执行文件的完整路径（.exe）" class="mt-2" />
            </div>
            <span class="hint" v-if="editingTask.binary_path && editingTask.binary_path !== '__custom__'">当前: {{ editingTask.binary_path }}</span>
            <span class="hint" v-else>选择执行此脚本的程序；Python 使用项目内解释器，仅支持 shell / bat / python / exe 四类</span>
          </div>
          <div class="form-group">
            <label for="script-content">脚本内容</label>
            <textarea id="script-content" v-model="editingTask.content" rows="18"
              placeholder="#!/usr/bin/env python3&#10;import httpx&#10;&#10;resp = httpx.post('http://...', data={...})&#10;..."
              class="script-editor"></textarea>
            <span class="hint">脚本可直接硬编码账号密码等参数，stdout 输出会记录到日志，方便调试</span>
          </div>
          <div class="task-editor-actions">
            <button class="btn btn-secondary" @click="loadScriptTemplate()">加载示例模板</button>
          </div>
        </div>
        <div class="card-footer">
          <button class="btn btn-secondary" @click="closeEditor">取消</button>
          <button class="btn btn-primary" @click="saveScript()">保存脚本</button>
        </div>
      </div>

      <!-- 帮助说明 -->
      <div v-else class="card">
        <div class="card-header"><h2>脚本说明</h2></div>
        <div class="card-body">
          <div class="help-content">
            <h4>执行程序</h4>
            <p>每个脚本可以指定不同的执行程序，仅支持 shell / bat / python / exe 四类：</p>
            <ul>
              <li><strong>Python</strong>（默认）— 使用项目内 Python 解释器执行</li>
              <li><strong>Shell / Bat</strong> — 支持 cmd（Windows）或 bash/sh（Linux），也可自定义 shell 路径</li>
              <li><strong>自定义</strong> — 任意可执行文件（.exe）</li>
            </ul>
            <p>如需其他解释器（如 node、powershell），请写在 bat 或 shell 脚本中自定义调用。</p>
            <h4>工作原理</h4>
            <p>脚本在子进程中执行，通过 HTTP 请求直接登录校园网，无需启动浏览器，资源占用极低。</p>
            <h4>输出说明</h4>
            <p>脚本只需发送请求，<strong>登录是否成功由系统网络检测自动判断</strong>。</p>
            <h4>示例（Python httpx）</h4>
            <pre>#!/usr/bin/env python3
import httpx

resp = httpx.post("http://10.0.0.1/login", data={
    "username": "your_username",
    "password": "your_password",
    "operator": "cmcc",
})
print(f"HTTP {resp.status_code}")</pre>
            <h4>注意事项</h4>
            <ul>
              <li>脚本超时默认 60 秒</li>
              <li>脚本设为活动任务后，自动监控会使用脚本登录</li>
              <li>stderr 输出会记录到日志，不影响结果判断</li>
            </ul>
          </div>
        </div>
      </div>
    </div>
  </div>
</template>
