<script setup lang="ts">
/**
 * 「录制新任务」弹窗：选方案 → 浏览器内录制 → 完成取回动作 → 预览步骤 → 命名保存。
 *
 * 会话编排（start/stop/409/放弃）在 useTaskRecording（与方案页
 * PersistentSessionSection 同构），本组件只负责三块界面：
 * - 选方案：下拉列出已保存方案（配置会话按方案 ID 落盘，未保存方案无从归属）；
 * - 录制中：指引横幅 + 完成/放弃（文案冻结口径见 RECORD_GUIDANCE）；
 * - 预览：步骤列表（转换产物，见 recordedTaskConverter，可单步删除）+ 名称/描述 + 保存。
 */
import Modal from "@/components/common/Modal.vue";
import CustomSelect from "@/components/common/CustomSelect.vue";
import IconApp from "@/components/common/IconApp.vue";
import { computed, ref, watch } from "vue";
import { useTaskRecording } from "./useTaskRecording";
import { useProfiles } from "@/composables/useProfiles";
import type { SelectOption } from "@/components/common/CustomSelect.vue";
import type { ProfileSummary } from "@/api/types";

const {
  open,
  phase,
  profileId,
  errorMsg,
  steps,
  skippedUnknown,
  truncated,
  requestClose,
  startRecording,
  abandonRecording,
  finishRecording,
  removeStep,
  saveRecordedTask,
} = useTaskRecording();

const { profiles } = useProfiles();

/** 录制指引（冻结口径）：{{USERNAME}} 等字面占位符必须经 JS 字符串进模板——
 *  直接写在 template 里会被 Vue 当成插值表达式求值 */
const SELECT_HINT =
  "选择一个方案，系统会打开浏览器并注入录制器：你在页面里的输入、选择与点击都会被记录，" +
  "完成后自动转换为标准浏览器任务（可手动运行、定时调度、绑定方案）。";
const RECORD_GUIDANCE =
  "在打开的浏览器里完整操作登录流程：输入账号密码、选择运营商、点击登录——每一步都会被记录。" +
  "完成后回到这里点「完成录制」。账号与密码会以 {{USERNAME}} / {{PASSWORD}} 占位符写入任务，" +
  "运行时自动注入真实凭据。";
const EMPTY_PROFILE_HINT = "尚无已保存的方案：请先到「方案」页创建并保存一个方案，再回来录制任务。";

/** 步骤类型徽标文案（转换器映射的中文名） */
const STEP_TYPE_LABELS: Record<string, string> = {
  click: "点击",
  input: "输入",
  select: "选择",
  navigate: "跳转",
};

function stepTypeLabel(type: string): string {
  return STEP_TYPE_LABELS[type] ?? type;
}

/** 方案下拉选项：id 为值、显示名（缺省回落 id）为标签，顺序与方案列表一致 */
const profileOptions = computed<SelectOption[]>(() =>
  (Object.values(profiles.value) as ProfileSummary[]).map((p) => ({
    value: p.id,
    label: String(p.name || "").trim() || p.id,
  })),
);

const hasProfiles = computed(() => profileOptions.value.length > 0);
const selectedProfileName = computed(
  () => profileOptions.value.find((o) => o.value === profileId.value)?.label ?? "",
);

// ===== 预览态的名称/描述（进入预览时按录制结果生成默认值，可改） =====
const taskName = ref("");
const taskDescription = ref("");

/** 默认任务名「录制任务 MM-DD HH:mm」（本机时间；格式化口径与 formatters 一致，不走 locale） */
function defaultRecordedName(): string {
  const d = new Date();
  const p = (n: number): string => String(n).padStart(2, "0");
  return `录制任务 ${p(d.getMonth() + 1)}-${p(d.getDate())} ${p(d.getHours())}:${p(d.getMinutes())}`;
}

watch(phase, (p) => {
  if (p !== "preview") return;
  taskName.value = defaultRecordedName();
  taskDescription.value =
    `由「录制新任务」从方案「${selectedProfileName.value || profileId.value}」的浏览器操作生成` +
    `（${steps.value.length} 个步骤）；凭据以占位符在运行时注入。`;
});

/** 保存任务（PUT /api/tasks/{id}，走既有创建管线；失败停在预览态可重试） */
async function onSave(): Promise<void> {
  await saveRecordedTask(taskName.value, taskDescription.value);
}
</script>

<template>
  <Modal
    :open="open"
    title="录制新任务"
    size="lg"
    :close-disabled="phase === 'finishing' || phase === 'saving'"
    @close="requestClose"
  >
    <!-- ==================== 选方案 ==================== -->
    <div v-if="phase === 'select' || phase === 'starting'" class="rec-stage">
      <p class="rec-hint">{{ SELECT_HINT }}</p>
      <div class="form-group">
        <label for="rec-profile-trigger">方案</label>
        <CustomSelect
          v-model="profileId"
          trigger-id="rec-profile-trigger"
          :options="profileOptions"
          :disabled="phase !== 'select'"
          aria-label="选择用于录制的方案"
          placeholder="选择用于录制的方案"
          data-testid="record-profile-select"
        />
        <span v-if="!hasProfiles" class="hint">{{ EMPTY_PROFILE_HINT }}</span>
      </div>
      <p v-if="errorMsg" class="rec-error" role="alert" data-testid="record-error">{{ errorMsg }}</p>
    </div>

    <!-- ==================== 录制中 ==================== -->
    <div v-else-if="phase === 'recording' || phase === 'finishing'" class="rec-stage">
      <div class="rec-banner" role="status" data-testid="record-banner">
        <strong>录制进行中{{ selectedProfileName ? `：方案「${selectedProfileName}」` : "" }}</strong>
        <span>{{ RECORD_GUIDANCE }}</span>
      </div>
      <p v-if="errorMsg" class="rec-error" role="alert" data-testid="record-error">{{ errorMsg }}</p>
    </div>

    <!-- ==================== 预览与保存 ==================== -->
    <div v-else class="rec-stage">
      <div v-if="truncated" class="note note--warn" data-testid="record-truncated">
        <IconApp name="alert-triangle" class="icon-sm" />
        <span>录制的动作数超出单次上限，仅保留前 {{ steps.length }} 个动作，超出部分未记录。</span>
      </div>
      <div v-if="skippedUnknown > 0" class="note note--warn" data-testid="record-skipped">
        <IconApp name="alert-triangle" class="icon-sm" />
        <span>{{ skippedUnknown }} 个动作不属于可转换的类型，已跳过；其余动作已正常转换。</span>
      </div>

      <p class="rec-hint">以下步骤由录制动作转换而来，误录的步骤可在此删除，保存后也可在编辑器中继续调整：</p>
      <ol class="rec-steps" data-testid="record-steps">
        <li v-for="(step, i) in steps" :key="step.id" class="rec-step">
          <span class="rec-step-index" aria-hidden="true">{{ i + 1 }}</span>
          <span class="badge badge--sm badge--info">{{ stepTypeLabel(step.type) }}</span>
          <code v-if="step.selector" class="rec-step-code" :title="step.selector">{{ step.selector }}</code>
          <code v-if="step.value" class="rec-step-code rec-step-value" :title="step.value">{{ step.value }}</code>
          <button
            type="button"
            class="rec-step-remove"
            :aria-label="`删除步骤 ${i + 1}`"
            data-testid="record-step-remove"
            @click="removeStep(i)"
          >
            <IconApp name="trash" class="icon-sm" />
          </button>
        </li>
      </ol>

      <div class="form-group">
        <label for="record-name">任务名称</label>
        <input
          id="record-name"
          v-model="taskName"
          type="text"
          data-testid="record-name"
          placeholder="录制任务"
        />
      </div>
      <div class="form-group">
        <label for="record-desc">描述</label>
        <input
          id="record-desc"
          v-model="taskDescription"
          type="text"
          data-testid="record-desc"
          placeholder="任务描述（可选）"
        />
      </div>
      <p v-if="errorMsg" class="rec-error" role="alert" data-testid="record-error">{{ errorMsg }}</p>
    </div>

    <template #footer>
      <!-- 选方案：开始录制（须已选方案；starting 中禁用防连点） -->
      <template v-if="phase === 'select' || phase === 'starting'">
        <span class="hint">{{ phase === "starting" ? "正在打开浏览器…" : "录制会话全局一次只能有一个。" }}</span>
        <span class="rec-footer-spacer"></span>
        <button
          type="button"
          class="btn btn-sm btn-primary"
          data-testid="record-start"
          :disabled="!profileId || phase !== 'select'"
          @click="startRecording"
        >
          <IconApp name="target" class="icon-sm" />
          {{ phase === "starting" ? "正在打开浏览器…" : "打开浏览器开始录制" }}
        </button>
      </template>
      <!-- 录制中：放弃（discard，不落盘）与完成（取回动作进预览） -->
      <template v-else-if="phase === 'recording' || phase === 'finishing'">
        <button
          type="button"
          class="btn btn-sm"
          data-testid="record-abandon"
          :disabled="phase === 'finishing'"
          @click="abandonRecording"
        >
          放弃录制
        </button>
        <button
          type="button"
          class="btn btn-sm btn-primary"
          data-testid="record-finish"
          :disabled="phase === 'finishing'"
          @click="finishRecording"
        >
          {{ phase === "finishing" ? "正在结束…" : "完成录制" }}
        </button>
      </template>
      <!-- 预览：不保存（会话已结束，直接关）与保存 -->
      <template v-else>
        <button
          type="button"
          class="btn btn-sm"
          data-testid="record-cancel"
          :disabled="phase === 'saving'"
          @click="requestClose"
        >
          不保存
        </button>
        <button
          type="button"
          class="btn btn-sm btn-primary"
          data-testid="record-save"
          :disabled="phase === 'saving' || !steps.length"
          @click="onSave"
        >
          <IconApp name="save" class="icon-sm" />
          {{ phase === "saving" ? "正在保存…" : "保存为任务" }}
        </button>
      </template>
    </template>
  </Modal>
</template>

<style scoped>
/* 弹窗私有排版；颜色/圆角/字号走全局 token，不复写共享类声明值 */

.rec-stage {
  display: flex;
  flex-direction: column;
  gap: var(--space-md);
}

.rec-hint {
  margin: 0;
  color: var(--text-secondary);
  font-size: var(--text-sm);
  line-height: 1.6;
}

/* 录制中横幅：与方案页配置会话横幅同观感（accent 底色 + 强调标题） */
.rec-banner {
  display: flex;
  flex-direction: column;
  gap: 4px;
  padding: 12px 14px;
  border: 1px solid rgba(var(--accent-rgb), 0.22);
  border-radius: var(--radius-md);
  background: rgba(var(--accent-rgb), 0.06);
  color: var(--text-secondary);
  font-size: var(--text-sm);
  line-height: 1.6;
}

.rec-banner strong {
  color: var(--text-primary);
  font-size: var(--text-md);
}

/* 步骤预览：有序列表语义供读屏播报序号；序号圈仅为视觉强化 */
.rec-steps {
  margin: 0;
  padding: 0;
  list-style: none;
  display: flex;
  flex-direction: column;
  gap: var(--space-sm);
  max-height: 280px;
  overflow-y: auto;
}

.rec-step {
  display: flex;
  align-items: center;
  gap: var(--space-sm);
  min-width: 0;
}

.rec-step-index {
  flex-shrink: 0;
  display: inline-flex;
  align-items: center;
  justify-content: center;
  width: 22px;
  height: 22px;
  border-radius: var(--radius-full);
  background: var(--bg-glass);
  border: 1px solid var(--border);
  color: var(--text-secondary);
  font-size: var(--text-xs);
}

.rec-step-code {
  min-width: 0;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  color: var(--text-secondary);
  font-family: var(--font-mono);
  font-size: var(--text-sm);
}

/* 填写值排在选择器之后并允许换行展示（占位符 {{USERNAME}} 要原样可见） */
.rec-step-value {
  flex-shrink: 0;
  color: var(--text-primary);
}

/* 单步删除：静默图标按钮，删除后列表与保存载荷同步收缩；删空由保存按钮 disabled 兜底 */
.rec-step-remove {
  flex-shrink: 0;
  margin-left: auto;
  display: inline-flex;
  align-items: center;
  justify-content: center;
  width: 26px;
  height: 26px;
  border-radius: var(--radius-sm);
  border: none;
  background: transparent;
  color: var(--text-tertiary);
  cursor: pointer;
  transition: color var(--dur-fast) var(--ease-out), background var(--dur-fast) var(--ease-out);
}

.rec-step-remove:hover {
  color: var(--danger);
  background: var(--danger-bg);
}

/* 就地错误：danger 语义色，role=alert 供读屏播报 */
.rec-error {
  margin: 0;
  color: var(--danger);
  font-size: var(--text-sm);
}

/* footer 左侧说明文字与右侧按钮组之间撑开 */
.rec-footer-spacer {
  flex: 1;
}

@media (max-width: 640px) {
  .rec-step {
    flex-wrap: wrap;
  }
}
</style>
