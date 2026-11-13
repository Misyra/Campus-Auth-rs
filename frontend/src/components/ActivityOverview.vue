<script setup lang="ts">
/**
 * 活动概览卡片（Dashboard 内嵌）：消费 GET /api/events 的近 N 天事件流，
 * 绘制三块轻量视图——
 *
 * 1. 登录成败按日堆叠柱状图（SVG，无文本受 preserveAspectRatio 拉伸影响，
 *    日期标签由等宽 HTML 行对齐）；
 * 2. 网络状态翻转时间线（分段条带：通 / 断 / 未知）；
 * 3. 事件类型分布（横向条形排行）。
 *
 * 选型说明：echarts 按需引入实测 gzip 182KB，超出 50KB 体积门禁（见 T8-6a
 * 汇报），故回落为自绘 SVG/CSS 方案——零依赖，且颜色全部走 CSS token 类
 * （fill/stroke 引用 var(--xxx)），深浅主题由变量定义自动生效，**无需监听
 * 主题变化重绘**（echarts 方案才需要取值注入 + 重绘）。
 *
 * 空态 / 加载态 / 错误态齐备；days 切换（7/14/30，上限与后端保留期一致）
 * 即重新拉取。事件载荷已在后端事件类型层脱敏（白名单式摘要），前端原样展示。
 */
import { computed, onMounted, ref, watch } from "vue";
import IconApp from "@/components/common/IconApp.vue";
import { eventsApi, extractApiError } from "@/api";
import type { AppEventType, EventsResponse } from "@/api/types";
import {
  eventTypeDistribution,
  loginDailySeries,
  networkFlipSegments,
  type LoginDailyCount,
  type NetworkSegment,
  type TypeCount,
} from "@/utils/activityStats";

/** 可选回看窗口（30 = 后端事件流固定保留期） */
const DAY_OPTIONS = [7, 14, 30] as const;

/** 事件类型中文名（分布图行标签；未知类型回退原始标识） */
const TYPE_LABELS: Record<AppEventType, string> = {
  login_succeeded: "登录成功",
  login_failed: "登录失败",
  network_down: "网络断开",
  network_up: "网络恢复",
  task_finished: "任务完成",
  task_failed: "任务失败",
  update_available: "发现更新",
  environment_ready: "环境就绪",
  profile_switched: "方案切换",
};

const loading = ref(false);
const errorText = ref("");
const data = ref<EventsResponse | null>(null);
const days = ref<number>(7);

/** 拉取事件流；days 切换与手动刷新共用（防连点：loading 期间早退） */
async function fetchEvents(): Promise<void> {
  if (loading.value) return;
  loading.value = true;
  errorText.value = "";
  try {
    data.value = await eventsApi.fetch(days.value);
  } catch (e) {
    errorText.value = extractApiError(e, "活动数据加载失败，请稍后重试");
  } finally {
    loading.value = false;
  }
}

watch(days, () => {
  void fetchEvents();
});
onMounted(() => {
  void fetchEvents();
});

// ---- 图表数据（纯函数聚合，见 utils/activityStats.ts）----

const dailySeries = computed<LoginDailyCount[]>(() =>
  loginDailySeries(data.value?.events ?? [], days.value),
);

/** 柱状图几何（viewBox 300x100，柱宽按天数均分再取 62%） */
const barGeom = computed(() => {
  const W = 300;
  const H = 100;
  const n = Math.max(dailySeries.value.length, 1);
  const unit = W / n;
  const barWidth = unit * 0.62;
  const maxTotal = Math.max(...dailySeries.value.map((d) => d.success + d.failed), 1);
  const bars = dailySeries.value.map((d, i) => {
    const x = i * unit + (unit - barWidth) / 2;
    const successH = (d.success / maxTotal) * H;
    const failedH = (d.failed / maxTotal) * H;
    return {
      date: d.date,
      label: d.date.slice(5),
      x,
      barWidth,
      // 成功段在下（贴底），失败段叠在其上；零计数不出柱
      successY: H - successH,
      successH,
      failedY: H - successH - failedH,
      failedH,
      total: d.success + d.failed,
      success: d.success,
      failed: d.failed,
    };
  });
  return { W, H, maxTotal, bars };
});

const segments = computed<NetworkSegment[]>(() =>
  networkFlipSegments(data.value?.events ?? [], days.value),
);

/** 窗口内网络状态翻转次数（up/down 事件数，即状态变更次数） */
const flipCount = computed(
  () =>
    (data.value?.events ?? []).filter(
      (e) => e.type === "network_up" || e.type === "network_down",
    ).length,
);

const SEGMENT_TITLE: Record<NetworkSegment["state"], string> = {
  up: "网络连通",
  down: "网络断开",
  unknown: "无观测（首个翻转事件之前）",
};

const typeRows = computed<TypeCount[]>(() => eventTypeDistribution(data.value?.events ?? []));

/** 分布条宽百分比（以最大计数为基准） */
function typeBarWidth(count: number): string {
  const max = typeRows.value[0]?.count ?? 1;
  return `${Math.max((count / max) * 100, 2)}%`;
}

/** 30 天窗口下隔日显示日期标签，避免拥挤（空占位保持等宽对齐） */
const labelStep = computed(() => (days.value >= 30 ? 2 : 1));

const empty = computed(() => !loading.value && !errorText.value && data.value != null && data.value.total === 0);
const hasData = computed(() => !loading.value && !errorText.value && data.value != null && data.value.total > 0);
</script>

<template>
  <div class="card activity-card">
    <div class="card-header">
      <h2>活动概览</h2>
      <div class="activity-actions">
        <div class="segmented" role="group" aria-label="回看天数">
          <button
            v-for="option in DAY_OPTIONS"
            :key="option"
            type="button"
            :class="{ active: days === option }"
            :data-testid="`activity-days-${option}`"
            :aria-pressed="days === option"
            @click="days = option"
          >
            {{ option }}天
          </button>
        </div>
        <button
          class="btn btn-sm btn-icon-only"
          data-testid="activity-refresh"
          title="刷新活动数据"
          aria-label="刷新活动数据"
          :disabled="loading"
          @click="fetchEvents"
        >
          <IconApp name="refresh" :class="{ spin: loading }" />
        </button>
      </div>
    </div>
    <div class="card-body activity-body">
      <!-- 加载态 -->
      <div v-if="loading" class="activity-loading" data-testid="activity-loading">
        <IconApp name="refresh" class="spin" />
        <span>加载中...</span>
      </div>

      <!-- 错误态 -->
      <div v-else-if="errorText" class="activity-error" role="alert" data-testid="activity-error">
        <span>{{ errorText }}</span>
        <button class="btn btn-sm btn-secondary" data-testid="activity-retry" @click="fetchEvents">重试</button>
      </div>

      <!-- 空态 -->
      <div v-else-if="empty" class="empty-state" data-testid="activity-empty">
        <IconApp name="activity" />
        <span>近 {{ days }} 天暂无事件</span>
        <span class="empty-desc">登录、网络翻转与任务事件会记录在这里</span>
      </div>

      <!-- 数据视图 -->
      <template v-else-if="hasData">
        <div v-if="data?.truncated" class="activity-truncated">
          数据量较大，部分更早事件未计入（单文件行数上限）
        </div>

        <!-- 登录成败按日堆叠柱状图 -->
        <section class="activity-section">
          <div class="activity-section-head">
            <h3>登录成败（按日）</h3>
            <div class="activity-legend">
              <span class="activity-dot activity-dot--success"></span>成功
              <span class="activity-dot activity-dot--failed"></span>失败
            </div>
          </div>
          <svg
            class="activity-bars"
            :viewBox="`0 0 ${barGeom.W} ${barGeom.H}`"
            preserveAspectRatio="none"
            role="img"
            aria-label="近N天登录成功与失败按日堆叠柱状图"
            data-testid="activity-bars"
          >
            <template v-for="bar in barGeom.bars" :key="bar.date">
              <rect
                v-if="bar.successH > 0"
                class="activity-bar activity-bar--success"
                :x="bar.x"
                :y="bar.successY"
                :width="bar.barWidth"
                :height="bar.successH"
                rx="1.5"
              >
                <title>{{ bar.date }}：成功 {{ bar.success }} 次</title>
              </rect>
              <rect
                v-if="bar.failedH > 0"
                class="activity-bar activity-bar--failed"
                :x="bar.x"
                :y="bar.failedY"
                :width="bar.barWidth"
                :height="bar.failedH"
                rx="1.5"
              >
                <title>{{ bar.date }}：失败 {{ bar.failed }} 次</title>
              </rect>
            </template>
          </svg>
          <div class="activity-day-labels" aria-hidden="true">
            <span v-for="(bar, i) in barGeom.bars" :key="bar.date">{{ i % labelStep === 0 ? bar.label : "" }}</span>
          </div>
          <p class="activity-scale-hint">峰值 {{ barGeom.maxTotal }} 次/日</p>
        </section>

        <!-- 网络状态翻转时间线 -->
        <section class="activity-section">
          <div class="activity-section-head">
            <h3>网络状态时间线</h3>
            <span class="activity-flip-count">窗口内翻转 {{ flipCount }} 次</span>
          </div>
          <div class="activity-timeline" data-testid="activity-timeline">
            <div
              v-for="(seg, i) in segments"
              :key="i"
              class="activity-seg"
              :class="`activity-seg--${seg.state}`"
              :style="{ width: `${(seg.end - seg.start) * 100}%` }"
              :title="SEGMENT_TITLE[seg.state]"
            ></div>
          </div>
          <div class="activity-legend activity-legend--timeline">
            <span class="activity-dot activity-dot--up"></span>连通
            <span class="activity-dot activity-dot--down"></span>断开
            <span class="activity-dot activity-dot--unknown"></span>无观测
          </div>
        </section>

        <!-- 事件类型分布 -->
        <section class="activity-section">
          <h3>事件类型分布</h3>
          <div class="activity-types" data-testid="activity-types">
            <div v-for="row in typeRows" :key="row.type" class="activity-type-row">
              <span class="activity-type-label">{{ TYPE_LABELS[row.type] || row.type }}</span>
              <div class="activity-type-track">
                <div class="activity-type-bar" :style="{ width: typeBarWidth(row.count) }" :title="`${TYPE_LABELS[row.type] || row.type}：${row.count} 次`"></div>
              </div>
              <span class="activity-type-count">{{ row.count }}</span>
            </div>
          </div>
        </section>
      </template>
    </div>
  </div>
</template>

<style scoped>
/* 全部走 token，无 [data-theme] 分支：深浅主题由变量定义自动生效（含 SVG fill） */
.activity-actions {
  display: flex;
  align-items: center;
  gap: var(--space-sm);
}

.activity-loading {
  display: flex;
  align-items: center;
  justify-content: center;
  gap: var(--space-sm);
  padding: var(--space-lg) 0;
  color: var(--text-muted);
  font-size: var(--text-sm);
}

.activity-error {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: var(--space-sm);
  padding: var(--space-sm) var(--space-md);
  border: 1px solid var(--danger-border);
  border-radius: var(--radius-md);
  background: var(--danger-bg);
  color: var(--danger);
  font-size: var(--text-sm);
}

.activity-truncated {
  margin-bottom: var(--space-sm);
  padding: var(--space-xs) var(--space-sm);
  border-radius: var(--radius-sm);
  background: rgba(var(--warning-rgb), 0.12);
  color: var(--warning-text);
  font-size: var(--text-xs);
}

.activity-section {
  padding: var(--space-sm) 0;
}

.activity-section + .activity-section {
  border-top: 1px solid var(--border);
}

.activity-section h3 {
  margin: 0;
  font-size: var(--text-sm);
  font-weight: 600;
  color: var(--text-primary);
}

.activity-section-head {
  display: flex;
  align-items: baseline;
  justify-content: space-between;
  gap: var(--space-sm);
  margin-bottom: var(--space-sm);
}

/* 图例圆点（HTML 与 SVG 共用配色来源：token 类） */
.activity-legend {
  display: flex;
  align-items: center;
  gap: var(--space-xs);
  color: var(--text-muted);
  font-size: var(--text-xs);
}

.activity-dot {
  width: 8px;
  height: 8px;
  border-radius: 50%;
  display: inline-block;
}

.activity-dot--success,
.activity-dot--up {
  background: var(--success);
}

.activity-dot--failed,
.activity-dot--down {
  background: var(--danger);
}

.activity-dot--unknown {
  background: var(--border-hover);
}

/* 堆叠柱状图：SVG 只画矩形（preserveAspectRatio=none 拉伸不变形），文字走 HTML */
.activity-bars {
  display: block;
  width: 100%;
  height: 88px;
}

.activity-bar--success {
  fill: var(--success);
}

.activity-bar--failed {
  fill: var(--danger);
}

.activity-day-labels {
  display: flex;
  margin-top: 4px;
  color: var(--text-muted);
  font-size: 10px;
  font-variant-numeric: tabular-nums;
}

.activity-day-labels span {
  flex: 1;
  text-align: center;
  overflow: hidden;
  white-space: nowrap;
}

.activity-scale-hint {
  margin: 4px 0 0;
  color: var(--text-muted);
  font-size: var(--text-xs);
}

/* 网络状态时间线：分段条带 */
.activity-timeline {
  display: flex;
  height: 14px;
  border-radius: var(--radius-sm);
  overflow: hidden;
  border: 1px solid var(--border);
}

.activity-seg--up {
  background: var(--success);
}

.activity-seg--down {
  background: var(--danger);
}

.activity-seg--unknown {
  background: var(--border-hover);
}

.activity-legend--timeline {
  margin-top: var(--space-xs);
}

.activity-flip-count {
  color: var(--text-muted);
  font-size: var(--text-xs);
}

/* 事件类型分布：横向条形排行 */
.activity-types {
  display: flex;
  flex-direction: column;
  gap: 6px;
  margin-top: var(--space-sm);
}

.activity-type-row {
  display: grid;
  grid-template-columns: 72px minmax(0, 1fr) auto;
  align-items: center;
  gap: var(--space-sm);
}

.activity-type-label {
  color: var(--text-secondary);
  font-size: var(--text-xs);
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}

.activity-type-track {
  height: 10px;
  border-radius: var(--radius-sm);
  background: var(--bg-glass-light);
  overflow: hidden;
}

.activity-type-bar {
  height: 100%;
  border-radius: var(--radius-sm);
  background: var(--accent);
  transition: width var(--dur-base) var(--ease-out);
}

.activity-type-count {
  color: var(--text-secondary);
  font-size: var(--text-xs);
  font-variant-numeric: tabular-nums;
  min-width: 2em;
  text-align: right;
}
</style>
