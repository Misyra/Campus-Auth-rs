<script setup lang="ts">
/**
 * 环境初始化弹窗：阶段步骤条 + 实时进度 + 下载源（镜像）选择行 + Worker 日志尾部。
 *
 * 数据通路：初始化 POST /api/environment/bootstrap 由后端同步执行（可达数分钟），
 * 弹窗期间以 1.2s 轮询 GET /api/init-status 的 environment 快照流动 stage/progress，
 * 同时同步全局 useEnvironment 的 envStatus（设置页环境卡状态随弹窗实时刷新）。
 * 日志复用 useLogs 单例（WS 推流），按 source=worker 过滤尾部展示。
 *
 * 下载源选择：打开时调 POST /api/environment/mirror-probe 实测直连与各镜像吞吐
 * （≤5s 并发窗口），结果以一排按钮呈现；点击即 PUT /api/environment/mirror-preference
 * 保存偏好，随后的 uv / Python 解释器下载按所选源执行（显式选源跳过自动测速）。
 */
import { computed, nextTick, onBeforeUnmount, ref, watch } from "vue";
import Modal from "./Modal.vue";
import IconApp from "./IconApp.vue";
import { environmentApi } from "@/api";
import type { EnvironmentStatus, MirrorChoice, MirrorProbeEntry } from "@/api/types";
import { extractApiError } from "@/api/client";
import { useEnvironment } from "@/composables/useEnvironment";
import { useLogs } from "@/composables/useLogs";
import { useStatus } from "@/composables/useStatus";
import { useToast } from "@/composables/useToast";

const props = withDefaults(
  defineProps<{
    open: boolean;
    /** 打开即触发初始化（设置页入口）；已在跑则仅跟随进度不重复触发 */
    autoStart?: boolean;
  }>(),
  { autoStart: false },
);

const emit = defineEmits<{ close: [] }>();

const { busy } = useStatus();
const { envStatus, bootstrapEnv } = useEnvironment();
const { logs, initialized: logsInitialized, fetchLogs } = useLogs();
const { toastOnly } = useToast();

/** 弹窗本地轮询快照：初始为空，打开后首轮拉取（envStatus 可能还是上次遗留） */
const status = ref<EnvironmentStatus | null>(null);
const pollError = ref(false);
let pollTimer: ReturnType<typeof setInterval> | undefined;

/** 阶段步骤定义（key 对齐后端 BootstrapStage 的 Debug 命名） */
const STAGES: { key: string; label: string }[] = [
  { key: "Checking", label: "检查环境" },
  { key: "DownloadingUv", label: "下载 uv" },
  { key: "SyncingVenv", label: "同步 Python 依赖" },
  { key: "VerifyingWorker", label: "验证 Worker" },
  { key: "ApplyingOcr", label: "对齐 OCR" },
  { key: "InstallingPlaywright", label: "安装浏览器" },
];

const stageIndex = computed(() => STAGES.findIndex((st) => st.key === status.value?.stage));
const stageFailed = computed(() => status.value?.stage === "Error");
const allDone = computed(() => Boolean(status.value?.capability_ready));

type StepState = "done" | "current" | "pending" | "failed";
function stepState(index: number): StepState {
  if (allDone.value) return "done";
  if (stageFailed.value) return index === stageFailedIndex.value ? "failed" : index < stageFailedIndex.value ? "done" : "pending";
  const current = stageIndex.value;
  if (current < 0) return "pending";
  return index < current ? "done" : index === current ? "current" : "pending";
}

/** 失败时把 Error 落到最后执行中的阶段上展示（后端 stage=Error 不携带阶段） */
const stageFailedIndex = computed(() => Math.max(stageIndex.value, 0));

const progressPercent = computed(() => {
  if (allDone.value) return 100;
  return status.value?.progress?.percent ?? null;
});
const progressText = computed(() => {
  if (allDone.value) return "环境已就绪";
  if (stageFailed.value) return "初始化失败";
  const p = status.value?.progress;
  if (!p) return busy.env ? "正在启动…" : "尚未开始";
  return `${p.message}${p.percent != null ? ` ${p.percent}%` : ""}`;
});

// ── 下载源选择 ──
const probeLoading = ref(false);
const probeError = ref("");
const probeResults = ref<MirrorProbeEntry[]>([]);
const selectedChoice = ref<MirrorChoice>({ choice: "auto" });
const savingChoice = ref(false);

interface MirrorOption {
  key: string;
  name: string;
  /** 辅助文案：auto 说明语义，源选项展示实测吞吐/失败 */
  state: string;
  ok: boolean;
  choice: MirrorChoice;
}

function choiceKey(c: MirrorChoice): string {
  return c.choice === "mirror" ? `mirror:${c.mirror}` : c.choice;
}

function formatThroughput(bytesPerSec: number): string {
  if (bytesPerSec >= 1024 * 1024) return `${(bytesPerSec / 1024 / 1024).toFixed(1)} MB/s`;
  if (bytesPerSec >= 1024) return `${Math.round(bytesPerSec / 1024)} KB/s`;
  return `${bytesPerSec} B/s`;
}

/** 一排选项：自动测速 + 直连 + 各镜像（顺序与后端镜像名单一致） */
const mirrorOptions = computed<MirrorOption[]>(() => [
  { key: "auto", name: "自动测速", state: "每次下载前实测，自动选最快", ok: true, choice: { choice: "auto" } },
  ...probeResults.value.map((r) => ({
    key: r.kind === "direct" ? "direct" : `mirror:${r.prefix}`,
    name: r.kind === "direct" ? "GitHub 直连" : r.label,
    state: r.ok ? formatThroughput(r.throughput) : "不可用",
    ok: r.ok,
    choice: (r.kind === "direct" ? { choice: "direct" } : { choice: "mirror", mirror: r.prefix }) as MirrorChoice,
  })),
]);

async function loadProbe(): Promise<void> {
  if (probeLoading.value) return;
  probeLoading.value = true;
  probeError.value = "";
  try {
    const res = await environmentApi.probeMirrors();
    probeResults.value = res.results ?? [];
    if (res.selected) selectedChoice.value = res.selected;
  } catch (e) {
    probeError.value = extractApiError(e, "测速失败，请稍后重试");
  } finally {
    probeLoading.value = false;
  }
}

async function chooseMirror(option: MirrorOption): Promise<void> {
  if (savingChoice.value || choiceKey(selectedChoice.value) === option.key) return;
  savingChoice.value = true;
  try {
    const res = await environmentApi.setMirrorPreference(option.choice);
    selectedChoice.value = res.selected ?? option.choice;
    toastOnly(true, `下载源已保存：${option.name}`);
  } catch (e) {
    toastOnly(false, extractApiError(e, "保存下载源失败"));
  } finally {
    savingChoice.value = false;
  }
}

// ── 日志尾部 ──
/** 仅 worker 来源（后端 normalize_source：environment/bridge/python_worker → worker） */
const envLogs = computed(() => logs.filter((l) => l.source === "worker").slice(-150));
const logBox = ref<HTMLElement | null>(null);
watch(
  () => envLogs.value.length,
  () => {
    void nextTick(() => {
      const el = logBox.value;
      if (el) el.scrollTop = el.scrollHeight;
    });
  },
);

function shortTime(ts: string): string {
  return (ts || "").length >= 19 ? ts.substring(11, 19) : ts;
}

// ── 轮询与触发 ──
async function pollOnce(): Promise<void> {
  try {
    const s = await environmentApi.fetchStatus();
    if (s) {
      status.value = s;
      // 同步全局快照：弹窗开着时，设置页环境卡徽标同步流动
      envStatus.value = s;
    }
    pollError.value = false;
  } catch {
    pollError.value = true;
  }
}

function startPoll(): void {
  if (pollTimer) return;
  void pollOnce();
  pollTimer = setInterval(() => { void pollOnce(); }, 1200);
}

function stopPoll(): void {
  if (pollTimer) {
    clearInterval(pollTimer);
    pollTimer = undefined;
  }
}

watch(
  () => props.open,
  (open) => {
    if (open) {
      status.value = envStatus.value;
      startPoll();
      if (!logsInitialized.value) void fetchLogs(true);
      void loadProbe();
      if (props.autoStart && !busy.env && !allDone.value) void bootstrapEnv();
    } else {
      stopPoll();
    }
  },
  // immediate：宿主可能以 open=true 挂载（如路由直开），同样要启动轮询与测速
  { immediate: true },
);

onBeforeUnmount(stopPoll);
</script>

<template>
  <Modal :open="open" title="环境初始化" size="lg" @close="emit('close')">
    <div class="env-dlg">
      <!-- 阶段步骤条 -->
      <ol class="env-steps" aria-label="初始化阶段">
        <li
          v-for="(st, i) in STAGES"
          :key="st.key"
          class="env-step"
          :class="`is-${stepState(i)}`"
        >
          <span class="env-step-dot">
            <IconApp v-if="stepState(i) === 'done'" name="check-circle" class="icon-sm" />
            <IconApp v-else-if="stepState(i) === 'failed'" name="x-circle" class="icon-sm" />
            <IconApp v-else-if="stepState(i) === 'current'" name="refresh" class="icon-sm spin" />
            <span v-else class="env-step-pending-dot"></span>
          </span>
          <span class="env-step-label">{{ st.label }}</span>
        </li>
      </ol>

      <!-- 进度行（role=status 供读屏跟进，不抢焦点） -->
      <div class="env-progress-line" role="status" aria-live="polite">
        <progress
          class="env-progress"
          :class="{ 'is-indeterminate': progressPercent == null && !allDone }"
          :value="progressPercent ?? undefined"
          max="100"
        ></progress>
        <span class="env-progress-text">{{ progressText }}</span>
        <span v-if="pollError" class="env-poll-error">状态获取失败，重试中…</span>
      </div>
      <p v-if="status?.last_error" class="env-dlg-error env-preline">{{ status.last_error }}</p>

      <!-- 下载源选择行 -->
      <section class="env-dlg-section">
        <div class="env-dlg-section-head">
          <h4>下载源</h4>
          <button
            type="button"
            class="btn btn-ghost btn-sm"
            :disabled="probeLoading"
            @click="loadProbe"
          >
            <IconApp name="refresh" class="icon-sm" :class="{ spin: probeLoading }" />
            {{ probeLoading ? "测速中…" : "重新测速" }}
          </button>
        </div>
        <p class="hint">
          实测当前网络到各源的下载速度；选定的源对 uv、Python 解释器下载生效，Auto 每次下载前自动选最快。
        </p>
        <p v-if="probeError" class="env-dlg-error">{{ probeError }}</p>
        <div class="mirror-row" role="radiogroup" aria-label="下载源选择">
          <button
            v-for="opt in mirrorOptions"
            :key="opt.key"
            type="button"
            class="mirror-opt"
            :class="{ selected: choiceKey(selectedChoice) === opt.key, saving: savingChoice }"
            :disabled="savingChoice"
            role="radio"
            :aria-checked="choiceKey(selectedChoice) === opt.key"
            @click="chooseMirror(opt)"
          >
            <span class="mirror-opt-name">{{ opt.name }}</span>
            <span class="mirror-opt-state" :class="{ 'is-dead': !opt.ok }">{{ opt.state }}</span>
          </button>
        </div>
      </section>

      <!-- 日志尾部 -->
      <section class="env-dlg-section">
        <div class="env-dlg-section-head">
          <h4>初始化日志</h4>
          <span class="hint">最近 {{ envLogs.length }} 条</span>
        </div>
        <div ref="logBox" class="env-log-box" aria-label="初始化日志">
          <p v-if="!envLogs.length" class="hint">暂无日志（开始初始化后实时滚动）</p>
          <div v-for="(l, i) in envLogs" :key="l.seq ?? `${l.timestamp}-${i}`" class="env-log-line">
            <span class="env-log-time">{{ shortTime(l.timestamp) }}</span>
            <span class="env-log-msg" :class="`lv-${l.level}`">{{ l.message }}</span>
          </div>
        </div>
      </section>
    </div>

    <template #footer>
      <span class="hint">关闭弹窗不会中断后台初始化，可稍后回到设置页查看结果。</span>
      <button type="button" class="btn btn-secondary btn-sm" @click="emit('close')">关闭</button>
    </template>
  </Modal>
</template>

<style scoped>
.env-dlg {
  display: flex;
  flex-direction: column;
  gap: var(--space-lg);
}

/* 阶段步骤条：横向一排，等分 */
.env-steps {
  display: flex;
  gap: var(--space-sm);
  margin: 0;
  padding: 0;
  list-style: none;
  flex-wrap: wrap;
}
.env-step {
  display: flex;
  align-items: center;
  gap: 6px;
  padding: 6px 10px;
  border: 1px solid var(--border);
  border-radius: var(--radius-md);
  color: var(--text-secondary);
  font-size: var(--text-sm);
  flex: 1 1 auto;
  min-width: 0;
}
.env-step-label {
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}
.env-step.is-done {
  color: var(--success);
  border-color: var(--success);
}
.env-step.is-current {
  color: var(--primary);
  border-color: var(--primary);
}
.env-step.is-failed {
  color: var(--danger);
  border-color: var(--danger);
}
.env-step-pending-dot {
  width: 8px;
  height: 8px;
  border-radius: 50%;
  background: var(--border);
  flex: none;
}

/* 进度行：原生 progress + accent-color 简样式（超过该范围会关闭 UA 内建样式） */
.env-progress-line {
  display: flex;
  align-items: center;
  gap: var(--space-md);
}
.env-progress {
  flex: 1 1 auto;
  height: 8px;
  accent-color: var(--primary);
}
.env-progress-text {
  font-size: var(--text-sm);
  color: var(--text-secondary);
  white-space: nowrap;
}
.env-poll-error {
  font-size: var(--text-sm);
  color: var(--danger);
}

.env-preline {
  white-space: pre-line;
}
.env-dlg-error {
  margin: 0;
  color: var(--danger);
  font-size: var(--text-sm);
}

/* 分区标题行 */
.env-dlg-section {
  display: flex;
  flex-direction: column;
  gap: var(--space-sm);
}
.env-dlg-section-head {
  display: flex;
  align-items: baseline;
  justify-content: space-between;
  gap: var(--space-sm);
}
.env-dlg-section-head h4 {
  margin: 0;
  font-size: var(--text-base);
  color: var(--text-primary);
}

/* 下载源选择行：一排等宽按钮 */
.mirror-row {
  display: grid;
  grid-template-columns: repeat(auto-fit, minmax(140px, 1fr));
  gap: var(--space-sm);
}
.mirror-opt {
  display: flex;
  flex-direction: column;
  align-items: flex-start;
  gap: 2px;
  padding: 8px 12px;
  border: 1px solid var(--border);
  border-radius: var(--radius-md);
  background: transparent;
  color: var(--text-primary);
  cursor: pointer;
  text-align: left;
}
.mirror-opt:disabled {
  cursor: default;
}
.mirror-opt:hover:not(:disabled) {
  border-color: var(--primary);
}
.mirror-opt.selected {
  border-color: var(--primary);
  box-shadow: 0 0 0 1px var(--primary);
}
.mirror-opt-name {
  font-size: var(--text-sm);
  font-weight: 500;
}
.mirror-opt-state {
  font-size: var(--text-xs);
  color: var(--text-secondary);
}
.mirror-opt-state.is-dead {
  color: var(--danger);
}

/* 日志尾部：等宽滚动区，自动贴底 */
.env-log-box {
  max-height: 200px;
  overflow-y: auto;
  overscroll-behavior: contain;
  border: 1px solid var(--border);
  border-radius: var(--radius-md);
  padding: var(--space-sm);
  font-family: var(--font-mono);
  font-size: var(--text-xs);
  background: var(--bg-secondary);
}
.env-log-line {
  display: flex;
  gap: var(--space-sm);
  line-height: 1.5;
}
.env-log-time {
  color: var(--text-secondary);
  flex: none;
}
.env-log-msg {
  overflow-wrap: anywhere;
  white-space: pre-wrap;
}
.lv-error { color: var(--log-error); }
.lv-warn { color: var(--log-warn); }
</style>
