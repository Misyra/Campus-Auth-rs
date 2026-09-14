<script setup lang="ts">
/** 脚本面板：脚本列表、编辑器、运行与导入导出。
 * 由「任务」页容器以子路由渲染；脚本不参与登录（登录认证由方案里的登录方式负责），
 * 只作为「定时任务」的执行目标或手动运行的通用自动化动作。 */
import IconApp from "@/components/common/IconApp.vue";
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
  scriptSaving,
  deleteScript,
  runScript,
  exportScript,
  importScript,
  loadScriptTemplate,
  onBinarySelectChange,
} = useScripts();

const { tasks: browserTasks } = useTasks();

onMounted(() => { void fetchScripts(); });

// B1：拖拽排序复用 useDragSort——本列表（脚本）重排，任务与脚本两组顺序
// 均随请求全量持久化（后端 order 接口整体替换，漏传的一组会被清空）
const drag = useDragSort(scripts, { tasks: browserTasks, scripts });

// 关闭编辑器走 composable 的 dirty 确认路径（对齐 ProfilesView 行为）
function closeEditor() { void closeScriptEditor(); }

/** 从空态直接创建带示例内容的脚本，先等待编辑器草稿初始化完成。 */
async function createExampleScript(): Promise<void> {
  await showScriptEditor();
  if (editingTask.value) loadScriptTemplate();
}

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
  <div class="tasks-grid">
    <div class="card">
      <div class="card-header">
        <h2>自定义脚本</h2>
        <div class="card-actions">
          <button class="btn btn-sm" @click="importScript" title="从文件导入脚本">
            <IconApp name="upload" class="icon-sm" />
            导入
          </button>
          <button class="btn btn-sm btn-primary" @click="showScriptEditor()">
            <IconApp name="plus" class="icon-sm" />
            新建脚本
          </button>
        </div>
      </div>
      <div class="card-body">
        <div v-if="!scripts.length" class="empty-state">
          <IconApp name="code" :stroke-width="1.5" />
          <span>暂无自定义脚本</span>
          <span class="hint">支持 Python、Shell 或任意可执行程序，用于定时执行打卡、签到等辅助动作</span>
          <div class="empty-actions">
            <button class="btn btn-sm btn-primary" type="button" @click="showScriptEditor()">
              <IconApp name="plus" />新建脚本
            </button>
            <button class="btn btn-sm btn-secondary" type="button" @click="void createExampleScript()">加载示例模板</button>
          </div>
        </div>
        <div v-else class="task-list">
          <div
            v-for="(script, index) in scripts" :key="script.id"
            class="task-item hover-lift"
            data-draggable-list
            @dragstart="drag.handleDragStart($event, index)"
            @dragover="drag.onDragOver($event, index)"
            @drop="drag.onDrop($event, index)"
            @dragend="drag.onDragEnd($event)"
          >
            <div class="task-drag-handle" title="拖拽排序"
              @mousedown="drag.onHandleMouseDown($event)"
              @mouseup="drag.onHandleMouseUp($event)">
              <IconApp name="list" class="icon-sm" />
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
          <IconApp name="close" />
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
          <input id="script-name" v-model="editingTask.name" type="text" placeholder="我的打卡脚本" />
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
            placeholder="#!/usr/bin/env python3&#10;from urllib.request import urlopen&#10;&#10;..."
            class="script-editor"></textarea>
          <span class="hint">脚本可直接硬编码账号密码等参数，stdout 输出会记录到日志，方便调试</span>
        </div>
        <div class="task-editor-actions">
          <button class="btn btn-secondary" @click="loadScriptTemplate()">加载示例模板</button>
        </div>
      </div>
      <div class="card-footer">
        <button class="btn btn-secondary" @click="closeEditor">取消</button>
        <button class="btn btn-primary" @click="saveScript()" :disabled="scriptSaving">保存脚本</button>
      </div>
    </div>

    <!-- 帮助说明 -->
    <div v-else class="card">
      <div class="card-header"><h2>脚本说明</h2></div>
      <div class="card-body">
        <div class="help-content">
          <div class="help-tip">
            <span>脚本用于<b>定时执行的辅助动作</b>（打卡、签到等）。若要登录校园网，请改用方案里的<b>直连请求</b>，无需编写代码。</span>
          </div>
          <h4>用途</h4>
          <p>脚本由「定时任务」页调度，或在列表里点「运行」立即执行一次；执行结果按退出码判定，退出码为 0 视为成功。</p>
          <p>脚本<strong>不参与登录认证</strong>：校园网登录由方案的「登录方式」负责（浏览器自动化或直连请求），两者互不影响。</p>
          <h4>执行程序</h4>
          <p>每个脚本可以指定不同的执行程序，仅支持 shell / bat / python / exe 四类：</p>
          <ul>
            <li><strong>Python</strong>（默认）— 使用项目内 Python 解释器执行</li>
            <li><strong>Shell / Bat</strong> — 支持 cmd（Windows）或 bash/sh（Linux），也可自定义 shell 路径</li>
            <li><strong>自定义</strong> — 任意可执行文件（.exe）</li>
          </ul>
          <p>如需其他解释器（如 node、powershell），请写在 bat 或 shell 脚本中自定义调用。</p>
          <h4>输出说明</h4>
          <p>stdout 与 stderr 都会记录到日志，方便调试；stderr 输出不影响结果判定。</p>
          <h4>示例（Python 标准库）</h4>
          <pre>#!/usr/bin/env python3
# 示例：每日签到；退出码 0 表示成功
from urllib.request import urlopen

with urlopen("http://example.com/checkin", timeout=30) as response:
    print(f"HTTP {response.status}")</pre>
          <h4>注意事项</h4>
          <ul>
            <li>脚本超时默认 60 秒</li>
            <li>stderr 输出会记录到日志，不影响结果判断</li>
          </ul>
        </div>
      </div>
    </div>
  </div>
</template>
