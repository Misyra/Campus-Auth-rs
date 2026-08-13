<script setup lang="ts">
/**
 * 单个步骤编辑器。
 * 根据步骤类型（type）动态显示对应配置字段。
 * 所有字段名使用 snake_case，与后端 JSON 一致。
 */

import { computed } from "vue";

/** 步骤类型定义 */
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

const props = defineProps<{
  /** 步骤数据对象（双向绑定） */
  modelValue: StepConfig;
  /** 步骤序号（从 0 开始） */
  index: number;
}>();

const emit = defineEmits<{
  "update:modelValue": [value: StepConfig];
  remove: [];
  moveUp: [];
  moveDown: [];
}>();

/** 可选的步骤类型列表 */
const STEP_TYPES = [
  { value: "input", label: "输入文本" },
  { value: "click", label: "点击元素" },
  { value: "select", label: "选择下拉框" },
  { value: "wait", label: "等待元素" },
  { value: "screenshot", label: "截图" },
  { value: "evaluate", label: "执行 JS" },
  { value: "navigate", label: "跳转 URL" },
  { value: "goto", label: "跳转 URL (goto)" },
  { value: "wait_for_selector", label: "等待选择器" },
  { value: "upload_file", label: "上传文件" },
  { value: "click_select", label: "点击选择" },
  { value: "wait_url", label: "等待 URL" },
  { value: "sleep", label: "等待时间" },
  { value: "ocr", label: "验证码识别" },
  { value: "assert_text", label: "断言文本" },
] as const;

/** 当前步骤类型的值 */
const stepType = computed(() => props.modelValue.type);

/** 是否需要 selector 字段 */
const needsSelector = computed(() =>
  ["input", "click", "select", "wait", "wait_for_selector", "upload_file", "click_select"].includes(stepType.value),
);

/** 是否需要 value 字段（输入值 / 断言文本） */
const needsValue = computed(() => ["input", "assert_text"].includes(stepType.value));

/** 是否需要 select_value 字段 */
const needsSelectValue = computed(() => stepType.value === "select");

/** 是否需要 url 字段 */
const needsUrl = computed(() => ["navigate", "goto"].includes(stepType.value));

/** 是否需要 script 字段 */
const needsScript = computed(() => stepType.value === "evaluate");

/** 是否需要 timeout 字段 */
const needsTimeout = computed(() => ["wait", "wait_for_selector", "wait_url"].includes(stepType.value));

/** 是否需要 sleep 时长字段 */
const needsSleepDuration = computed(() => stepType.value === "sleep");

/** 更新某个字段值 */
function updateField(key: string, value: unknown): void {
  emit("update:modelValue", { ...props.modelValue, [key]: value });
}
</script>

<template>
  <div class="step-editor">
    <div class="step-editor-header">
      <span class="step-index-badge">{{ index + 1 }}</span>
      <select
        class="step-type-select"
        :value="modelValue.type"
        @change="updateField('type', ($event.target as HTMLSelectElement).value)"
      >
        <option v-for="t in STEP_TYPES" :key="t.value" :value="t.value">
          {{ t.label }}
        </option>
      </select>
      <div class="step-editor-actions">
        <button class="btn btn-icon-only btn-sm" title="上移" @click="$emit('moveUp')">
          <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" class="icon-xs">
            <polyline points="18 15 12 9 6 15" />
          </svg>
        </button>
        <button class="btn btn-icon-only btn-sm" title="下移" @click="$emit('moveDown')">
          <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" class="icon-xs">
            <polyline points="6 9 12 15 18 9" />
          </svg>
        </button>
        <button class="btn btn-icon-only btn-sm btn-danger" title="删除步骤" @click="$emit('remove')">
          <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" class="icon-xs">
            <line x1="18" y1="6" x2="6" y2="18" /><line x1="6" y1="6" x2="18" y2="18" />
          </svg>
        </button>
      </div>
    </div>
    <div class="step-editor-body">
      <!-- 描述 -->
      <div class="step-field">
        <label>描述</label>
        <input
          type="text"
          :value="modelValue.description || ''"
          placeholder="步骤说明（可选）"
          @input="updateField('description', ($event.target as HTMLInputElement).value)"
        />
      </div>

      <!-- selector -->
      <div v-if="needsSelector" class="step-field">
        <label>CSS 选择器</label>
        <input
          type="text"
          :value="modelValue.selector || ''"
          placeholder="#username, .submit-btn"
          @input="updateField('selector', ($event.target as HTMLInputElement).value)"
        />
      </div>

      <!-- value（输入值） -->
      <div v-if="needsValue" class="step-field">
        <label>输入值</label>
        <input
          type="text"
          :value="modelValue.value || ''"
          placeholder="{{USERNAME}}"
          @input="updateField('value', ($event.target as HTMLInputElement).value)"
        />
      </div>

      <!-- select_value -->
      <div v-if="needsSelectValue" class="step-field">
        <label>选择值</label>
        <input
          type="text"
          :value="modelValue.select_value || ''"
          placeholder="选项值"
          @input="updateField('select_value', ($event.target as HTMLInputElement).value)"
        />
      </div>

      <!-- url -->
      <div v-if="needsUrl" class="step-field">
        <label>URL</label>
        <input
          type="text"
          :value="modelValue.url || ''"
          placeholder="https://..."
          @input="updateField('url', ($event.target as HTMLInputElement).value)"
        />
      </div>

      <!-- script -->
      <div v-if="needsScript" class="step-field">
        <label>JS 脚本</label>
        <textarea
          :value="modelValue.script || ''"
          rows="4"
          placeholder="document.querySelector('#btn').click();"
          @input="updateField('script', ($event.target as HTMLTextAreaElement).value)"
        ></textarea>
      </div>

      <!-- timeout -->
      <div v-if="needsTimeout" class="step-field step-field-sm">
        <label>超时（秒）</label>
        <input
          type="number"
          :value="modelValue.timeout ?? 10"
          min="1"
          max="300"
          @input="updateField('timeout', Number(($event.target as HTMLInputElement).value))"
        />
      </div>

      <!-- sleep 等待时长（duration 字段，与后端 handle_wait 保持一致） -->
      <div v-if="needsSleepDuration" class="step-field step-field-sm">
        <label>等待（秒）</label>
        <input
          type="number"
          :value="modelValue.duration ?? 3"
          min="1"
          max="60"
          @input="updateField('duration', Number(($event.target as HTMLInputElement).value))"
        />
      </div>
    </div>
  </div>
</template>
