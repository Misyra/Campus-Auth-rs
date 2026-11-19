<script setup lang="ts">
import IconApp from "@/components/common/IconApp.vue";
import EmptyState from "@/components/common/EmptyState.vue";
import { ref, computed, onMounted, onBeforeUnmount, nextTick, watch } from "vue";
import { useRouter } from "vue-router";
import { useStatus } from "@/composables/useStatus";
import { useEnvironment } from "@/composables/useEnvironment";
import { useProfiles } from "@/composables/useProfiles";
import { useLogs } from "@/composables/useLogs";
import { useUi } from "@/composables/useUi";
import { useAppearance } from "@/composables/useAppearance";
import { throttleRaf } from "@/utils/debounce";
import { LOG_SOURCE_LABELS } from "@/utils/constants";
import { channelNeedsRuntimeEnvironment } from "@/utils/loginChannel";
import { missingRequiredComponents } from "@/utils/environmentStatus";
import { formatDuration, formatTimestamp, formatShortTime } from "@/utils/formatters";
import CustomSelect from "@/components/common/CustomSelect.vue";
import NetworkDiagnosticsPanel from "@/components/NetworkDiagnosticsPanel.vue";
import type { SelectOption } from "@/components/common/CustomSelect.vue";
import type { LoginChannel } from "@/api/types";

const s = useStatus();
const logs = useLogs();
const ui = useUi();
const router = useRouter();
const { envStatus, refreshEnv, bootstrapEnv } = useEnvironment();
const { profiles, activeProfileId } = useProfiles();

// HTTP 登录渠道在 Rust 进程内完成登录，不拉起 Python Worker 与浏览器，
// 故环境未就绪与之无关——否则免 Python/浏览器的用户会一直看到无意义的
// "环境未就绪"横幅。
// 渠道取自活跃方案的摘要（ProfileSummary.login_channel）：它已是方案字段，
// 不再随 GET /api/config 的凭据投影下发。
const activeChannel = computed(
  () => profiles.value[activeProfileId.value]?.login_channel ?? "browser",
);
const showEnvBanner = computed(
  () =>
    channelNeedsRuntimeEnvironment(activeChannel.value) &&
    envStatus.value != null &&
    !envStatus.value.capability_ready,
);

// 横幅点名缺失的组件（口径与关于页/环境页共用 environmentStatus），
// 不再笼统说「Python 环境未就绪」——缺认证核心/浏览器时说 Python 会误导排查方向
const envMissingText = computed(() => {
  if (!envStatus.value) return "";
  const missing = missingRequiredComponents(envStatus.value);
  return missing.length ? missing.join("、") : "部分组件";
});

/** 横幅就地初始化：成功后 bootstrapEnv 内部会刷新状态，showEnvBanner 随之消失 */
async function initEnvNow() {
  await bootstrapEnv();
}

// FE2-8：延迟复查句柄须可清理——组件卸载后触发的 refreshEnv 虽是幂等 GET，
// 但仍会更新已卸载页面关联的单例状态，提前离开时取消
let envRetryTimer: ReturnType<typeof setTimeout> | null = null;

// ---- 悬浮工具条：默认收起为闪电球，点开展开动作列（speed dial）----
const dockOpen = ref(false);
const dockEl = ref<HTMLElement | null>(null);

// ---- 工具条位置：自由拖动 + localStorage 记忆 ----
// 存 right/bottom 偏移而非 left/top：换视口尺寸后仍保持「贴哪一角」的语义；
// 渲染前按当前视口夹取，旧的大屏坐标挪到小屏不会越界。
const DOCK_POS_KEY = "dash-dock-pos";
const DEFAULT_DOCK_POS = { right: 16, bottom: 24 };
/** 展开条需要的安全高度（6 钮 × 48px + 边框分隔线 + 间距余量） */
const DOCK_BAR_SPACE = 308;

const dockPos = ref({ ...DEFAULT_DOCK_POS });
const viewport = ref({ w: window.innerWidth, h: window.innerHeight });

const clampedDockPos = computed(() => ({
  right: Math.min(Math.max(dockPos.value.right, 0), Math.max(0, viewport.value.w - 50)),
  bottom: Math.min(Math.max(dockPos.value.bottom, 0), Math.max(0, viewport.value.h - 48)),
}));

// 球体上方 vs 下方的可用空间：上方够放整条展开条就向上弹；不够时选空间更大的一侧
// （都放不下也选大的，尽量少遮挡）
const expandUp = computed(() => {
  const ballTop = viewport.value.h - clampedDockPos.value.bottom - 48;
  const upSpace = ballTop;
  const downSpace = viewport.value.h - ballTop - 48;
  return upSpace >= DOCK_BAR_SPACE || upSpace >= downSpace;
});

const dockStyle = computed(() => {
  const style = { right: `${clampedDockPos.value.right}px` };
  if (expandUp.value) {
    // 向上展开：bottom 锚定球底边，展开条在容器内向球上方生长，球不动
    return { ...style, bottom: `${clampedDockPos.value.bottom}px` };
  }
  // 向下展开：top 锚定球顶边。容器高度随展开条（含收起态的占位）变化，
  // 若仍用 bottom 锚定，球会被整段展开条顶高约 300px 甚至顶出视口——
  // 「拖上去就消失、只有刷新闪一下」即此成因
  return { ...style, top: `${viewport.value.h - clampedDockPos.value.bottom - 48}px` };
});

function loadDockPos(): void {
  try {
    const raw = localStorage.getItem(DOCK_POS_KEY);
    if (!raw) return;
    const parsed = JSON.parse(raw) as { right?: number; bottom?: number };
    if (Number.isFinite(parsed.right) && Number.isFinite(parsed.bottom)) {
      dockPos.value = { right: parsed.right as number, bottom: parsed.bottom as number };
    }
  } catch {
    // 坏数据静默回退默认位置
  }
}

function saveDockPos(): void {
  try {
    localStorage.setItem(DOCK_POS_KEY, JSON.stringify(clampedDockPos.value));
  } catch {
    // 存储不可用则不记忆，本次会话内仍可拖动
  }
}
loadDockPos();

// 拖动：pointermove 累计位移 ≥5px 才算拖动（否则视为点击开合）；拖完抑制紧随的
// click，防止「拖完顺手开合」；setPointerCapture 让指针移出按钮也持续收到事件
let dragState: { startX: number; startY: number; baseRight: number; baseBottom: number; moved: boolean } | null = null;
let suppressToggleClick = false;
const dockDragging = ref(false);

function onDockTogglePointerDown(e: PointerEvent): void {
  if (e.pointerType === "mouse" && e.button !== 0) return;
  suppressToggleClick = false;
  dragState = {
    startX: e.clientX,
    startY: e.clientY,
    baseRight: clampedDockPos.value.right,
    baseBottom: clampedDockPos.value.bottom,
    moved: false,
  };
  (e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
}

function onDockTogglePointerMove(e: PointerEvent): void {
  if (!dragState) return;
  if (!dragState.moved && Math.abs(e.clientX - dragState.startX) + Math.abs(e.clientY - dragState.startY) < 5) return;
  dragState.moved = true;
  dockDragging.value = true;
  dockPos.value = {
    right: dragState.baseRight + (dragState.startX - e.clientX),
    bottom: dragState.baseBottom + (dragState.startY - e.clientY),
  };
}

function onDockTogglePointerUp(): void {
  if (dragState?.moved) {
    suppressToggleClick = true;
    saveDockPos();
  }
  dragState = null;
  dockDragging.value = false;
}

function onDockToggleClick(): void {
  if (suppressToggleClick) {
    suppressToggleClick = false;
    return;
  }
  dockOpen.value = !dockOpen.value;
}

function onDockResize(): void {
  viewport.value = { w: window.innerWidth, h: window.innerHeight };
}

/** 收起并执行动作：点动作即执行，不必再手动收起 */
function runDockAction(action: () => void): void {
  dockOpen.value = false;
  action();
}

// 外部点击 / Esc 收起：监听常驻（开销可忽略），展开态才生效；
// 触发球与动作列都在 dockEl 内，contains 判定不误收
function onDocPointerDown(e: PointerEvent): void {
  if (dockOpen.value && dockEl.value && !dockEl.value.contains(e.target as Node)) {
    dockOpen.value = false;
  }
}

function onDocKeydown(e: KeyboardEvent): void {
  if (e.key === "Escape" && dockOpen.value) {
    dockOpen.value = false;
  }
}

// ---- 悬浮工具条动作：开始诊断 / 刷新 / 主题切换 ----
// 诊断报告归面板自身状态，必须经面板实例触发（直接调 API 面板不展示结果）；
// running 防连点守卫在面板的 startDiagnostics 内，这里不做禁用绑定。
const diagPanelRef = ref<InstanceType<typeof NetworkDiagnosticsPanel> | null>(null);

function runDiagnostics(): void {
  void diagPanelRef.value?.startDiagnostics();
}

/** 一键刷新看板数据：状态快照 + 登录历史 + 实时日志（均为幂等 GET） */
function refreshDashboard(): void {
  void s.fetchStatus();
  void ui.fetchLoginHistory(true);
  void logs.fetchLogs();
}

// 与顶栏主题钮同语义：快捷切换写显式明/暗，「跟随系统」仍在外观设置页选择
const { appearance, getEffectiveTheme } = useAppearance();
const isDarkTheme = computed(() => getEffectiveTheme() === "dark");

function toggleTheme(): void {
  appearance.theme = isDarkTheme.value ? "light" : "dark";
}

onMounted(() => {
  document.addEventListener("pointerdown", onDocPointerDown);
  document.addEventListener("keydown", onDocKeydown);
  window.addEventListener("resize", onDockResize);
  void ui.fetchLoginHistory();
  void logs.fetchLogs();
  void refreshEnv().then(() => {
    // 后端启动时的环境探测是后台任务，可能晚于本次请求完成：首次取到
    // "未就绪"时延迟复查一次，避免磁盘环境完好却误挂未就绪横幅
    if (envStatus.value && !envStatus.value.capability_ready) {
      envRetryTimer = setTimeout(() => {
        void refreshEnv();
      }, 5000);
    }
  });
});
onBeforeUnmount(() => {
  document.removeEventListener("pointerdown", onDocPointerDown);
  document.removeEventListener("keydown", onDocKeydown);
  window.removeEventListener("resize", onDockResize);
  if (envRetryTimer != null) clearTimeout(envRetryTimer);
});

// ---- 登录历史 — 复用 useUi 共享状态，避免手动登录后 Dashboard 不更新 ----
const loginHistory = ui.loginHistory;
const fetchLoginHistory = ui.fetchLoginHistory;
// 清空登录历史：复用 useUi 带确认逻辑的版本（遮蔽本地无确认实现）
const clearLoginHistory = ui.clearLoginHistory;
const historySourceLabels: Record<string, string> = {
  auto: "自动登录",
  manual: "手动登录",
  login_once: "单次登录",
  browser: "任务执行",
};
const historyChannelLabels: Record<LoginChannel, string> = {
  browser: "浏览器",
  http: "HTTP",
  script: "脚本",
};

// ---- 日志 ----
const logViewer = ref<HTMLElement | null>(null);

function scrollToBottom() {
  logs.autoScroll.value = true;
  // 必须瞬时跳底：平滑滚动的中间帧会触发 onLogScroll 把 autoScroll 翻成 false，
  // 启动窗口内到达的日志会被误计入"新消息"（勿给 .log-viewer 加 scroll-behavior: smooth）
  nextTick(() => {
    if (logViewer.value) logViewer.value.scrollTop = logViewer.value.scrollHeight;
  });
}

// P11：滚动节流——同一帧内的多次 scroll 事件合并为一次（rAF 内读取 scrollTop）
const onLogScroll = throttleRaf(() => {
  if (!logViewer.value) return;
  const { scrollTop, scrollHeight, clientHeight } = logViewer.value;
  const atBottom = scrollTop + clientHeight >= scrollHeight - 40;
  logs.autoScroll.value = atBottom;
  // 回到底部即视为已读：清零滞留的"新消息"计数，避免钉底后按钮残留
  if (atBottom) logs.markAtBottom();
});

// 日志实时追加时自动滚动到底部。
// P13：监听最后一条的 seq（全局单调递增）而非数组长度——缓冲达上限后长度不变、
// 每次 splice 裁剪 push 长度恒定，旧写法会导致 autoScroll 开着也不滚动；
// seq 在缓冲满员后仍持续变化，任何新日志都能触发。
// 旧后端条目缺 seq 时回退监听长度（与原行为一致）。
watch(
  () => logs.logs[logs.logs.length - 1]?.seq ?? logs.logs.length,
  () => {
    if (logs.autoScroll.value) nextTick(scrollToBottom);
  },
);

// ---- 日志筛选 ----
const logLevelOptions: SelectOption[] = [
  { value: "", label: "全部级别" },
  { value: "ERROR", label: "ERROR" },
  { value: "WARN", label: "WARN" },
  { value: "INFO", label: "INFO" },
  { value: "DEBUG", label: "DEBUG" },
  { value: "TRACE", label: "TRACE" },
];
// 来源标签单一来源：constants.LOG_SOURCE_LABELS（筛选选项与徽标展示共用一份映射）
const baseLogSourceOptions: SelectOption[] = [
  { value: "", label: "全部来源" },
  ...Object.entries(LOG_SOURCE_LABELS).map(([value, label]) => ({ value, label })),
];

function getSourceLabel(src: string): string {
  return LOG_SOURCE_LABELS[src] || src;
}

// 后端 target 和前端 scope 会随模块扩展，动态补充当前日志中尚未预置的来源，
// 避免日志已显示但下拉框无法筛选的问题。
const logSourceOptions = computed<SelectOption[]>(() => {
  const options = new Map(baseLogSourceOptions.map((option) => [option.value, option.label]));
  for (const entry of logs.logs) {
    if (entry.source && !options.has(entry.source)) {
      options.set(entry.source, getSourceLabel(entry.source));
    }
  }
  return Array.from(options, ([value, label]) => ({ value, label }));
});

/** 剥离登录消息中的"截图已保存：..."提示（正文展示用），正则与后端消息文案耦合。
 *  注意：useUi.ts 另有针对"截图: /logs/..."格式的独立剥离实现——两套正则对应不同时代的后端文案，合并前先确认消息来源已统一。 */
function stripScreenshotHint(msg: string): string {
  return (msg || "").replace(/截图已保存[：:]\s*[^\s]*/g, "").trim();
}
/** 从消息中提取截图路径（供"查看截图"入口用），与 stripScreenshotHint 同源文案 */
function extractScreenshotUrl(msg: string): string | null {
  const m = (msg || "").match(/截图已保存[：:]\s*([^\s]+)/);
  return m ? m[1] : null;
}
/** 新标签页打开截图；noopener 防止新页面拿到本页引用 */
function openFullscreen(url: string) { window.open(url, "_blank", "noopener,noreferrer"); }
</script>

<template>
  <!-- page-in：消费 motion.css 的 Catmail 页面入场基元（4px / --dur-base）。
       外壳契约化后这是顶层页面入场的唯一出处——layout.css 的结构选择器入场
       （pageEnter）已退役，不再与 .page-in 同特异度竞争。 -->
  <div class="page-content dashboard-page page-in">
    <!-- 主状态行：网络结论 + 主操作（启动/停止检测）。
         启动检测自顶栏迁入首页：它是页面的主行为，放在状态旁语义更近。 -->
    <div class="network-status-banner network-status-hero" :class="s.networkStatus.value">
      <div class="network-status-symbol"><IconApp :name="s.status.monitoring ? 'wifi' : 'pause'" /></div>
      <div class="network-status-copy">
        <strong class="status-text"><span class="status-dot"></span>{{ s.status.monitoring ? s.networkStatusText.value : '自动监测已停止' }}</strong>
        <span v-if="s.status.monitoring" class="status-detail">{{ s.networkStatusDetail.value }}</span>
      </div>
      <div class="hero-actions">
        <button v-if="s.busy.login" class="btn btn-danger" @click="ui.cancelLogin()" title="取消正在执行的登录"><IconApp name="close" />取消登录</button>
        <button
          class="btn"
          :class="s.status.monitoring ? 'btn-secondary' : 'btn-primary'"
          @click="ui.toggleMonitor()"
          :disabled="s.busy.monitor"
          :title="s.status.monitoring ? '停止网络检测和自动登录' : '开始检测网络，断网时自动登录'"
          :aria-label="s.status.monitoring ? '停止检测' : '启动检测'"
        >
          <span v-if="s.busy.monitor" class="spinner" :class="{ 'spinner-on-accent': !s.status.monitoring }"></span>
          <IconApp v-else class="btn-icon" :name="s.status.monitoring ? 'pause' : 'play'" />
          <span class="monitor-action-label">{{ s.busy.monitor ? "处理中..." : (s.status.monitoring ? "停止检测" : "启动检测") }}</span>
        </button>
      </div>
    </div>

    <!-- 环境未就绪提示：点名缺失组件，并就地提供初始化和设置入口。 -->
    <div v-if="showEnvBanner" class="network-status-banner disconnected env-banner">
      <span class="status-dot"></span>
      <span class="status-text" :title="`运行环境未就绪（缺 ${envMissingText}），手动登录时会自动初始化`">
        <span class="env-message-full">运行环境未就绪（缺 {{ envMissingText }}），手动登录时会自动初始化</span>
        <span class="env-message-compact">运行环境缺少{{ envMissingText }}</span>
      </span>
      <button
        class="btn btn-sm env-banner-action"
        :disabled="s.busy.env"
        @click.stop="void initEnvNow()"
        title="自动下载并安装缺失的组件（首次可能耗时数分钟）"
        aria-label="初始化运行环境"
      >
        <IconApp v-if="s.busy.env" name="refresh" class="spin" />
        <span v-if="s.busy.env">初始化中...</span>
        <template v-else><span class="env-action-full">初始化环境</span><span class="env-action-compact">初始化</span></template>
      </button>
      <button class="btn btn-sm btn-link env-banner-detail" @click.stop="router.push({ name: 'settings-tasks' })" aria-label="查看任务与环境设置"><span class="env-action-full">前往设置查看</span><span class="env-action-compact">设置</span></button>
    </div>

    <!-- 统计卡片：stagger-children 让四张卡 40ms 步进级联浮起（motion.css 基元） -->
    <div class="stats-grid stagger-children">
      <div class="stat-card">
        <div class="stat-icon green">
          <IconApp name="clock" />
        </div>
        <div class="stat-info">
          <span class="stat-label">已检测时长</span>
          <span class="stat-value">{{ formatDuration(s.status.monitoring_seconds) }}</span>
        </div>
      </div>
      <div class="stat-card">
        <div class="stat-icon blue">
          <IconApp name="activity" />
        </div>
        <div class="stat-info">
          <span class="stat-label">检测次数</span>
          <span class="stat-value">{{ s.status.network_check_count }}</span>
          <!-- G23：主数值为累计探测次数，连续失败计数降级为副文案展示 -->
          <span v-if="s.status.consecutive_failures > 0" class="stat-sub stat-sub-warn">连续失败 {{ s.status.consecutive_failures }} 次</span>
        </div>
      </div>
      <div class="stat-card">
        <div class="stat-icon orange">
          <IconApp name="alert-triangle" />
        </div>
        <div class="stat-info">
          <span class="stat-label">登录次数</span>
          <span class="stat-value">{{ s.status.login_attempt_count }}</span>
          <!-- G23：主数值为累计登录次数，当前重试进度降级为副文案展示 -->
          <span v-if="s.status.retry_count > 0" class="stat-sub stat-sub-warn">重试中 第 {{ s.status.retry_count }} 次</span>
        </div>
      </div>
      <div class="stat-card">
        <div class="stat-icon purple">
          <IconApp name="calendar" />
        </div>
        <div class="stat-info">
          <span class="stat-label">最后检测</span>
          <span class="stat-value">{{ s.status.last_check_time ? formatShortTime(s.status.last_check_time) : '-' }}</span>
        </div>
      </div>
    </div>

    <!-- 一键网络诊断 + 实时日志双栏行：左诊断右日志，grid 默认 stretch 等高——
         右列日志卡固定高度主导行高，左列诊断卡吃满同行高度（上限钉在同一变量，
         内容超出转卡内滚动），两卡上下缘严格对齐；≤1100px 折回单列
         （见 dashboard.css 的 .dashboard-split） -->
    <div class="dashboard-split">
      <NetworkDiagnosticsPanel ref="diagPanelRef" />

      <!-- 实时日志：排障时最先看的信息流，与诊断卡同行占右列；
           固定高度（行高主导者），内部 .log-viewer 滚动 -->
      <div class="card log-card">
        <div class="card-header">
          <h2>实时日志</h2>
          <div class="flex-row gap-sm">
            <button class="btn btn-icon-only" :class="{ 'btn-active': logs.autoScroll.value }" @click="logs.autoScroll.value = !logs.autoScroll.value" :title="logs.autoScroll.value ? '自动滚动：开' : '自动滚动：关'" :aria-label="logs.autoScroll.value ? '自动滚动：开' : '自动滚动：关'">
              <IconApp :name="logs.autoScroll.value ? 'arrow-up' : 'arrow-down'" />
            </button>
            <button class="btn btn-icon-only" @click="logs.fetchLogs()" title="刷新" aria-label="刷新">
              <IconApp name="refresh" />
            </button>
            <button class="btn btn-icon-only" @click="logs.clearLogs()" title="清空" aria-label="清空">
              <IconApp name="trash" />
            </button>
          </div>
        </div>
        <div class="card-body">
          <div class="log-toolbar">
            <CustomSelect v-model="logs.logFilter.level" :options="logLevelOptions" aria-label="日志级别" compact />
            <CustomSelect v-model="logs.logFilter.source" :options="logSourceOptions" aria-label="日志来源" compact />
            <input v-model="logs.logFilter.search" type="text" placeholder="搜索日志..." class="log-search" aria-label="搜索日志" />
          </div>
          <div ref="logViewer" class="log-viewer" @scroll="onLogScroll">
            <EmptyState v-if="!logs.filteredLogs.value.length">
              <span>{{ logs.logFilter.search || logs.logFilter.level || logs.logFilter.source ? '无匹配日志' : '暂无日志' }}</span>
            </EmptyState>
            <div v-else class="log-entries">
              <div
                v-for="(item, index) in logs.filteredLogs.value"
                :key="item.seq ?? index + '-' + item.timestamp + '-' + item.message"
                class="log-entry"
                :class="[item.level ? 'log-' + item.level.toLowerCase() : '']"
              >
                <span class="log-time">{{ formatTimestamp(item.timestamp) }}</span>
                <span v-if="item.level" class="log-level-badge" :class="'log-' + item.level.toLowerCase()">{{ item.level }}</span>
                <span v-if="item.source" class="log-source-badge" :class="'source-' + item.source">{{ getSourceLabel(item.source) }}</span>
                <div class="log-content">
                  <span class="log-message">{{ stripScreenshotHint(item.message) }}</span>
                  <div v-if="extractScreenshotUrl(item.message)" class="log-screenshot-wrap">
                    <!-- 可点击截图补键盘路径：role=button + Enter/Space 触发全屏 -->
                    <img
                      :src="extractScreenshotUrl(item.message)!"
                      class="log-screenshot-preview"
                      role="button"
                      tabindex="0"
                      alt="截图，回车放大查看"
                      @click="openFullscreen(extractScreenshotUrl(item.message)!)"
                      @keydown.enter.prevent="openFullscreen(extractScreenshotUrl(item.message)!)"
                      @keydown.space.prevent="openFullscreen(extractScreenshotUrl(item.message)!)"
                      loading="lazy"
                    />
                  </div>
                </div>
              </div>
            </div>
            <button v-if="logs.newLogCount.value > 0" class="new-logs-btn" @click="scrollToBottom(); logs.newLogCount.value = 0">
              <IconApp name="arrow-down" />
              {{ logs.newLogCount.value }} 条新消息
            </button>
          </div>
        </div>
      </div>
    </div>
    <!-- /.dashboard-split -->

    <!-- 登录历史：双栏下方全宽卡收尾（列表单一限高滚动，取值见 dashboard.css 的
         .history-list） -->
    <div class="card history-card">
      <div class="card-header">
        <h2>登录历史</h2>
        <div class="history-actions">
          <button class="btn btn-sm btn-icon-only" @click="fetchLoginHistory(true)" title="刷新" aria-label="刷新登录历史">
            <IconApp name="refresh" />
          </button>
          <button class="btn btn-sm btn-icon-only" @click="clearLoginHistory" title="清空" aria-label="清空登录历史" :disabled="!loginHistory.length">
            <IconApp name="trash" />
          </button>
        </div>
      </div>
      <div class="card-body">
        <EmptyState v-if="!loginHistory.length" icon="clock" title="暂无登录记录" description="配置账号后，可立即尝试一次登录">
          <template #actions>
            <button class="btn btn-sm btn-secondary" type="button" @click="router.push({ name: 'profiles' })">去配置账号</button>
          </template>
        </EmptyState>
        <div v-else class="history-list">
          <div v-for="(item, idx) in loginHistory" :key="idx" class="history-item" :class="item.result === 'success' ? 'success' : 'failed'">
            <div class="history-status">
              <IconApp name="check" v-if="item.result === 'success'" />
              <IconApp name="x-circle" v-else />
            </div>
            <div class="history-info">
              <div class="history-row">
                <span class="history-time">{{ formatTimestamp(item.timestamp) }}</span>
                <span class="history-duration">{{ item.duration_secs.toFixed(1) }}s</span>
              </div>
              <div class="history-row history-row--meta">
                <span class="history-profile">{{ item.profile_id || '默认方案' }} · {{ historySourceLabels[item.source] || item.source }}</span>
                <span v-if="item.channel" class="badge badge--sm" :class="{ 'badge-browser': item.channel === 'browser', 'badge--info': item.channel === 'http', 'badge-script': item.channel === 'script' }">{{ historyChannelLabels[item.channel] }}</span>
              </div>
              <span v-if="item.result !== 'success'" class="history-error">{{ item.message }}</span>
            </div>
          </div>
        </div>
      </div>
    </div>

    <!-- 悬浮工具条（speed dial）：默认收起为主色闪电球，点开自球体上方展开一条
         连体展开条（48px 动作钮与球同尺寸、细分隔线分行，悬停左侧浮出说明），
         点动作直接执行并收起；外部点击 / Esc 收起。fixed 定位不随内容滚动。
         球体可自由拖动重定位（localStorage 记忆，右/下偏移锚定），拖到顶部区域
         展开条自动改为向下弹出。 -->
    <div
      ref="dockEl"
      class="dash-dock"
      :class="{ open: dockOpen, 'expand-down': !expandUp, dragging: dockDragging }"
      :style="dockStyle"
    >
      <div class="dash-dock-actions" role="toolbar" aria-label="快捷操作" :aria-hidden="!dockOpen">
        <button
          type="button"
          class="dash-dock-btn"
          :data-tip="s.status.monitoring ? '停止监测' : '开始监测'"
          :aria-label="s.status.monitoring ? '停止监测' : '开始监测'"
          :tabindex="dockOpen ? 0 : -1"
          :disabled="s.busy.monitor"
          @click="runDockAction(() => void ui.toggleMonitor())"
        >
          <IconApp :name="s.status.monitoring ? 'pause' : 'play'" />
        </button>
        <button
          type="button"
          class="dash-dock-btn"
          data-tip="手动登录"
          aria-label="手动登录"
          :tabindex="dockOpen ? 0 : -1"
          :disabled="s.busy.action || s.busy.loginCooldown || s.busy.login"
          @click="runDockAction(() => void ui.manualLogin())"
        >
          <IconApp name="log-in" />
        </button>
        <button
          type="button"
          class="dash-dock-btn"
          data-tip="网络测试"
          aria-label="网络测试"
          :tabindex="dockOpen ? 0 : -1"
          :disabled="s.busy.action"
          @click="runDockAction(() => void ui.testNetwork())"
        >
          <IconApp name="wifi" />
        </button>
        <button
          type="button"
          class="dash-dock-btn"
          data-tip="开始诊断"
          aria-label="开始诊断"
          :tabindex="dockOpen ? 0 : -1"
          @click="runDockAction(() => runDiagnostics())"
        >
          <IconApp name="activity" />
        </button>
        <button
          type="button"
          class="dash-dock-btn"
          data-tip="刷新数据"
          aria-label="刷新数据"
          :tabindex="dockOpen ? 0 : -1"
          @click="runDockAction(() => refreshDashboard())"
        >
          <IconApp name="refresh" />
        </button>
        <button
          type="button"
          class="dash-dock-btn"
          :data-tip="isDarkTheme ? '切换浅色主题' : '切换深色主题'"
          :aria-label="isDarkTheme ? '切换浅色主题' : '切换深色主题'"
          :tabindex="dockOpen ? 0 : -1"
          @click="runDockAction(() => toggleTheme())"
        >
          <IconApp :name="isDarkTheme ? 'sun' : 'moon'" />
        </button>
      </div>
      <button
        type="button"
        class="dash-dock-toggle"
        data-tip="快捷操作"
        :aria-expanded="dockOpen"
        aria-label="快捷操作：手动登录、网络测试"
        @pointerdown="onDockTogglePointerDown"
        @pointermove="onDockTogglePointerMove"
        @pointerup="onDockTogglePointerUp"
        @pointercancel="onDockTogglePointerUp"
        @click="onDockToggleClick"
      >
        <IconApp :name="dockOpen ? 'close' : 'zap'" />
      </button>
    </div>
  </div>
</template>

<style scoped>
/* 悬浮工具条容器：竖排右对齐，展开条在球体上方展开。
   right/bottom 由内联样式提供（拖动 + localStorage 记忆），此处不写死 */
.dash-dock {
  position: fixed;
  display: flex;
  flex-direction: column;
  align-items: flex-end;
  gap: 6px;
  z-index: var(--z-dropdown);
  user-select: none;
}

/* 球体被拖到顶部区域时改为向下展开：column-reverse 让球位居条上方 */
.dash-dock.expand-down {
  flex-direction: column-reverse;
}

/* 展开条：与触发球同宽的连体胶囊，动作钮以细分隔线分行（不是散浮的独立按钮）；
   默认隐藏（pointer-events 与 tab 焦点双保险），open 时自球体方向展开缩放上浮，
   transform 原点取右下 = 球体所在角。注意不能 overflow:hidden——说明标签定位在
   按钮外侧，会被容器裁掉；首末钮自担圆角即可 */
.dash-dock-actions {
  display: flex;
  flex-direction: column;
  opacity: 0;
  pointer-events: none;
  border: 1px solid var(--border);
  border-radius: var(--radius-lg);
  background: var(--bg-modal);
  box-shadow: var(--shadow-float);
  transform: translateY(10px) scale(0.94);
  transform-origin: calc(100% - 24px) 100%;
  transition: opacity var(--dur-base) var(--ease-out), transform var(--dur-base) var(--ease-out);
}

/* 向下展开的位移/原点覆盖排在基础规则之后（no-descending-specificity） */
.dash-dock.expand-down .dash-dock-actions {
  transform: translateY(-10px) scale(0.94);
  transform-origin: calc(100% - 24px) 0;
}

.dash-dock.open .dash-dock-actions {
  opacity: 1;
  pointer-events: auto;
  transform: none;
}

/* 动作钮：48px 与触发球同尺寸；边框与底色由展开条统一承担，行间用细分隔线；
   图标用主色，悬停浮浅底（首末钮圆角跟随容器，悬停底色不冒出胶囊外） */
.dash-dock-btn {
  position: relative;
  display: inline-flex;
  align-items: center;
  justify-content: center;
  width: 48px;
  height: 48px;
  padding: 0;
  border: 0;
  border-radius: 0;
  background: transparent;
  color: var(--accent);
  cursor: pointer;
  transition: background var(--dur-fast) var(--ease-out);
}

.dash-dock-btn:first-child {
  border-radius: var(--radius-lg) var(--radius-lg) 0 0;
}

.dash-dock-btn:last-child {
  border-radius: 0 0 var(--radius-lg) var(--radius-lg);
}

.dash-dock-btn + .dash-dock-btn {
  border-top: 1px solid var(--border);
}

.dash-dock-btn svg {
  width: 20px;
  height: 20px;
}

.dash-dock-btn:disabled {
  opacity: 0.5;
  cursor: not-allowed;
}

.dash-dock-btn:hover:not(:disabled) {
  background: var(--bg-hover);
}

/* 说明标签：悬停 / 键盘聚焦时在按钮左侧浮出（黑底白字在深色主题下不可读，
   用弹层面色 + 边框保证双主题可读）；触发球同样生效 */
.dash-dock-btn::after,
.dash-dock-toggle::after {
  content: attr(data-tip);
  position: absolute;
  right: calc(100% + 10px);
  top: 50%;
  translate: 0 -50%;
  padding: 4px 10px;
  border: 1px solid var(--border);
  border-radius: var(--radius-sm);
  background: var(--bg-modal);
  color: var(--text-primary);
  font-size: var(--text-xs);
  line-height: 1.5;
  white-space: nowrap;
  opacity: 0;
  pointer-events: none;
  transition: opacity var(--dur-fast) var(--ease-out);
}

.dash-dock-btn:hover::after,
.dash-dock-btn:focus-visible::after,
.dash-dock-toggle:hover::after,
.dash-dock-toggle:focus-visible::after {
  opacity: 1;
}

/* 触发球：48px 主色圆钮（与「启动检测」黑钮同一视觉语言），展开后图标换 ✕；
   position:relative 让说明标签锚到球自身而非整个 dock 容器。
   可拖动：grab 光标提示可拖，触摸屏需禁掉浏览器滚动手势（touch-action:none）
   否则拖动会变成滚动页面 */
.dash-dock-toggle {
  position: relative;
  display: inline-flex;
  align-items: center;
  justify-content: center;
  width: 48px;
  height: 48px;
  padding: 0;
  border: 1px solid transparent;
  border-radius: var(--radius-full);
  background: var(--accent);
  color: var(--on-accent);
  cursor: grab;
  touch-action: none;
  box-shadow: 0 6px 20px rgba(0, 0, 0, 0.25);
  transition: transform var(--dur-fast) var(--ease-out), box-shadow var(--dur-fast) var(--ease-out), background var(--dur-fast) var(--ease-out);
}

.dash-dock.dragging .dash-dock-toggle {
  cursor: grabbing;
}

.dash-dock-toggle svg {
  width: 20px;
  height: 20px;
  transition: transform var(--dur-base) var(--ease-out);
}

/* 展开时轻微放大 + 收起图标，暗示进入"待命"态 */
.dash-dock.open .dash-dock-toggle {
  background: var(--accent-hover);
  transform: scale(1.06);
  box-shadow: 0 12px 28px rgba(0, 0, 0, 0.3);
}
</style>
