<script setup lang="ts">
/**
 * 首次启动配置向导（此前仅协议确认，现扩为五步）：
 * 使用协议 → 登录方式 → 环境准备（仅浏览器渠道）→ 学校与任务匹配 → 完成。
 *
 * - 全屏阻断例外：无 Teleport/关闭入口，打开期间锁定背景滚动（沿用原协议向导口径）。
 * - 显隐门槛仍是 init-status 的 `agreed`（仅全新安装显示；同意即写标记，中途退出不再续显）。
 * - 登录方式：浏览器渠道无绑定校验，选定即写 `login_channel`；直连渠道受
 *   `validate_login_task_binding` 约束——必须与已存在的直连任务绑定同一次提交，
 *   故延后到任务匹配步导入成功后才一并 PATCH，无匹配则保持浏览器渠道。
 * - 环境准备：bootstrap 是同步端点（最长可达数十分钟），采用 fire-and-forget +
 *   轮询 init-status 的 stage/progress 观察进度；浏览器选择与「设置 · 浏览器」同口径。
 * - 任务匹配：对两个预设镜像源并行测速（utils/repoSpeed），胜出源拉索引后按学校名
 *   过滤（与仓库导入弹窗共用 filterRepoTasks 口径），导入复用 RepoImportModals。
 * - 完成步自动应用「调试模式」预设，引导用户完成方案配置后到「设置 · 系统」切回。
 */
import { computed, onBeforeUnmount, ref, watch } from "vue";
import IconApp from "./IconApp.vue";
import BrowserIcon from "./BrowserIcon.vue";
import { useStatus } from "../../composables/useStatus";
import { useUi } from "../../composables/useUi";
import { useConfig } from "../../composables/useConfig";
import { useConfirm } from "../../composables/useConfirm";
import { useToast } from "../../composables/useToast";
import { useRunMode } from "../../composables/useRunMode";
import { useRepoImport, filterRepoTasks } from "../../composables/useRepoImport";
import { browsersApi, configApi, environmentApi } from "../../api";
import { extractApiError } from "../../api/client";
import { frontendLogger } from "../../utils/logger";
import { DOCS, type TaskRepoKind, type TaskRepoSourceId } from "../../utils/constants";
import { measureRepoSources, pickFastestSource, type RepoSourceTiming } from "../../utils/repoSpeed";
import type { BrowserInfo, EnvironmentStatus, RepoTask } from "../../api/types";
import { lockBodyScroll, unlockBodyScroll } from "../../composables/useBodyScrollLock";

const { state, agreeWizardTerms, closeWizard } = useUi();
const { busy } = useStatus();
const { confirm } = useConfirm();
const { toastOnly } = useToast();
const { applyRunMode } = useRunMode();
const { showRepoImport } = useRepoImport();
const configStore = useConfig();

// 向导展示期间锁定背景滚动（全屏阻断，无其他关闭路径）
// FE2-3：滚动锁走全局计数，与 Modal/ConfirmDialog 协调
watch(
  () => state.showWizard,
  (val) => {
    if (val) lockBodyScroll();
    else unlockBodyScroll();
  },
  { immediate: true },
);

/* ============ 步骤定义与导航 ============ */

type StepKey = "terms" | "channel" | "environment" | "school" | "done";

const STEPS: { key: StepKey; title: string }[] = [
  { key: "terms", title: "使用协议" },
  { key: "channel", title: "登录方式" },
  { key: "environment", title: "环境准备" },
  { key: "school", title: "任务匹配" },
  { key: "done", title: "完成" },
];

/** 每步的标题与副标题（头部随步骤切换） */
const STEP_HEADER: Record<StepKey, { title: string; sub: string }> = {
  terms: {
    title: "欢迎使用认证喵（Campus-Auth 校园网自动认证）",
    sub: "请阅读以下协议内容，同意后进入初始配置向导",
  },
  channel: {
    title: "选择你的登录方式",
    sub: "决定用浏览器自动化还是直连请求完成校园网认证，后续可在「方案」页修改",
  },
  environment: {
    title: "准备运行环境",
    sub: "浏览器自动化需要 Python 环境与一个可用的浏览器，可直接自动安装或选择已有浏览器",
  },
  school: {
    title: "匹配你学校的登录任务",
    sub: "输入学校名称，自动为两个任务仓库测速、择优获取索引并查找匹配的社区任务",
  },
  done: {
    title: "初始配置完成",
    sub: "已切换至调试模式，完成方案配置后可切换回默认模式日常使用",
  },
};

const currentKey = ref<StepKey>("terms");

/** 环境准备步只属于浏览器渠道：选直连后从流程里消失 */
const visibleSteps = computed(() => STEPS.filter((s) => s.key !== "environment" || selectedChannel.value !== "http"));
const currentIndex = computed(() => visibleSteps.value.findIndex((s) => s.key === currentKey.value));

function goStepIndex(i: number) {
  // 只允许回到已到达过的步骤，不能跳过未走过的流程
  if (i >= 0 && i <= currentIndex.value) currentKey.value = visibleSteps.value[i].key;
}
function goNext() {
  const next = visibleSteps.value[currentIndex.value + 1];
  if (next) currentKey.value = next.key;
}
function goPrev() {
  const prev = visibleSteps.value[currentIndex.value - 1];
  if (prev) currentKey.value = prev.key;
}

/* ============ 第 1 步：使用协议 ============ */

async function proceedFromTerms() {
  const ok = await agreeWizardTerms();
  if (ok) goNext();
}

/* ============ 第 2 步：登录方式 ============ */

const selectedChannel = ref<"browser" | "http" | "">("");
const channelBusy = ref(false);

/**
 * 切换登录选择时清空任务匹配的现场：两类渠道读不同索引、绑定字段也不同，
 * 残留的测速结果/已导入 id 会把 A 渠道的任务绑进 B 渠道的方案。
 */
function resetMatchState() {
  speedResults.value = [];
  speedDone.value = false;
  matchedTasks.value = [];
  importedId.value = "";
  httpChannelDropped.value = false;
}

function chooseChannel(ch: "browser" | "http") {
  if (selectedChannel.value === ch) return;
  selectedChannel.value = ch;
  resetMatchState();
}

async function proceedFromChannel() {
  if (!selectedChannel.value || channelBusy.value) return;
  if (selectedChannel.value === "http") {
    // 直连渠道此时还不能写：后端校验「渠道 + 已存在绑定」的合并状态，
    // 导入任务后在任务匹配步与绑定一并提交
    goNext();
    return;
  }
  channelBusy.value = true;
  try {
    // 浏览器渠道无绑定校验（有内置默认任务兜底），随时可安全写入
    await configApi.patchProfileBinding({ login_channel: "browser" });
    goNext();
  } catch (e) {
    toastOnly(false, extractApiError(e, "保存登录方式失败"));
  } finally {
    channelBusy.value = false;
  }
}

/* ============ 第 3 步：环境准备（仅浏览器渠道） ============ */

const envStatus = ref<EnvironmentStatus | null>(null);
const envError = ref("");
const envPreparing = ref(false);
let envPollTimer: ReturnType<typeof setInterval> | undefined;

const BROWSER_OFFICIAL_URL: Record<string, string> = {
  msedge: "https://www.microsoft.com/edge/download",
  chrome: "https://www.google.com/chrome/",
};
/** Playwright 可托管的引擎（可由本程序安装）；Edge/Chrome 属系统浏览器只能引导官网 */
const PLAYWRIGHT_INSTALLABLE = new Set(["chromium", "firefox", "webkit"]);

const browsers = ref<BrowserInfo[]>([]);
const browsersLoading = ref(false);
const browserError = ref("");
const browserCurrent = ref(configStore.config.browser.browser_channel);
const browserSaving = ref(false);
const installingChannel = ref("");

const BOOTSTRAP_STAGE_LABELS: Record<string, string> = {
  Idle: "待机",
  DownloadingUv: "下载 uv",
  SyncingVenv: "同步 Python 依赖",
  VerifyingWorker: "校验认证核心",
  ApplyingOcr: "对齐 OCR 组件",
  InstallingPlaywright: "安装 Playwright 浏览器",
  Done: "完成",
  Error: "出错",
};

const envItems = computed(() => {
  const e = envStatus.value;
  return [
    { label: "uv", ready: e?.uv_ready ?? false },
    { label: "Python", ready: e?.python_ready ?? false },
    { label: "认证核心（认证 Worker）", ready: e?.worker_ready ?? false },
    { label: "Playwright 浏览器引擎", ready: e?.playwright_ready ?? false },
  ];
});

const envStageLabel = computed(() => {
  const stage = envStatus.value?.stage ?? "";
  return BOOTSTRAP_STAGE_LABELS[stage] ?? stage;
});

/** bootstrap 进度文案（phase 消息 + 百分比），无进度时为空串 */
const bootstrapProgressText = computed(() => {
  const p = envStatus.value?.progress;
  if (!p) return "";
  const msg = p.message ? `：${p.message}` : "";
  const percent = typeof p.percent === "number" ? `（${p.percent}%）` : "";
  return `${msg}${percent}`;
});

async function refreshEnvStatus() {
  envError.value = "";
  try {
    const status = await environmentApi.fetchStatus();
    envStatus.value = status;
  } catch (e) {
    envError.value = extractApiError(e, "环境状态检测失败");
  }
}

function stopBootstrapPoll() {
  if (envPollTimer) {
    clearInterval(envPollTimer);
    envPollTimer = undefined;
  }
}

/** bootstrap 完成或失败后的统一收尾 */
async function finishBootstrapPoll(done: boolean, message = "") {
  stopBootstrapPoll();
  envPreparing.value = false;
  if (message) envError.value = message;
  if (done) {
    // 引导可能顺带装好 Playwright Chromium，重查浏览器列表
    void fetchBrowsers();
  }
}

function startBootstrap() {
  if (envPreparing.value) return;
  envPreparing.value = true;
  envError.value = "";
  // 同步端点最长可达数十分钟（uv 下载×3 重试 + uv sync + 浏览器安装），不能 await：
  // 请求超时也不代表后端停止，进度一律以轮询 init-status 为准
  void environmentApi.bootstrap().catch(() => undefined);
  stopBootstrapPoll();
  envPollTimer = setInterval(() => void pollBootstrapStatus(), 2000);
  void pollBootstrapStatus();
}

async function pollBootstrapStatus() {
  try {
    const status = await environmentApi.fetchStatus();
    if (status) envStatus.value = status;
  } catch {
    // 单次轮询失败忽略，下一轮再试
    return;
  }
  const e = envStatus.value;
  if (!e) return;
  if (e.capability_ready) {
    frontendLogger.info("wizard", "环境初始化完成");
    await finishBootstrapPoll(true);
  } else if (e.stage === "Error") {
    await finishBootstrapPoll(false, e.last_error || "环境初始化失败，请查看日志");
  }
}

async function fetchBrowsers() {
  browsersLoading.value = true;
  browserError.value = "";
  try {
    const data = await browsersApi.fetch();
    browsers.value = data.browsers;
    browserCurrent.value = data.current;
  } catch (e) {
    browserError.value = extractApiError(e, "浏览器检测失败");
  } finally {
    browsersLoading.value = false;
  }
}

/** 选中一个已安装浏览器：与 useRunMode 同口径整段 PATCH，回读对齐快照（避免设置页出现幽灵"未保存"） */
async function chooseBrowser(b: BrowserInfo) {
  if (!b.installed || b.channel === browserCurrent.value || browserSaving.value) return;
  browserSaving.value = true;
  browserError.value = "";
  try {
    await configApi.patch({
      browser: { ...configStore.config.browser, browser_channel: b.channel },
      worker: configStore.config.worker,
      monitor: configStore.config.monitor,
      pause: configStore.config.pause,
      logging: configStore.config.logging,
      retry: configStore.config.retry,
      app_settings: configStore.config.app_settings,
      updater: configStore.config.updater,
    });
    await configStore.fetchConfig();
    browserCurrent.value = b.channel;
  } catch (e) {
    browserError.value = extractApiError(e, "保存浏览器选择失败");
  } finally {
    browserSaving.value = false;
  }
}

/** 经 Playwright 安装指定引擎（与「设置 · 浏览器」同口径）；装完重查列表并选中 */
async function installBrowserEngine(channel: string) {
  if (installingChannel.value) return;
  installingChannel.value = channel;
  browserError.value = "";
  try {
    // 安装可能持续数分钟，放宽超时（后端同步等待完成）
    await browsersApi.installPlaywright(channel, { timeout: 650000 });
    const data = await browsersApi.fetch();
    browsers.value = data.browsers;
    const found = browsers.value.find((b) => b.channel === channel);
    if (found) {
      await chooseBrowser(found);
      return;
    }
    browserError.value = `${channel} 安装完成，但未检测到浏览器，请到「设置 · 浏览器」重试`;
  } catch (e) {
    browserError.value = `${channel} 安装失败：${extractApiError(e, "未知错误")}`;
  } finally {
    installingChannel.value = "";
  }
}

// 进入环境准备步时拉取环境与浏览器列表
watch(currentKey, (key) => {
  if (key === "environment") {
    void refreshEnvStatus();
    void fetchBrowsers();
  }
});

onBeforeUnmount(stopBootstrapPoll);

/* ============ 第 4 步：学校与任务匹配 ============ */

const schoolName = ref("");
const speedRunning = ref(false);
const speedDone = ref(false);
const speedResults = ref<RepoSourceTiming[]>([]);
const matchedTasks = ref<RepoTask[]>([]);
const importedId = ref("");
/** 直连渠道未导入任务时用户显式确认「保持浏览器渠道并继续」 */
const httpChannelDropped = ref(false);

/** 向导读哪份索引、导入到哪类任务，跟随登录方式 */
const repoKind = computed<TaskRepoKind>(() => (selectedChannel.value === "http" ? "http" : "browser"));

const fastestSource = computed<TaskRepoSourceId | null>(() => pickFastestSource(speedResults.value));

function sourceLabel(source: TaskRepoSourceId): string {
  return { github: "GitHub", gitee: "Gitee", custom: "自定义" }[source];
}

/**
 * 测速 + 匹配：两个预设源并行计时拉索引（胜出源的条目直接复用，不重复拉），
 * 再按学校名过滤（与导入弹窗同一匹配口径）。
 */
async function runSchoolMatch() {
  const q = schoolName.value.trim();
  if (!q || speedRunning.value) return;
  speedRunning.value = true;
  speedDone.value = false;
  matchedTasks.value = [];
  try {
    const results = await measureRepoSources(repoKind.value);
    speedResults.value = results;
    speedDone.value = true;
    const fastest = pickFastestSource(results);
    if (!fastest) return;
    const winner = results.find((r) => r.source === fastest)!;
    matchedTasks.value = filterRepoTasks(winner.tasks, repoKind.value, q);
  } finally {
    speedRunning.value = false;
  }
}

/**
 * 打开复用的仓库导入弹窗：预置胜出源与关键词（浏览全部时不带关键词），
 * 导入成功经 afterImport 回到向导（不跳编辑器）并绑定到方案。
 */
function openImport(withKeyword: boolean) {
  const fastest = fastestSource.value;
  showRepoImport(repoKind.value, {
    ...(fastest ? { source: fastest } : {}),
    ...(withKeyword && schoolName.value.trim() ? { keyword: schoolName.value.trim() } : {}),
    autoFetch: true,
    afterImport: (id) => onImported(id),
  });
}

/** 导入落盘后的绑定：直连渠道必须「渠道 + 绑定」同一次提交（后端校验合并后状态） */
async function onImported(id: string) {
  if (selectedChannel.value === "http") {
    await configApi.patchProfileBinding({ login_channel: "http", active_http_task: id });
  } else {
    await configApi.patchProfileBinding({ active_task: id });
  }
  importedId.value = id;
  // 导入即配置完成，直接进入完成步（调试模式提示）
  goNext();
}

async function proceedFromSchool() {
  if (selectedChannel.value === "http" && !importedId.value) {
    const ok = await confirm({
      title: "未导入直连任务",
      message:
        "直连渠道需要一个直连任务才能登录，未导入时将保持浏览器渠道。可以稍后在任务页的「仓库导入」中再导入。确定继续吗？",
      confirmText: "保持浏览器渠道并继续",
      cancelText: "返回",
    });
    if (!ok) return;
    httpChannelDropped.value = true;
  }
  goNext();
}

/* ============ 第 5 步：完成（应用调试模式） ============ */

const debugState = ref<"applying" | "applied" | "failed">("applying");

watch(
  () => currentKey.value === "done",
  (onDone) => {
    if (onDone && debugState.value === "applying") void applyDebugMode();
  },
);

async function applyDebugMode() {
  const ok = await applyRunMode("debug");
  debugState.value = ok ? "applied" : "failed";
}

/** 完成步展示的最终登录方式：直连未导入任务时实际保持浏览器渠道 */
const appliedChannelLabel = computed(() => {
  if (selectedChannel.value === "http" && importedId.value) return "直连请求";
  return "浏览器自动化";
});

/* ============ 跳过向导 ============ */

async function skipWizard() {
  const ok = await confirm({
    title: "跳过配置向导",
    message: "跳过后可随时在「方案」与「设置」页完成登录方式、环境和任务的配置。确定跳过吗？",
    confirmText: "跳过",
    cancelText: "继续配置",
  });
  if (!ok) return;
  frontendLogger.info("wizard", "已跳过首次配置向导");
  closeWizard();
}
</script>

<template>
  <div v-if="state.showWizard" class="wizard-overlay" role="dialog" aria-modal="true" aria-label="首次启动配置向导">
    <div class="wizard-container">
      <div class="wizard-header">
        <span class="wizard-logo logo-mark" role="img" aria-label="认证喵 Campus-Auth"></span>
        <h1>{{ STEP_HEADER[currentKey].title }}</h1>
        <p>{{ STEP_HEADER[currentKey].sub }}</p>
      </div>

      <!-- 步骤条：已到达步骤可点回看，未到达禁用（与直连配置向导同一交互口径） -->
      <ol class="wizard-steps">
        <li
          v-for="(step, i) in visibleSteps"
          :key="step.key"
          class="wizard-step"
          :class="{ active: i === currentIndex, done: i < currentIndex }"
        >
          <button type="button" class="wizard-step-btn" :disabled="i > currentIndex" @click="goStepIndex(i)">
            <span class="wizard-step-num">
              <IconApp v-if="i < currentIndex" name="check" class="icon-sm" />
              <template v-else>{{ i + 1 }}</template>
            </span>
            <span>{{ step.title }}</span>
          </button>
        </li>
      </ol>

      <div class="wizard-content">
        <!-- 第 1 步：使用协议（沿用原协议向导全文） -->
        <div v-if="currentKey === 'terms'" class="wizard-page">
          <h2>使用协议与免责声明</h2>
          <div class="terms-content">
            <h4>使用协议</h4>
            <p>本软件（认证喵 / Campus-Auth）是一款校园网自动认证工具，仅供学习和个人使用。使用本软件前，请您仔细阅读并理解以下条款：</p>
            <ul>
              <li>本软件按"现状"提供，不作任何明示或暗示的保证。</li>
              <li>用户应自行承担使用本软件的一切风险和后果。</li>
              <li>用户应遵守所在学校和网络服务提供商的相关规定。</li>
              <li>用户不得将本软件用于任何非法用途或违反相关法律法规的行为。</li>
              <li>本软件开发者不对因使用本软件而产生的任何直接或间接损失承担责任。</li>
            </ul>
            <h4>免责声明</h4>
            <ul>
              <li>本软件不保证在所有网络环境下均能正常工作。</li>
              <li>因网络环境变化、学校政策调整等原因导致软件无法使用，开发者不承担责任。</li>
              <li>用户因使用本软件导致的账号异常、网络服务中断等问题，开发者不承担责任。</li>
              <li>本软件可能因系统更新、依赖变更等原因需要调整，开发者保留随时修改或终止软件的权利。</li>
            </ul>
          </div>
          <div class="docs-hint">
            使用遇到问题？请查阅在线文档：
            <a :href="DOCS.faqLogin" target="_blank" rel="noopener noreferrer">无法自动登录排查（常见问题）</a>
            ·
            <a :href="DOCS.gettingStarted" target="_blank" rel="noopener noreferrer">快速开始（新手上路）</a>
          </div>
          <div class="terms-checkbox">
            <label class="toggle">
              <input type="checkbox" v-model="state.agreedToTerms" />
              <span class="toggle-slider"></span>
              <span class="toggle-label">我已阅读并同意《使用协议》和《免责声明》</span>
            </label>
          </div>
        </div>

        <!-- 第 2 步：登录方式 -->
        <div v-else-if="currentKey === 'channel'" class="wizard-page">
          <div class="wizard-channel-grid" role="radiogroup" aria-label="登录方式">
            <button
              type="button"
              class="wizard-channel-card"
              :class="{ selected: selectedChannel === 'browser' }"
              @click="chooseChannel('browser')"
            >
              <IconApp name="chrome" class="wizard-channel-icon" />
              <strong>浏览器自动化</strong>
              <span>需要 Python 与浏览器环境</span>
              <span>适合有验证码、动态表单或复杂交互的门户</span>
            </button>
            <button
              type="button"
              class="wizard-channel-card"
              :class="{ selected: selectedChannel === 'http' }"
              @click="chooseChannel('http')"
            >
              <IconApp name="globe" class="wizard-channel-icon" />
              <strong>直连请求</strong>
              <span>免 Python 与浏览器环境</span>
              <span>适合门户登录接口可直接调用、无验证码的情况</span>
            </button>
          </div>
          <p class="wizard-note">自定义脚本渠道属高级用法，可在「方案」页的登录方式中配置；不确定选哪个时，先选浏览器自动化。</p>
        </div>

        <!-- 第 3 步：环境准备（仅浏览器渠道，直连流程不含此步） -->
        <div v-else-if="currentKey === 'environment'" class="wizard-page">
          <div class="wizard-section">
            <h3>Python 环境</h3>
            <ul class="wizard-env-list">
              <li v-for="item in envItems" :key="item.label" class="wizard-env-item">
                <IconApp :name="item.ready ? 'check-circle' : 'x-circle'" class="icon-sm" :class="item.ready ? 'wizard-env-ok' : 'wizard-env-missing'" />
                <span>{{ item.label }}</span>
                <span class="wizard-env-state">{{ item.ready ? "就绪" : "未就绪" }}</span>
              </li>
            </ul>
            <div v-if="envPreparing" class="wizard-note">
              <IconApp name="refresh" class="icon-sm spin" />
              正在初始化环境（{{ envStageLabel }}）{{ bootstrapProgressText }}，可能需要几分钟，请保持网络可用。
            </div>
            <div v-else-if="envError" class="wizard-note wizard-note--error">
              <IconApp name="alert-triangle" class="icon-sm" />
              {{ envError }}
            </div>
            <div v-else-if="envStatus && !envStatus.capability_ready" class="wizard-note wizard-note--warn">
              环境尚未就绪。可点击下方按钮自动下载并安装（首次安装耗时数分钟，取决于网络）。
            </div>
            <button
              v-if="!envPreparing"
              class="btn btn-secondary btn-sm"
              :disabled="!!envStatus?.capability_ready"
              @click="startBootstrap"
            >
              <IconApp name="download" class="icon-sm" />
              {{ envStatus?.capability_ready ? "环境已就绪" : envError || envStatus?.last_error ? "重新安装" : "自动安装" }}
            </button>
          </div>

          <div class="wizard-section">
            <h3>选择浏览器</h3>
            <p class="wizard-section-sub">自动探测系统中已安装的浏览器，也可安装 Playwright 托管引擎；随后可在此修改。</p>
            <div v-if="browsersLoading" class="wizard-note"><IconApp name="refresh" class="icon-sm spin" /> 正在检测浏览器…</div>
            <div v-else-if="browsers.length === 0" class="wizard-note wizard-note--warn">未检测到任何可用浏览器。</div>
            <div v-else class="wizard-browser-list" role="radiogroup" aria-label="浏览器选择">
              <!-- 已安装与 Playwright 可装引擎是可点项（选择 / 安装）；其余（未装的
                   Edge/Chrome 等）是静态项，只放官网链接，不伪装成可点的按钮 -->
              <template v-for="b in browsers" :key="b.channel">
                <button
                  v-if="b.installed || PLAYWRIGHT_INSTALLABLE.has(b.channel)"
                  type="button"
                  class="wizard-browser-item"
                  :class="{ selected: browserCurrent === b.channel }"
                  :disabled="browserSaving || !!installingChannel"
                  @click="b.installed ? chooseBrowser(b) : installBrowserEngine(b.channel)"
                >
                  <BrowserIcon :channel="b.channel" :size="20" />
                  <span class="wizard-browser-name">{{ b.name }}</span>
                  <span v-if="b.installed" class="wizard-browser-state">{{ browserCurrent === b.channel ? "当前使用" : "已安装" }}</span>
                  <span v-else-if="installingChannel === b.channel" class="wizard-browser-state wizard-browser-install">
                    <IconApp name="refresh" class="icon-sm spin" /> 安装中…
                  </span>
                  <span v-else class="wizard-browser-state wizard-browser-install">点击安装</span>
                </button>
                <div v-else class="wizard-browser-item wizard-browser-item--static">
                  <BrowserIcon :channel="b.channel" :size="20" />
                  <span class="wizard-browser-name">{{ b.name }}</span>
                  <a
                    v-if="BROWSER_OFFICIAL_URL[b.channel]"
                    class="wizard-browser-state"
                    :href="BROWSER_OFFICIAL_URL[b.channel]"
                    target="_blank"
                    rel="noopener noreferrer"
                  >官网下载</a>
                  <span v-else class="wizard-browser-state">未安装</span>
                </div>
              </template>
            </div>
            <div v-if="browserError" class="wizard-note wizard-note--error">
              <IconApp name="alert-triangle" class="icon-sm" />
              {{ browserError }}
            </div>
            <p class="wizard-note wizard-note--muted">环境与浏览器也可以稍后在「设置 · 任务与环境 / 浏览器」中配置，不阻塞继续。</p>
          </div>
        </div>

        <!-- 第 4 步：学校与任务匹配 -->
        <div v-else-if="currentKey === 'school'" class="wizard-page">
          <div class="wizard-field">
            <label for="wizard-school-input">学校名称</label>
            <div class="wizard-field-row">
              <!-- 接入全局 form-group 皮肤（bg/border/radius/聚焦环），与全站输入框一致 -->
              <div class="form-group form-group--flush wizard-field-input">
                <input
                  id="wizard-school-input"
                  v-model="schoolName"
                  type="text"
                  placeholder="例如：四川大学"
                  @keydown.enter="runSchoolMatch"
                />
              </div>
              <button class="btn btn-primary" :disabled="!schoolName.trim() || speedRunning" @click="runSchoolMatch">
                <IconApp :name="speedRunning ? 'refresh' : 'search'" class="icon-sm" :class="{ spin: speedRunning }" />
                {{ speedRunning ? "测速并匹配中…" : speedDone ? "重新匹配" : "测速并匹配任务" }}
              </button>
            </div>
          </div>

          <!-- 测速结果：两个镜像源并列展示，最快者标记"已选" -->
          <div v-if="speedDone" class="wizard-speed">
            <span
              v-for="r in speedResults"
              :key="r.source"
              class="wizard-speed-item"
              :class="{ 'wizard-speed-fail': r.ms === null, 'wizard-speed-best': r.source === fastestSource }"
            >
              {{ sourceLabel(r.source) }}：{{ r.ms !== null ? `${r.ms} ms` : "不可用" }}
              <em v-if="r.source === fastestSource">已选</em>
            </span>
          </div>

          <div v-if="speedDone && !fastestSource" class="wizard-note wizard-note--warn">
            <IconApp name="alert-triangle" class="icon-sm" />
            两个任务仓库均不可达，可能当前处于离线或受限网络。可点击「重新匹配」重试，或稍后在任务页的「仓库导入」中操作。
          </div>

          <template v-else-if="speedDone">
            <!-- 有匹配：列出条目，点「查看并导入」打开复用的仓库导入弹窗 -->
            <div v-if="matchedTasks.length > 0" class="wizard-matches">
              <p class="wizard-match-title">
                找到 {{ matchedTasks.length }} 个与「{{ schoolName.trim() }}」相关的{{ repoKind === "http" ? "直连" : "浏览器" }}任务：
              </p>
              <div v-for="t in matchedTasks" :key="t.id" class="wizard-match-item">
                <div class="wizard-match-text">
                  <strong>{{ t.name }}</strong>
                  <span v-if="t.description">{{ t.description }}</span>
                </div>
              </div>
              <button class="btn btn-primary btn-sm wizard-match-cta" @click="openImport(true)">
                <IconApp name="download" class="icon-sm" />
                查看并导入
              </button>
            </div>
            <!-- 无匹配：结构化引导（浏览器与直连共用，选项按渠道微调） -->
            <div v-else class="wizard-nomatch">
              <p class="wizard-nomatch-title">
                <IconApp name="info" class="icon-sm" />
                当前无适配「{{ schoolName.trim() }}」的任务
              </p>
              <ul class="wizard-nomatch-options">
                <li v-if="repoKind === 'browser'">
                  <strong>尝试默认任务</strong>
                  <span>内置任务覆盖大多数标准门户，完成向导后即可直接使用</span>
                </li>
                <li>
                  <strong>尝试自定义任务</strong>
                  <span>{{ repoKind === "http" ? "到「任务」页新建直连任务，编辑器内的配置向导可引导完成登录请求配置" : "到「任务」页新建或录制浏览器任务，也可用 AI 生成" }}</span>
                </li>
              </ul>
              <p class="wizard-nomatch-docs">
                详细请查看文档：
                <a v-if="repoKind === 'http'" :href="DOCS.httpLogin" target="_blank" rel="noopener noreferrer">直连请求登录</a>
                <a v-else :href="DOCS.taskBrowser" target="_blank" rel="noopener noreferrer">浏览器任务</a>
                ·
                <a :href="DOCS.faqLogin" target="_blank" rel="noopener noreferrer">无法自动登录排查</a>
              </p>
              <button class="btn btn-secondary btn-sm" @click="openImport(false)">浏览全部任务</button>
            </div>
          </template>

          <div v-if="importedId" class="wizard-note wizard-note--ok">
            <IconApp name="check-circle" class="icon-sm" />
            已导入任务并{{ selectedChannel === "http" ? "切换到直连渠道、绑定" : "绑定" }}到当前方案。
          </div>
        </div>

        <!-- 第 5 步：完成（应用调试模式） -->
        <div v-else-if="currentKey === 'done'" class="wizard-page">
          <div class="wizard-done">
            <IconApp name="check-circle" class="wizard-done-icon" />
            <div v-if="debugState === 'applying'" class="wizard-note"><IconApp name="refresh" class="icon-sm spin" /> 正在应用调试模式…</div>
            <div v-else-if="debugState === 'applied'" class="wizard-mode-note">
              <strong>当前已切换至调试模式</strong>
              <p>
                浏览器窗口可见、日志记录 DEBUG 级别、启动后不自动登录，便于你完成剩余配置并验证登录。
                请到「方案」页填写校园网账号、密码与认证地址，确认能正常登录后，
                到「设置 · 系统」把运行模式切换回<strong>默认模式</strong>即可日常使用。
              </p>
            </div>
            <div v-else class="wizard-note wizard-note--warn">
              调试模式应用失败（部分设置可能已生效），可稍后在「设置 · 系统」的运行模式中手动切换。
            </div>
            <ul class="wizard-done-list">
              <li>登录方式：{{ appliedChannelLabel }}{{ importedId ? "（已绑定导入的任务）" : "" }}</li>
              <li v-if="selectedChannel === 'browser' && !importedId">浏览器渠道已内置默认登录任务，可直接使用或之后替换</li>
              <li>环境或浏览器未就绪的，可到「设置 · 任务与环境 / 浏览器」继续安装</li>
            </ul>
          </div>
        </div>
      </div>

      <div class="wizard-footer">
        <button v-if="currentIndex > 0" class="btn btn-ghost" @click="skipWizard">跳过向导</button>
        <div class="spacer"></div>
        <button v-if="currentIndex > 0 && currentKey !== 'done'" class="btn btn-secondary" @click="goPrev">上一步</button>
        <!-- 第 1 步：同意协议 -->
        <button v-if="currentKey === 'terms'" class="btn btn-primary" :disabled="!state.agreedToTerms || busy.save" @click="proceedFromTerms">
          同意并开始配置
        </button>
        <!-- 第 2 步：选完登录方式才能继续 -->
        <button v-else-if="currentKey === 'channel'" class="btn btn-primary" :disabled="!selectedChannel || channelBusy" @click="proceedFromChannel">
          下一步
        </button>
        <!-- 第 3 步：环境未就绪也不阻塞（可稍后在设置中安装） -->
        <button v-else-if="currentKey === 'environment'" class="btn btn-primary" @click="goNext">下一步</button>
        <!-- 第 4 步：直连渠道未导入任务时先确认降级为浏览器渠道 -->
        <button v-else-if="currentKey === 'school'" class="btn btn-primary" :disabled="speedRunning" @click="proceedFromSchool">下一步</button>
        <!-- 第 5 步：完成 -->
        <button v-else-if="currentKey === 'done'" class="btn btn-primary" @click="closeWizard">进入认证喵</button>
      </div>
    </div>
  </div>
</template>
