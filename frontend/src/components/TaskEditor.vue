<script setup lang="ts">
/**
 * 任务编辑器组件。
 * 可视化编辑任务配置（基本信息 + 步骤列表），
 * 底层同步到 editingTask.json 字符串，保持与现有 useTasks 兼容。
 *
 * 支持两种模式：
 * - visual：表单 + 可视化步骤列表
 * - raw：原始 JSON 编辑（由父组件 TasksView 处理）
 */

import { computed, ref, watch } from "vue";
import type { BrowserTaskDraft } from "@/composables/useTasks";
import { useTasks } from "@/composables/useTasks";
import StepEditor from "./StepEditor.vue";

const t = useTasks();

/** 步骤类型定义（snake_case，与后端一致） */
interface StepConfig {
  type: string;
  selector?: string;
  value?: string;
  select_value?: string;
  url?: string;
  script?: string;
  timeout?: number;
  description?: string;
  [key: string]: unknown;
}

/** 解析后的任务配置结构 */
interface TaskConfig {
  name?: string;
  description?: string;
  url?: string;
  navigation_wait?: number;
  success_condition?: string;
  variables?: Record<string, string>;
  steps?: StepConfig[];
  [key: string]: unknown;
}

/** 当前编辑模式 */
const mode = ref<"visual" | "raw">("visual");

/** 从 editingTask.json 解析出的配置对象（仅 visual 模式使用） */
const config = ref<TaskConfig>({});
const parseError = ref("");

/** 当前编辑的任务草稿 */
const task = computed(() => t.editingTask.value);

/** 步骤列表 */
const steps = computed(() => config.value.steps || []);

/**
 * 从 editingTask.json 同步到 config 对象。
 * 仅在切换到 visual 模式或 editingTask 变化时触发。
 */
function syncFromJson(): void {
  if (!task.value?.json) {
    config.value = {};
    parseError.value = "";
    return;
  }
  try {
    const parsed = JSON.parse(task.value.json) as TaskConfig;
    config.value = parsed;
    parseError.value = "";
  } catch (e) {
    parseError.value = (e as Error).message;
  }
}

/**
 * 将 config 对象同步回 editingTask.json。
 * 每次 visual 模式下编辑后调用。
 */
function syncToJson(): void {
  if (!task.value) return;
  // 同步 name/description/url 到 config
  config.value.name = task.value.name || config.value.name;
  config.value.description = task.value.description || config.value.description;
  config.value.url = task.value.url || config.value.url;
  task.value.json = JSON.stringify(config.value, null, 2);
  t.validateJson();
}

/** 监听 editingTask 变化，自动同步 */
watch(
  () => task.value?.json,
  () => {
    if (mode.value === "visual") syncFromJson();
  },
  { immediate: true },
);

/** 切换编辑模式 */
function switchMode(newMode: "visual" | "raw"): void {
  if (newMode === "visual") {
    syncFromJson();
  } else {
    // 切到 raw 模式前先将 config 同步回 json
    syncToJson();
  }
  mode.value = newMode;
}

/** 添加新步骤 */
function addStep(): void {
  if (!config.value.steps) config.value.steps = [];
  config.value.steps.push({ type: "input", selector: "", value: "", description: "" });
  syncToJson();
}

/** 删除指定步骤 */
function removeStep(index: number): void {
  if (!config.value.steps) return;
  config.value.steps.splice(index, 1);
  syncToJson();
}

/** 上移步骤 */
function moveStepUp(index: number): void {
  if (!config.value.steps || index <= 0) return;
  const arr = config.value.steps;
  [arr[index - 1], arr[index]] = [arr[index], arr[index - 1]];
  syncToJson();
}

/** 下移步骤 */
function moveStepDown(index: number): void {
  if (!config.value.steps || index >= config.value.steps.length - 1) return;
  const arr = config.value.steps;
  [arr[index], arr[index + 1]] = [arr[index + 1], arr[index]];
  syncToJson();
}

/** 更新指定步骤 */
function updateStep(index: number, value: StepConfig): void {
  if (!config.value.steps) return;
  config.value.steps[index] = value;
  syncToJson();
}

/** 更新基础字段并同步 */
function updateBasicField(key: string, value: string): void {
  if (!task.value) return;
  (task.value as Record<string, unknown>)[key] = value;
  (config.value as Record<string, unknown>)[key] = value;
  syncToJson();
}

/** 加载默认模板 */
async function loadDefaultTemplate(): Promise<void> {
  await t.loadTemplate("default");
  syncFromJson();
}

/** 格式化 JSON（raw 模式） */
function formatJson(): void {
  t.formatJson();
}
</script>

<template>
  <div v-if="task" class="task-editor">
    <div class="card-header">
      <h2>{{ task._isNew ? "新建任务" : "编辑任务" }}</h2>
      <div class="editor-mode-switch">
        <button class="btn btn-sm" :class="{ 'btn-primary': mode === 'visual' }" @click="switchMode('visual')">可视化</button>
        <button class="btn btn-sm" :class="{ 'btn-primary': mode === 'raw' }" @click="switchMode('raw')">JSON</button>
      </div>
    </div>

    <div class="card-body">
      <!-- ========== 基本信息 ========== -->
      <div class="form-group">
        <label for="editor-task-id">任务ID</label>
        <input
          id="editor-task-id"
          type="text"
          :value="task.id"
          placeholder="task_id"
          :disabled="!task._isNew"
          @input="task.id = ($event.target as HTMLInputElement).value"
        />
        <span class="hint">必须以字母开头，且只能包含字母、数字和下划线</span>
      </div>

      <div class="form-group">
        <label for="editor-task-name">任务名称</label>
        <input
          id="editor-task-name"
          type="text"
          :value="task.name"
          placeholder="我的登录任务"
          @input="updateBasicField('name', ($event.target as HTMLInputElement).value)"
        />
      </div>

      <div class="form-group">
        <label for="editor-task-desc">描述</label>
        <input
          id="editor-task-desc"
          type="text"
          :value="task.description"
          placeholder="任务描述"
          @input="updateBasicField('description', ($event.target as HTMLInputElement).value)"
        />
      </div>

      <div class="form-group">
        <label for="editor-task-url">认证地址</label>
        <input
          id="editor-task-url"
          type="text"
          :value="task.url"
          placeholder="不填则使用系统设置的认证地址"
          @input="updateBasicField('url', ($event.target as HTMLInputElement).value)"
        />
        <span class="hint">留空默认使用系统认证地址</span>
      </div>

      <div class="form-group">
        <label for="editor-task-timeout">导航等待（秒）</label>
        <input
          id="editor-task-timeout"
          type="number"
          :value="config.navigation_wait ?? 3"
          min="0"
          max="60"
          @input="config.navigation_wait = Number(($event.target as HTMLInputElement).value); syncToJson()"
        />
        <span class="hint">页面加载后的额外等待时间</span>
      </div>

      <div class="form-group">
        <label for="editor-task-success-condition">成功条件变量</label>
        <input
          id="editor-task-success-condition"
          type="text"
          :value="config.success_condition || ''"
          placeholder="留空则自动通过网络检测判断"
          @input="config.success_condition = ($event.target as HTMLInputElement).value; syncToJson()"
        />
        <span class="hint">填写 eval 步骤 store_as 的变量名，登录成功以该变量真值判定（留空则登录后自动网络检测）</span>
      </div>

      <template v-if="mode === 'visual'">
        <!-- ========== 可视化步骤编辑 ========== -->
        <div class="step-list-header">
          <h3>步骤列表</h3>
          <button class="btn btn-sm btn-primary" @click="addStep">
            <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" class="icon-xs">
              <line x1="12" y1="5" x2="12" y2="19" /><line x1="5" y1="12" x2="19" y2="12" />
            </svg>
            添加步骤
          </button>
        </div>

        <div v-if="!steps.length" class="empty-state">
          <span>暂无步骤，点击上方按钮添加</span>
        </div>

        <div v-else class="step-list">
          <StepEditor
            v-for="(step, i) in steps"
            :key="i"
            :model-value="step"
            :index="i"
            @update:model-value="updateStep(i, $event)"
            @remove="removeStep(i)"
            @move-up="moveStepUp(i)"
            @move-down="moveStepDown(i)"
          />
        </div>

        <div v-if="parseError" class="json-error">{{ parseError }}</div>
      </template>

      <template v-else>
        <!-- ========== 原始 JSON 编辑（委托给父组件或直接展示 textarea） ========== -->
        <div class="form-group">
          <label for="editor-raw-json">JSON 配置</label>
          <textarea
            id="editor-raw-json"
            :value="task.json"
            rows="16"
            placeholder="任务JSON配置"
            :class="{ 'json-invalid': t.jsonError.value, 'json-valid': task.json?.trim() && !t.jsonError.value }"
            @input="task.json = ($event.target as HTMLTextAreaElement).value; t.validateJson(); t.syncJsonToMeta()"
          ></textarea>
          <div v-if="t.jsonError.value" class="json-error">{{ t.jsonError.value }}</div>
          <span v-else class="hint">编辑完整的任务配置JSON</span>
        </div>

        <div class="task-editor-actions">
          <button class="btn btn-secondary" @click="loadDefaultTemplate">加载默认模板</button>
          <button class="btn btn-secondary" @click="formatJson">格式化</button>
        </div>
      </template>
    </div>

    <div class="card-footer">
      <button class="btn btn-secondary" @click="t.editingTask.value = null">取消</button>
      <button class="btn btn-primary" :disabled="!!t.jsonError.value" @click="t.saveTask()">保存任务</button>
    </div>
  </div>
</template>
