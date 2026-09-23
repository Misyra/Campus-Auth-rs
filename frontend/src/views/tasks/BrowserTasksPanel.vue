<script setup lang="ts">
/** 浏览器任务面板：浏览器任务的增删改查、立即执行、仓库导入与拖拽排序。
 * 由「任务」页容器以子路由渲染；列表与脚本面板同源（useTaskDirectory 单次拉取）。 */
import IconApp from "@/components/common/IconApp.vue";
import FieldHelp from "@/components/common/FieldHelp.vue";
import { computed, onMounted } from "vue";
import { useTasks } from "@/composables/useTasks";
import { useScripts } from "@/composables/useScripts";
import { useRepoImport } from "@/composables/useRepoImport";
import { useDragSort } from "@/utils/drag";
import { useDebug } from "@/composables/useDebug";
import { TASK_REPO_URL, TUTORIAL_VIDEO_URL } from "@/utils/constants";

const t = useTasks();
const s = useScripts();
const repo = useRepoImport();
const debug = useDebug();
// B1：拖拽排序必须互传全量——后端 order 接口会整体替换任务与脚本两组顺序，
// 漏传的一组会被清空，因此脚本列表也要一并传入用于持久化
const drag = useDragSort(t.tasks, { tasks: t.tasks, scripts: s.scripts });

onMounted(() => { void t.fetchTasks(); });

// browserTasks 是实际列表
const browserTasks = computed(() => t.tasks.value);

// 关闭编辑器走 composable 的 dirty 确认路径（对齐 ProfilesView 行为）
function closeEditor() { void t.closeTaskEditor(); }
</script>

<template>
  <div class="tasks-grid" :class="{ 'tasks-grid--empty': !browserTasks.length }">
    <div class="card">
      <div class="card-header">
        <h2>浏览器任务</h2>
        <!-- 归属提示收进标题旁的 `?`（与「直连任务」Tab 同口径）：任务是"编辑对象"，
             真正生效由方案绑定决定，这属于"第一次配置要读一次"的解释，不该常驻一整条横幅 -->
        <FieldHelp
          text="任务本身不会自动运行：要到「方案」页选中这个任务并保存，自动登录才会用它。此处仅编辑与调试。"
        />
        <div class="card-actions">
          <button class="btn btn-sm" @click="t.importTask()" title="从文件导入任务">
            <IconApp name="upload" class="icon-sm" />
            导入
          </button>
          <button class="btn btn-sm" @click="repo.showRepoImport('browser')" title="从云端仓库导入">
            <IconApp name="globe-grid" class="icon-sm" />
            仓库导入
          </button>
          <!-- 指向**任务**仓库而非主程序仓库：这一入口的目的是贡献自己适配的登录任务，
               而任务由独立仓库承载（见 TASK_REPO_URL 的说明）。此前误指主程序仓库，
               点过去找不到任何可分享的任务。 -->
          <a :href="TASK_REPO_URL" target="_blank" rel="noopener" class="btn btn-sm" title="把你的登录任务分享到任务仓库，供他人一键导入">
            <IconApp name="share-2" class="icon-sm" />
            分享适配
          </a>
          <button class="btn btn-sm btn-primary" @click="t.showTaskEditor()">
            <IconApp name="plus" class="icon-sm" />
            新建任务
          </button>
        </div>
      </div>
      <div class="card-body">
        <div v-if="!browserTasks.length" class="empty-state">
          <IconApp name="layout" :stroke-width="1.5" />
          <span>暂无任务配置</span>
        </div>
        <div v-else class="task-list">
          <div v-for="(task, index) in browserTasks" :key="task.id" class="task-item hover-lift"
            data-draggable-list
            @dragstart="drag.handleDragStart($event, index)"
            @dragover="drag.onDragOver($event, index)"
            @drop="drag.onDrop($event, index)"
            @dragend="drag.onDragEnd($event)">
            <div class="task-drag-handle" title="拖拽排序"
              @mousedown="drag.onHandleMouseDown($event)"
              @mouseup="drag.onHandleMouseUp($event)">
              <IconApp name="list" class="icon-sm" />
            </div>
            <div class="task-info">
              <h3>{{ task.name }}</h3>
              <p v-if="task.description" class="task-desc">{{ task.description }}</p>
            </div>
            <div class="task-actions">
              <button class="btn btn-sm btn-icon-only" @click="t.showTaskEditor(task.id)" title="编辑任务">
                <IconApp name="pencil" class="icon-sm" />
              </button>
              <button class="btn btn-sm btn-icon-only" @click="debug.startDebug(task.id)" title="调试（单步执行当前任务步骤）">
                <IconApp name="bug" class="icon-sm" />
              </button>
              <button class="btn btn-sm btn-icon-only" @click="t.duplicateTask(task.id)" :disabled="t.duplicatingIds.has(task.id)" title="复制为新任务">
                <IconApp name="copy" class="icon-sm" />
              </button>
              <button class="btn btn-sm btn-icon-only" @click="t.exportTask(task.id)" :disabled="t.exportingIds.has(task.id)" title="导出为JSON文件">
                <IconApp name="download" class="icon-sm" />
              </button>
              <button class="btn btn-sm btn-icon-only btn-danger" @click="t.deleteTask(task.id)" :disabled="task.id === 'default'" title="删除任务">
                <IconApp name="trash" class="icon-sm" />
              </button>
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
          <IconApp name="close" />
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
        <button class="btn btn-primary" @click="t.saveTask()" :disabled="!!t.jsonError.value || t.taskSaving.value">保存任务</button>
      </div>
    </div>

    <!-- 帮助说明 -->
    <div v-else class="card">
      <div class="card-header"><h2>JSON 配置说明</h2></div>
      <div class="card-body">
        <div class="help-content">
          <div class="help-tip">
            <span>不想手写 JSON？<b>仓库导入</b>里有其他人适配好的校园网登录任务，一键即可获取。</span>
            <button class="btn btn-sm" @click="repo.showRepoImport('browser')" title="从云端仓库浏览并导入现成任务">
              <IconApp name="globe-grid" class="icon-sm" />
              从仓库导入
            </button>
          </div>
          <div class="help-tip">
            <span>想自动生成步骤？先用 <b>任务录制器</b> 在登录页点选账号框、密码框、登录按钮等元素，再点面板上的「📋 复制 AI 提示词」发给大模型，即可得到任务 JSON。</span>
            <a href="/api/tools/task-recorder.user.js" class="btn btn-sm" title="下载录制器脚本">安装录制器</a>
            <a :href="TUTORIAL_VIDEO_URL" target="_blank" rel="noopener noreferrer" class="btn btn-sm" title="任务录制器使用教程视频">使用教程</a>
            <a href="/api/docs/task-writing-guide" class="btn btn-sm" title="下载编写指南">编写指南</a>
            <router-link :to="{ name: 'tasks-ai' }" class="btn btn-sm" title="用 AI 描述生成任务">用 AI 生成</router-link>
          </div>
          <p class="hint">OCR 相关设置已移至 <router-link :to="{ name: 'settings-tasks' }">设置 · 任务与环境</router-link>。</p>
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
          <p class="hint hint--mt">提示：<code>ocr</code> 步骤需安装 OCR 依赖（ddddocr），未安装时会返回明确错误。</p>
          <h4>如何判定任务真的成功</h4>
          <p class="hint">
            默认判定为「<b>步骤都执行完即成功</b>」，<b>不会校验登录是否被门户接受</b>——
            验证码识别错误、账号密码错误等情况仍会显示「已执行完成（未校验结果）」。
            需要「成功 = 确实登录成功」时，请用 <code>success_condition</code> 指定判定变量，
            并用 <code>eval</code> 步骤把结果页的判据写入该变量：
          </p>
          <pre v-pre class="help-code">{
  "success_condition": "portalOk",
  "steps": [
    { "type": "input", "selector": "#username", "value": "{{USERNAME}}" },
    { "type": "input", "selector": "#password", "value": "{{PASSWORD}}" },
    { "type": "click", "selector": "#login-btn" },
    { "type": "eval", "store_as": "portalOk",
      "script": "document.body.innerText.includes('认证成功')" }
  ]
}</pre>
          <p class="hint">
            变量真值为假时任务判为失败（提示「成功条件未命中」）。含 <code>ocr</code> +
            <code>click</code> 的登录型任务建议都加上，否则界面显示成功但与实际是否上网无关。
          </p>
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
</template>
