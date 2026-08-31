<script setup lang="ts">
import IconApp from "@/components/common/IconApp.vue";
import { ref, computed, onMounted, nextTick, watch } from "vue";
import { useRouter } from "vue-router";
import { useStatus } from "@/composables/useStatus";
import { useEnvironment } from "@/composables/useEnvironment";
import { useLogs } from "@/composables/useLogs";
import { useUi } from "@/composables/useUi";
import { throttleRaf } from "@/utils/debounce";
import { LOG_SOURCE_LABELS } from "@/utils/constants";
import { formatDuration, formatTimestamp, formatShortTime } from "@/utils/formatters";
import CustomSelect from "@/components/common/CustomSelect.vue";
import type { SelectOption } from "@/components/common/CustomSelect.vue";

const s = useStatus();
const logs = useLogs();
const ui = useUi();
const router = useRouter();
const { envStatus, refreshEnv } = useEnvironment();
const showEnvBanner = computed(() => envStatus.value != null && !envStatus.value.capability_ready);

onMounted(() => {
  void ui.fetchLoginHistory();
  void logs.fetchLogs();
  void refreshEnv();
});

// ---- 登录历史 — 复用 useUi 共享状态，避免手动登录后 Dashboard 不更新 ----
const loginHistory = ui.loginHistory;
const fetchLoginHistory = ui.fetchLoginHistory;
// 清空登录历史：复用 useUi 带确认逻辑的版本（遮蔽本地无确认实现）
const clearLoginHistory = ui.clearLoginHistory;

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

function stripScreenshotHint(msg: string): string {
  return (msg || "").replace(/截图已保存[：:]\s*[^\s]*/g, "").trim();
}
function extractScreenshotUrl(msg: string): string | null {
  const m = (msg || "").match(/截图已保存[：:]\s*([^\s]+)/);
  return m ? m[1] : null;
}
function openFullscreen(url: string) { window.open(url, "_blank"); }
</script>

<template>
  <div class="page-content" :class="{ 'has-banner': s.status.monitoring }">
    <!-- 网络状态横幅 -->
    <div v-if="s.status.monitoring" class="network-status-banner" :class="s.networkStatus.value">
      <span class="status-dot"></span>
      <span>{{ s.networkStatusText.value }}</span>
    </div>

    <!-- 环境未就绪提示（非阻塞，仅链接到 系统 → Python 环境） -->
    <div v-if="showEnvBanner" class="network-status-banner disconnected" style="cursor:pointer" @click="router.push({ name: 'settings-system' })">
      <span class="status-dot"></span>
      <span>Python 环境未就绪，手动登录将自动初始化或可前往 设置 · 系统 手动修复</span>
    </div>

    <!-- 统计卡片 -->
    <div class="stats-grid">
      <div class="stat-card">
        <div class="stat-icon green">
          <IconApp name="clock" />
        </div>
        <div class="stat-info">
          <span class="stat-label">开始监控时长</span>
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

    <div class="dashboard-grid">
      <div class="dashboard-left">
        <!-- 快捷操作 -->
        <div class="card">
          <div class="card-header"><h2>快捷操作</h2></div>
          <div class="card-body">
            <div class="action-buttons">
              <button v-if="s.busy.login" class="btn btn-danger" @click="ui.cancelLogin()" title="取消正在执行的登录">
                <IconApp name="close" />
                取消登录
              </button>
              <button v-else class="btn btn-secondary" @click="ui.manualLogin()" :disabled="s.busy.action || s.busy.loginCooldown" title="立即执行一次登录认证">
                <IconApp name="log-in" />
                手动登录
              </button>
              <button class="btn btn-secondary" @click="ui.testNetwork()" :disabled="s.busy.action">
                <IconApp name="wifi" />
                网络测试
              </button>
            </div>
          </div>
        </div>

        <!-- 登录历史 -->
        <div class="card history-card">
          <div class="card-header">
            <h2>登录历史</h2>
            <div class="flex-row gap-sm">
              <button class="btn btn-icon-only" @click="fetchLoginHistory" title="刷新">
                <IconApp name="download" />
              </button>
              <button class="btn btn-icon-only" @click="clearLoginHistory" title="清空" :disabled="!loginHistory.length">
                <IconApp name="trash" />
              </button>
            </div>
          </div>
          <div class="card-body">
            <div v-if="!loginHistory.length" class="empty-state">
              <IconApp name="clock" />
              <span>暂无登录记录</span>
            </div>
            <div v-else class="history-list">
              <div v-for="(item, idx) in loginHistory" :key="idx" class="history-item" :class="item.result === 'success' ? 'success' : 'failed'">
                <div class="history-status">
                  <IconApp name="check" v-if="item.result === 'success'" />
                  <IconApp name="x-circle" />
                </div>
                <div class="history-info">
                  <div class="history-row">
                    <span class="history-time">{{ item.timestamp }}</span>
                    <span class="history-duration">{{ item.duration_secs.toFixed(1) }}s</span>
                  </div>
                  <div class="history-row">
                    <span class="history-profile">{{ item.profile_id || '默认方案' }} · {{ item.source }}</span>
                  </div>
                  <span v-if="item.result !== 'success'" class="history-error">{{ item.message }}</span>
                </div>
              </div>
            </div>
          </div>
        </div>
      </div>

      <!-- 实时日志 -->
      <div class="card log-card">
        <div class="card-header">
          <h2>实时日志</h2>
          <div class="flex-row gap-sm">
            <button class="btn btn-icon-only" :class="{ 'btn-active': logs.autoScroll.value }" @click="logs.autoScroll.value = !logs.autoScroll.value" :title="logs.autoScroll.value ? '自动滚动：开' : '自动滚动：关'">
              <IconApp :name="logs.autoScroll.value ? 'arrow-up' : 'arrow-down'" />
            </button>
            <button class="btn btn-icon-only" @click="logs.fetchLogs()" title="刷新">
              <IconApp name="download" />
            </button>
            <button class="btn btn-icon-only" @click="logs.clearLogs()" title="清空">
              <IconApp name="trash" />
            </button>
          </div>
        </div>
        <div class="card-body">
          <div class="log-toolbar">
            <CustomSelect v-model="logs.logFilter.level" :options="logLevelOptions" compact />
            <CustomSelect v-model="logs.logFilter.source" :options="logSourceOptions" compact />
            <input v-model="logs.logFilter.search" type="text" placeholder="搜索日志..." class="log-search" />
          </div>
          <div ref="logViewer" class="log-viewer" @scroll="onLogScroll">
            <div v-if="!logs.filteredLogs.value.length" class="empty-state">
              <span>{{ logs.logFilter.search || logs.logFilter.level || logs.logFilter.source ? '无匹配日志' : '暂无日志' }}</span>
            </div>
            <div v-else class="log-entries">
              <div
                v-for="(item, index) in logs.filteredLogs.value"
                :key="item.seq ?? index + '-' + item.timestamp + '-' + item.message"
                class="log-entry"
                :class="[item.level ? 'log-' + item.level.toLowerCase() : '']"
              >
                <span class="log-time">{{ formatTimestamp(item.timestamp) }}</span>
                <span v-if="item.level" class="log-level-badge" :class="'level-' + item.level.toLowerCase()">{{ item.level }}</span>
                <span v-if="item.source" class="log-source-badge" :class="'source-' + item.source">{{ getSourceLabel(item.source) }}</span>
                <div class="log-content">
                  <span class="log-message">{{ stripScreenshotHint(item.message) }}</span>
                  <div v-if="extractScreenshotUrl(item.message)" class="log-screenshot-wrap">
                    <img :src="extractScreenshotUrl(item.message)!" class="log-screenshot-preview" @click="openFullscreen(extractScreenshotUrl(item.message)!)" loading="lazy" alt="截图" />
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
  </div>
</template>
