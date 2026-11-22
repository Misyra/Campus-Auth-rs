<script setup lang="ts">
/**
 * 持久会话（浏览器记住密码）分区：开关 + 配置会话编排
 * （打开浏览器配置 → 浏览器内登录门户 → 完成/放弃 → 重新录制）
 * + 自由浏览入口（打开浏览器，不录制，供已启用持久会话的方案单独开浏览器）。
 *
 * 分工与「登录方式」（LoginChannelField）对齐：
 * - 开关（persistent_session_enabled）随方案草稿走既有保存通道（PUT /api/profiles/{id}）；
 * - 录制动作（recorded_actions）由后端配置会话 stop 落盘，是后端权威数据，本组件
 *   只读展示，保存载荷中不回传（见 useProfiles.saveProfile 的剥离注释）；
 * - 配置会话的 start/stop 是跨进程的后端状态，本组件的横幅只是前端观测——页面
 *   刷新后横幅即丢而后端会话仍在，此时再点「打开浏览器配置」会拿到 409，
 *   「已有配置会话进行中」的固定文案即指向这一场景。
 */
import IconApp from "@/components/common/IconApp.vue";
import FieldHelp from "@/components/common/FieldHelp.vue";
import ToggleSwitch from "@/components/common/ToggleSwitch.vue";
import { computed, ref } from "vue";
import { configSessionApi } from "@/api";
import { ApiError, extractApiError } from "@/api/client";
import { useToast } from "@/composables/useToast";
import type { RecordedAction } from "@/api/types";

/** 本组件读写的最小字段集（宿主草稿类型可含更多字段） */
export interface PersistentSessionDraft {
  /** 是否启用持久会话（随方案保存通道落盘） */
  persistent_session_enabled?: boolean;
  /** 配置会话录制的动作（后端权威，只读展示；保存通道不回传） */
  recorded_actions?: RecordedAction[];
}

const props = defineProps<{
  /** 草稿对象（原地写回 persistent_session_enabled，宿主脏检测基于全量序列化比对） */
  modelValue: PersistentSessionDraft;
  /** 已保存方案 ID；无（新建未保存）时不传——配置会话必须落在已存在方案上 */
  profileId?: string;
}>();

const emit = defineEmits<{ sessionStopped: [] }>();

const { toastOnly } = useToast();

/** 开关说明文案（任务冻结口径，逐字使用） */
const ENABLE_HINT =
  "启用后自动登录将打开持久浏览器上下文：门户记住密码自动回填账号密码，系统回放你录制的登录点击；失败时提示重新配置";

const HELP =
  "「打开浏览器配置」会拉起一个真实浏览器：在里面登录门户并勾选门户的记住密码，" +
  "期间你在登录页上的点击会被录制下来。启用后自动登录不再执行任务步骤，而是复用" +
  "门户记住的密码与你录制的点击。\n\n「打开浏览器」直接拉起同一个持久浏览器供自由" +
  "使用（不录制）：手动登录并勾选记住密码后关闭窗口即可，适合只想补存凭据、不想" +
  "重新录制的场景。\n\n配置会话全局一次只能有一个；浏览器被手动关掉后点「完成配置」" +
  "会提示会话已结束，重新打开即可。";

/** 会话阶段：starting 拉起浏览器中；active 配置浏览器进行中；stopping 正在结束 */
const phase = ref<"idle" | "starting" | "active" | "stopping">("idle");

/** 当前会话模式：record 录制配置会话；free 自由浏览（不录制，无完成配置动作） */
const mode = ref<"record" | "free">("record");

/** 最近一次「完成配置」的录制数（来自 stop 响应；草稿中的动作表不随之改写） */
const lastRecordedCount = ref<number | null>(null);
const lastTruncated = ref(false);

/** 就地错误（start/stop 失败；成功后清空，role=alert 供读屏与测试定位） */
const errorMsg = ref("");

const hasSavedProfile = computed(() => Boolean(props.profileId));

/** 已录制动作数：优先展示本次会话结果，否则回落草稿里的既有录制 */
const recordedCount = computed(() => {
  if (lastRecordedCount.value !== null) return lastRecordedCount.value;
  return props.modelValue.recorded_actions?.length ?? 0;
});

/** 是否已有可用录制（决定按钮文案是「打开浏览器配置」还是「重新录制」） */
const hasRecording = computed(() => recordedCount.value > 0);

/** 开关代理：undefined 视为关闭；写回布尔，草稿字段保持与后端同形状 */
const enabled = computed<boolean>({
  get: () => props.modelValue.persistent_session_enabled === true,
  set: (value: boolean) => {
    props.modelValue.persistent_session_enabled = value;
  },
});

/** start 的 409 用固定文案：占用可能来自另一个页面/另一个方案的配置会话，
 *  也可能来自本页刷新后丢失的横幅，统一的行动指引比逐场景解释更有用 */
function conflictCopy(): string {
  return "已有配置会话进行中";
}

/**
 * 打开会话。record 模式（首次=打开浏览器配置；已有录制=重新录制，后端覆盖旧
 * 录制）409 走既有就地文案；free 模式（自由浏览，record=false，须已启用持久
 * 会话）409 走 toast——占用提示是瞬时行动指引而非表单状态，就地错误区留给
 * 真正的失败信息。
 */
async function startSession(sessionMode: "record" | "free"): Promise<void> {
  if (!hasSavedProfile.value || phase.value !== "idle") return;
  if (sessionMode === "free" && !enabled.value) return;
  phase.value = "starting";
  mode.value = sessionMode;
  errorMsg.value = "";
  try {
    await configSessionApi.start(
      props.profileId as string,
      sessionMode === "free" ? { record: false } : undefined,
    );
    phase.value = "active";
  } catch (error) {
    phase.value = "idle";
    if (error instanceof ApiError && error.status === 409) {
      if (sessionMode === "free") {
        toastOnly(false, "配置会话进行中，请先关闭");
      } else {
        errorMsg.value = conflictCopy();
      }
    } else {
      errorMsg.value = extractApiError(
        error,
        sessionMode === "free" ? "打开浏览器失败" : "打开配置浏览器失败",
      );
    }
  }
}

/** 完成配置：stop 落盘后反映开关与录制数（后端已置位则自动勾选开关） */
async function finishSession(): Promise<void> {
  if (phase.value !== "active") return;
  phase.value = "stopping";
  errorMsg.value = "";
  try {
    const result = await configSessionApi.stop(props.profileId as string);
    // 开关只单向勾选不反向取消：契约恒返回 true；万一返回 false 也不过度反应，
    // 让用户自己决定是否再关掉开关（避免覆盖用户在会话期间的手动改动）
    if (result.persistent_session_enabled) props.modelValue.persistent_session_enabled = true;
    lastRecordedCount.value = result.recorded_count;
    lastTruncated.value = result.truncated;
    phase.value = "idle";
    emit("sessionStopped");
  } catch (error) {
    if (error instanceof ApiError && error.status === 409) {
      // 无活跃会话 = 配置浏览器已经没了（手动关闭/后端超时回收），横幅是陈旧的：
      // 退回 idle 让用户能重新发起，后端 message 直显说明原因
      phase.value = "idle";
      errorMsg.value = extractApiError(error, "配置会话已结束");
    } else {
      // 其他失败（网络抖动、落盘异常）会话可能仍在：保留横幅可重试
      phase.value = "active";
      errorMsg.value = extractApiError(error, "结束配置会话失败");
    }
  }
}

/**
 * 放弃：调 stop（discard=true）只关浏览器——后端不落盘录制、不写方案，「放弃 =
 * 不采纳录制」由前后端共同保证（响应仍带真实 recorded_count，本路径一律丢弃：
 * 不写开关、不更新录制数、不通知宿主）。「完成配置」走缺省 stop（落盘采纳）。
 * `errorFallback` 供自由浏览关闭路径换用贴合其语义的失败文案。
 */
async function abandonSession(errorFallback = "结束配置会话失败"): Promise<void> {
  if (phase.value !== "active" && phase.value !== "stopping") return;
  phase.value = "stopping";
  errorMsg.value = "";
  try {
    await configSessionApi.stop(props.profileId as string, { discard: true });
  } catch (error) {
    // 409（无活跃会话）= 浏览器已经没了，放弃的目的已达成，静默收场；
    // 其余失败如实提示（浏览器可能还开着，用户需自行关闭或重试）
    errorMsg.value =
      error instanceof ApiError && error.status === 409
        ? ""
        : extractApiError(error, errorFallback);
  } finally {
    phase.value = "idle";
  }
}

/**
 * 关闭自由浏览浏览器：与放弃同走 stop（discard=true）——自由浏览会话没有录制
 * 可落盘（后端也拒绝其非 discard 结算），不写方案、不通知宿主；409（用户已
 * 直接关窗，Worker 幽灵收尾）即目的已达成，静默收场。
 */
async function closeFreeBrowser(): Promise<void> {
  if (mode.value !== "free") return;
  await abandonSession("关闭浏览器失败");
}
</script>

<template>
  <div class="editor-section" data-testid="persistent-session">
    <div class="editor-section-label">
      持久会话（浏览器记住密码）
      <FieldHelp :text="HELP" />
    </div>

    <ToggleSwitch
      v-model="enabled"
      label="启用持久会话"
      :description="ENABLE_HINT"
      data-testid="session-toggle"
    />

    <!-- 已录制动作数：优先展示本次会话结果，否则回落草稿里的既有录制 -->
    <p v-if="recordedCount > 0" class="session-count" data-testid="session-count">
      <IconApp name="check-circle" class="icon-sm" />
      已录制 {{ recordedCount }} 个动作
    </p>
    <div v-if="lastTruncated" class="note note--warn session-truncated" data-testid="session-truncated">
      <IconApp name="alert-triangle" class="icon-sm" />
      <span>录制的动作数超出单次上限，仅保留前 {{ recordedCount }} 个动作，超出部分未记录。</span>
    </div>

    <!-- 配置会话动作行：新建方案（未落盘）没有可绑定的会话，只给指引 -->
    <div class="session-actions">
      <button
        v-if="hasSavedProfile"
        type="button"
        class="btn btn-sm"
        data-testid="session-start"
        :disabled="phase !== 'idle'"
        @click="startSession('record')"
      >
        <IconApp name="globe" class="icon-sm" />
        {{ phase === "starting" && mode === "record" ? "正在打开浏览器…" : hasRecording ? "重新录制" : "打开浏览器配置" }}
      </button>
      <!-- 自由浏览入口：已启用持久会话后可单独打开持久浏览器手动登录（不录制）；
           未启用时禁用并提示（后端同样以 400 门禁兜底） -->
      <button
        v-if="hasSavedProfile"
        type="button"
        class="btn btn-sm"
        data-testid="session-open"
        :disabled="!enabled || phase !== 'idle'"
        @click="startSession('free')"
      >
        <IconApp name="globe" class="icon-sm" />
        {{ phase === "starting" && mode === "free" ? "正在打开浏览器…" : "打开浏览器" }}
      </button>
      <span v-if="!hasSavedProfile" class="hint">保存方案后才能打开配置浏览器（会话按方案 ID 落盘）。</span>
      <span v-else-if="!enabled" class="hint" data-testid="session-open-hint">
        启用持久会话并保存方案后才能直接打开浏览器。
      </span>
    </div>

    <!-- 配置会话进行中横幅：录制模式给指引 + 完成/放弃；自由浏览模式给关窗指引 + 关闭按钮 -->
    <div
      v-if="phase === 'active' || phase === 'stopping'"
      class="session-banner"
      data-testid="session-banner"
      role="status"
    >
      <div class="session-banner-copy">
        <template v-if="mode === 'free'">
          <strong>浏览器已打开</strong>
          <span>在浏览器中手动登录门户（勾选门户的记住密码），登录态会保存在本方案的持久目录；用完直接关闭浏览器窗口即可。</span>
        </template>
        <template v-else>
          <strong>配置会话进行中</strong>
          <span>在打开的浏览器中登录门户（勾选门户的记住密码），完成后点击「完成配置」。</span>
        </template>
      </div>
      <div class="session-banner-actions">
        <button
          v-if="mode === 'free'"
          type="button"
          class="btn btn-sm"
          data-testid="session-close"
          :disabled="phase === 'stopping'"
          @click="closeFreeBrowser"
        >
          {{ phase === "stopping" ? "正在关闭…" : "关闭浏览器" }}
        </button>
        <template v-else>
          <button
            type="button"
            class="btn btn-sm"
            data-testid="session-abandon"
            :disabled="phase === 'stopping'"
            @click="abandonSession()"
          >
            放弃
          </button>
          <button
            type="button"
            class="btn btn-sm btn-primary"
            data-testid="session-finish"
            :disabled="phase === 'stopping'"
            @click="finishSession"
          >
            {{ phase === "stopping" ? "正在结束…" : "完成配置" }}
          </button>
        </template>
      </div>
    </div>

    <p v-if="errorMsg" class="session-error" role="alert" data-testid="session-error">{{ errorMsg }}</p>
  </div>
</template>

<style scoped>
/* 分区私有布局（颜色/圆角等走全局 token，不在此复写共享类声明值） */

/* 录制数行：紧跟开关的成功态反馈 */
.session-count {
  display: flex;
  align-items: center;
  gap: 6px;
  margin: var(--space-sm) 0 0;
  color: var(--success);
  font-size: var(--text-sm);
}

.session-truncated {
  margin-top: var(--space-sm);
}

/* 动作行：与开关的间距对齐 */
.session-actions {
  display: flex;
  align-items: center;
  gap: var(--space-md);
  margin-top: var(--space-md);
}

/* 进行中横幅：指引文案与完成/放弃动作同行，窄屏折行 */
.session-banner {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: var(--space-md);
  margin-top: var(--space-md);
  padding: 12px 14px;
  border: 1px solid rgba(var(--accent-rgb), 0.22);
  border-radius: var(--radius-md);
  background: rgba(var(--accent-rgb), 0.06);
}

.session-banner-copy {
  display: flex;
  flex-direction: column;
  gap: 3px;
  min-width: 0;
  color: var(--text-secondary);
  font-size: var(--text-sm);
  line-height: 1.5;
}

.session-banner-copy strong {
  color: var(--text-primary);
  font-size: var(--text-md);
}

.session-banner-actions {
  display: flex;
  align-items: center;
  gap: var(--space-sm);
  flex-shrink: 0;
}

@media (max-width: 640px) {
  .session-banner {
    align-items: stretch;
    flex-direction: column;
  }
}

/* 就地错误：与全局 danger 语义同源，仅作排版 */
.session-error {
  margin: var(--space-sm) 0 0;
  color: var(--danger);
  font-size: var(--text-sm);
}
</style>
