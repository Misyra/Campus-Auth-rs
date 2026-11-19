<script setup lang="ts">
/**
 * 网络诊断面板（Dashboard 内嵌卡片）。
 *
 * 一键触发 POST /api/diagnostics/run：后端五项并发探测（网卡 / 网关 / DNS /
 * 门户 / 近期登录失败归因），整体受 15s 总限时约束，前端超时放宽到 30s 兜底。
 * 诊断失败是数据不是请求错误：逐项 ✓/⚠/✗/– + 耗时 + 摘要，细节可折叠，
 * 总结论高亮。样式全部走 token，深浅主题由 base.css 的变量定义自动生效。
 *
 * 「导出诊断包」与「开始诊断」并排：GET /api/diagnostics/bundle 返回 zip
 * 附件（manifest + 脱敏配置快照 + 事件流 + 日志 + 环境观测），blob 下载 +
 * a.download 命名，导出失败就地报错（与诊断错误互不覆盖）。
 */
import { ref } from "vue";
import IconApp from "@/components/common/IconApp.vue";
import type { IconName } from "@/components/common/IconApp.vue";
import { diagnosticsApi, extractApiError } from "@/api";
import type { DiagnosticCheck, DiagnosticReport, DiagnosticStatus } from "@/api/types";

const running = ref(false);
const report = ref<DiagnosticReport | null>(null);
const errorText = ref("");
/** 导出诊断包进行中（防连点）与失败信息 */
const exporting = ref(false);
const exportError = ref("");
/** 已展开细节的检查项 key 集合 */
const expandedKeys = ref<Set<string>>(new Set());

/** 总结论展示元数据 */
const VERDICT_META: Record<string, { label: string; cls: string }> = {
  healthy: { label: "链路正常", cls: "diag-verdict--healthy" },
  degraded: { label: "存在警告", cls: "diag-verdict--degraded" },
  faulty: { label: "发现异常", cls: "diag-verdict--faulty" },
};

/** 单项结论展示元数据（图标 / 标签 / 着色类） */
const STATUS_META: Record<DiagnosticStatus, { icon: IconName; label: string; cls: string }> = {
  ok: { icon: "check-circle", label: "正常", cls: "diag-status--ok" },
  warn: { icon: "alert-triangle", label: "警告", cls: "diag-status--warn" },
  fail: { icon: "x-circle", label: "失败", cls: "diag-status--fail" },
  skip: { icon: "info", label: "跳过", cls: "diag-status--skip" },
};

/** 细节字段中文名（details 的 key → 展示标签；未收录的 key 原样展示） */
const DETAIL_LABELS: Record<string, string> = {
  count: "数量",
  adapters: "网卡",
  name: "名称",
  ipv4: "IPv4",
  dns_servers: "DNS",
  gateways: "网关",
  probes: "探测明细",
  target: "目标",
  success: "连通",
  elapsed_ms: "耗时",
  error: "错误",
  host: "主机",
  port: "端口",
  ips: "解析地址",
  reason: "原因",
  url: "探测地址",
  http_status: "HTTP 状态",
  kind: "失败类别",
  url_source: "地址来源",
  window_days: "回看天数",
  failure_count: "失败次数",
  categories: "原因分类",
  latest: "最新一次",
  timestamp: "时间",
  message: "消息",
  category: "归类",
  channel: "渠道",
  ip_literal: "IP 直连",
  total_timeout: "总限时收敛",
};

/** 细节值文本化（数组并列、对象以 键=值 连写，保持稳定顺序） */
function detailValue(value: unknown): string {
  if (value == null) return "—";
  if (typeof value === "boolean") return value ? "是" : "否";
  if (Array.isArray(value)) return value.map(detailValue).join("、");
  if (typeof value === "object") {
    return Object.entries(value as Record<string, unknown>)
      .map(([k, v]) => `${DETAIL_LABELS[k] ?? k}=${detailValue(v)}`)
      .join(" ");
  }
  return String(value);
}

/** 细节展开为逐行文本（顶层对象逐键一行） */
function detailLines(details: unknown): string[] {
  if (details == null || typeof details !== "object") {
    return details == null ? [] : [String(details)];
  }
  return Object.entries(details as Record<string, unknown>).map(
    ([k, v]) => `${DETAIL_LABELS[k] ?? k}: ${detailValue(v)}`,
  );
}

function hasDetail(check: DiagnosticCheck): boolean {
  return detailLines(check.details).length > 0;
}

function isExpanded(key: string): boolean {
  return expandedKeys.value.has(key);
}

function toggleDetail(key: string): void {
  const next = new Set(expandedKeys.value);
  if (next.has(key)) next.delete(key);
  else next.add(key);
  expandedKeys.value = next;
}

/** 开始诊断：防连点（running 期间按钮禁用 + 早退双保险） */
async function startDiagnostics(): Promise<void> {
  if (running.value) return;
  running.value = true;
  errorText.value = "";
  report.value = null;
  expandedKeys.value = new Set();
  try {
    report.value = await diagnosticsApi.run();
  } catch (e) {
    errorText.value = extractApiError(e, "诊断请求失败，请稍后重试");
  } finally {
    running.value = false;
  }
}

/** 诊断包下载文件名：campus-auth-diagnostics-<日期>.zip */
function bundleFilename(): string {
  return `campus-auth-diagnostics-${new Date().toISOString().slice(0, 10)}.zip`;
}

/** 导出诊断包：blob 下载 + a.download 命名；防连点，失败就地报错 */
async function exportBundle(): Promise<void> {
  if (exporting.value) return;
  exporting.value = true;
  exportError.value = "";
  try {
    const blob = await diagnosticsApi.exportBundle();
    const url = URL.createObjectURL(blob);
    const a = document.createElement("a");
    a.href = url;
    a.download = bundleFilename();
    document.body.appendChild(a);
    a.click();
    a.remove();
    URL.revokeObjectURL(url);
  } catch (e) {
    exportError.value = extractApiError(e, "诊断包导出失败，请稍后重试");
  } finally {
    exporting.value = false;
  }
}

// 仪表盘悬浮工具条的「开始诊断」经模板 ref 调到这里：诊断报告归面板自身状态，
// 从入口处直接调 API 面板不会展示结果。running 的防连点守卫在 startDiagnostics 内。
defineExpose({ startDiagnostics });
</script>

<template>
  <div class="card diag-card">
    <div class="card-header">
      <h2>网络诊断</h2>
    </div>
    <div class="card-body">
      <p v-if="!report && !errorText && !running" class="diag-hint">
        对当前网络链路做一次快速体检：本机网卡 → 默认网关 → DNS → 认证门户 → 近期登录失败。
      </p>

      <div v-if="errorText" class="diag-error" role="alert">{{ errorText }}</div>
      <div
        v-if="exportError"
        class="diag-error"
        role="alert"
        data-testid="diag-export-error"
      >
        {{ exportError }}
      </div>

      <template v-if="report">
        <div class="diag-verdict" :class="VERDICT_META[report.verdict].cls" data-testid="diag-verdict">
          <strong>{{ VERDICT_META[report.verdict].label }}</strong>
          <span>{{ report.conclusion }}</span>
          <span class="diag-total-time">共 {{ report.total_ms }}ms</span>
        </div>
        <ol class="diag-checks">
          <li
            v-for="check in report.checks"
            :key="check.key"
            class="diag-check"
            :class="STATUS_META[check.status].cls"
          >
            <div class="diag-check-head">
              <span class="diag-check-status" :title="STATUS_META[check.status].label">
                <IconApp :name="STATUS_META[check.status].icon" />
              </span>
              <span class="diag-check-name">{{ check.name }}</span>
              <span class="diag-check-summary" :title="check.summary">{{ check.summary }}</span>
              <span class="diag-check-time">{{ check.elapsed_ms }}ms</span>
              <button
                v-if="hasDetail(check)"
                class="btn btn-sm btn-icon-only diag-detail-toggle"
                :aria-expanded="isExpanded(check.key)"
                :aria-label="`展开${check.name}细节`"
                @click="toggleDetail(check.key)"
              >
                <IconApp name="chevron-down" :class="{ 'diag-chevron-open': isExpanded(check.key) }" />
              </button>
            </div>
            <ul v-if="isExpanded(check.key)" class="diag-check-details">
              <li v-for="(line, i) in detailLines(check.details)" :key="i">{{ line }}</li>
            </ul>
          </li>
        </ol>
      </template>
    </div>
    <!-- 操作区贴底：卡片在仪表盘双栏被拉伸等高时，多余高度由 body 的检查列表吸收，
         footer 恒贴卡底（card 为 flex column，见 dashboard.css 的 .diag-card） -->
    <div class="card-footer">
      <button
        class="btn btn-sm btn-secondary"
        :disabled="exporting"
        data-testid="diag-export"
        @click="exportBundle"
      >
        <IconApp :name="exporting ? 'refresh' : 'download'" :class="{ spin: exporting }" />
        {{ exporting ? "导出中..." : "导出诊断包" }}
      </button>
      <button
        class="btn btn-sm btn-secondary"
        :disabled="running"
        data-testid="diag-start"
        @click="startDiagnostics"
      >
        <IconApp :name="running ? 'refresh' : 'activity'" :class="{ spin: running }" />
        {{ running ? "诊断中..." : "开始诊断" }}
      </button>
    </div>
  </div>
</template>

<style scoped>
/* 全部走 token，无 [data-theme] 分支：深浅主题由变量定义自动生效 */
.diag-hint {
  margin: 0;
  color: var(--text-muted);
  font-size: var(--text-sm);
}

.diag-error {
  padding: var(--space-sm) var(--space-md);
  border: 1px solid var(--danger-border);
  border-radius: var(--radius-md);
  background: var(--danger-bg);
  color: var(--danger);
  font-size: var(--text-sm);
}

/* 总结论高亮：按结论着色左边条与文本 */
.diag-verdict {
  display: flex;
  align-items: baseline;
  flex-wrap: wrap;
  gap: var(--space-sm);
  padding: var(--space-sm) var(--space-md);
  margin-bottom: var(--space-md);
  border-left: 3px solid var(--border-accent);
  border-radius: var(--radius-sm);
  font-size: var(--text-sm);
}

.diag-verdict--healthy {
  border-left-color: var(--success);
  background: var(--success-bg);
  color: var(--success);
}

.diag-verdict--degraded {
  border-left-color: var(--warning);
  background: var(--warning-bg);
  color: var(--warning-text);
}

.diag-verdict--faulty {
  border-left-color: var(--danger);
  background: var(--danger-bg);
  color: var(--danger);
}

.diag-verdict strong {
  font-weight: 600;
}

/* 总耗时与结论主体分开着色：正文用普通文本色，避免整句染成状态色难读 */
.diag-verdict > span {
  color: var(--text-secondary);
}

/* 检查列表：body（flex column）里唯一的弹性子项——仪表盘双栏把卡片拉伸等高时
   就地吸收多余高度（grow 只长不缩，常规报告量不产生滚动）；内容整体超高时由
   card-body 滚动兜底，总结论与贴底操作区保持可见 */
.diag-checks {
  list-style: none;
  margin: 0;
  padding: 0;
  flex: 1 0 auto;
}

.diag-check {
  border-top: 1px solid var(--border);
  padding: var(--space-sm) 0;
}

.diag-check:first-child {
  border-top: none;
}

.diag-check-head {
  display: grid;
  grid-template-columns: auto auto minmax(0, 1fr) auto auto;
  gap: var(--space-sm);
  align-items: center;
}

.diag-check-status {
  display: inline-flex;
  color: var(--text-muted);
}

.diag-status--ok .diag-check-status { color: var(--success); }
.diag-status--warn .diag-check-status { color: var(--warning); }
.diag-status--fail .diag-check-status { color: var(--danger); }

.diag-check-name {
  font-weight: 600;
  color: var(--text-primary);
  white-space: nowrap;
}

.diag-check-summary {
  color: var(--text-secondary);
  font-size: var(--text-sm);
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.diag-check-time {
  color: var(--text-muted);
  font-size: var(--text-xs);
  font-variant-numeric: tabular-nums;
}

.diag-detail-toggle .diag-chevron-open {
  transform: rotate(180deg);
}

.diag-check-details {
  list-style: none;
  margin: var(--space-sm) 0 0;
  padding: var(--space-sm) var(--space-md);
  border: 1px solid var(--border);
  border-radius: var(--radius-sm);
  background: var(--bg-secondary);
  color: var(--text-secondary);
  font-size: var(--text-xs);
  display: flex;
  flex-direction: column;
  gap: 4px;
  word-break: break-all;
}
</style>
