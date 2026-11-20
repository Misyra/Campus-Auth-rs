<script setup lang="ts">
/**
 * schema 标准行渲染器：按 SettingRowDef 渲染「label + 右侧控件」的设置行。
 *
 * 控件经绑定路径（PATCH /api/config 载荷点分路径）直连 useConfig 草稿；开关统一
 * 产出 role="switch" + aria-checked（显式绑定状态值，读屏可读），下拉经 CustomSelect
 * 并把 ariaLabel 绑到触发器——settingsA11y.test.ts 按渲染结果断言这些语义。
 * 行根元素继承外部传入的 data-settings-row（搜索命中高亮的挂点）。
 */
import { computed } from "vue";
import SettingsRow from "@/components/common/SettingsRow.vue";
import CustomSelect from "@/components/common/CustomSelect.vue";
import type { SettingRowDef } from "./schema";
import { useConfig } from "@/composables/useConfig";

const props = defineProps<{ row: SettingRowDef }>();

const config = useConfig();

/** 点分路径取值（reactive 草稿上的属性访问） */
function getPath(path: string): unknown {
  let node: unknown = config.config;
  for (const key of path.split(".")) {
    if (node == null) return undefined;
    node = (node as Record<string, unknown>)[key];
  }
  return node;
}

/** 点分路径写值（同上；schema 绑定均为叶子字段） */
function setPath(path: string, value: unknown): void {
  const keys = path.split(".");
  const leaf = keys.pop();
  if (!leaf) return;
  let node: unknown = config.config;
  for (const key of keys) {
    node = (node as Record<string, unknown>)[key];
  }
  (node as Record<string, unknown>)[leaf] = value;
}

const control = computed(() => props.row.control);
const bindValue = computed(() => getPath(control.value.binding));

/** 开关行的状态（绑定点均为布尔字段；Boolean 兜底防止草稿缺字段时 aria 落空串） */
const bindBool = computed(() => bindValue.value === true);

/** 输入行的 input id（开关/下拉/分段没有 inputId，label 走纯文本） */
const inputId = computed(() => ("inputId" in control.value ? control.value.inputId : undefined));

/** select 的禁用联动：disabledBy 路径为假值时禁用 */
const selectDisabled = computed(() => {
  const c = control.value;
  return c.kind === "select" && c.disabledBy ? !getPath(c.disabledBy) : false;
});

/** 下拉回显：数值字段以字符串比对（CustomSelect 的 value 域是 string） */
const selectValue = computed(() => String(bindValue.value ?? ""));

/** 输入框回显统一转字符串（绑定点可能暂时缺省，避免 unknown 直接落属性） */
const inputValue = computed(() => String(bindValue.value ?? ""));

function setSwitch(event: Event): void {
  setPath(control.value.binding, (event.target as HTMLInputElement).checked);
}

/** 复刻 v-model.number 语义：parseFloat 失败时回写原始串（清空输入不误落 0） */
function setNumber(event: Event): void {
  const raw = (event.target as HTMLInputElement).value;
  const parsed = Number.parseFloat(raw);
  setPath(control.value.binding, Number.isNaN(parsed) ? raw : parsed);
}

/** 复刻 v-model.trim 语义 */
function setText(event: Event): void {
  setPath(control.value.binding, (event.target as HTMLInputElement).value.trim());
}

function setSelect(value: string): void {
  const numeric = control.value.kind === "select" && control.value.numeric;
  setPath(control.value.binding, numeric ? Number(value) : value);
}

function setSegmented(value: string): void {
  setPath(control.value.binding, value);
}
</script>

<template>
  <!-- 说明文字减法：hint / help 不再常驻渲染，统一由 SettingsRow 收进标题行的 ? 气泡
       （help 在前、hint 在后拼接）；两者都缺省的行不渲染 ?。hint 数据仍留在 schema
       供设置搜索匹配（settingsSearch 纯数据语料），此处只管渲染。 -->
  <SettingsRow :description="row.hint" :help="row.help">
    <template #label>
      <!-- 开关行：label 不指向控件（toggle 由 sr-only + aria-checked 自述）；输入行：label for 指向 input -->
      <label v-if="inputId" :for="inputId">{{ row.label }}</label>
      <template v-else>{{ row.label }}</template>
      <span v-if="row.badge" class="badge badge--sm" :class="row.badge.variant === 'info' ? 'badge--info' : undefined">{{ row.badge.text }}</span>
    </template>

    <label v-if="control.kind === 'switch'" class="toggle setting-row-switch">
      <input
        type="checkbox"
        role="switch"
        :aria-checked="bindBool"
        :checked="bindBool"
        @change="setSwitch"
      />
      <span class="toggle-slider"></span>
      <span class="sr-only">{{ row.label }}</span>
    </label>

    <input
      v-else-if="control.kind === 'number'"
      :id="inputId"
      type="number"
      :min="control.min"
      :max="control.max"
      :value="inputValue"
      @input="setNumber"
    />

    <input
      v-else-if="control.kind === 'text'"
      :id="inputId"
      type="text"
      :placeholder="control.placeholder"
      spellcheck="false"
      :value="inputValue"
      @input="setText"
    />

    <CustomSelect
      v-else-if="control.kind === 'select'"
      :model-value="selectValue"
      :options="[...control.options]"
      :aria-label="control.ariaLabel"
      :disabled="selectDisabled"
      @update:model-value="setSelect"
    />

    <div v-else-if="control.kind === 'segmented'" class="segmented" role="group" :aria-label="control.ariaLabel">
      <button
        v-for="opt in control.options"
        :key="opt.value"
        type="button"
        :class="{ active: bindValue === opt.value }"
        @click="setSegmented(opt.value)"
      >
        {{ opt.label }}
      </button>
    </div>
  </SettingsRow>
</template>
