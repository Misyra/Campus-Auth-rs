<script setup lang="ts">
/**
 * 直连测试结果卡（任务页编辑器与方案编辑器共用）。
 *
 * 只负责渲染一次测试的结论、下一步建议与「实际发出的请求/响应」明细——
 * 结果状态与发送动作由宿主持有（两处的凭据来源不同：任务页手填，方案页取方案）。
 */
import IconApp from "@/components/common/IconApp.vue";
import { httpTestOutcomeHint, httpTestOutcomeLabel } from "@/utils/loginChannel";
import type { HttpLoginTestResult } from "@/api/types";

defineProps<{
  /** 测试结果；null 时由宿主决定不渲染本组件 */
  result: HttpLoginTestResult;
}>();
</script>

<template>
  <div class="http-test-result" :class="result.outcome === 'success' ? 'success' : 'failed'">
    <div class="http-test-result-head">
      <span class="http-test-result-icon">
        <IconApp :name="result.outcome === 'success' ? 'check-circle' : 'alert-triangle'" />
      </span>
      <div class="http-test-result-title">
        <strong>{{ httpTestOutcomeLabel(result.outcome) }}</strong>
        <span v-if="result.status">HTTP {{ result.status }}</span>
        <span v-else>无响应</span>
        <span>{{ result.duration_ms }} ms</span>
      </div>
    </div>

    <p class="http-test-result-message">{{ result.message }}</p>
    <p class="http-test-result-hint">{{ httpTestOutcomeHint(result.outcome) }}</p>

    <div v-if="result.script_error" class="http-test-script-error">
      <strong>脚本错误</strong>
      <code>{{ result.script_error }}</code>
    </div>

    <details class="http-test-detail">
      <summary>查看实际发出的请求与响应</summary>
      <dl>
        <template v-if="result.rendered_url">
          <dt>请求地址</dt><dd><code>{{ result.rendered_url }}</code></dd>
        </template>
        <template v-if="result.rendered_headers">
          <dt>请求头</dt><dd><code>{{ result.rendered_headers }}</code></dd>
        </template>
        <template v-if="result.rendered_body">
          <dt>请求内容</dt><dd><code>{{ result.rendered_body }}</code></dd>
        </template>
        <template v-if="result.response_headers">
          <dt>响应头</dt><dd><code>{{ result.response_headers }}</code></dd>
        </template>
        <template v-if="result.response_snippet">
          <dt>响应片段</dt><dd><code>{{ result.response_snippet }}</code></dd>
        </template>
      </dl>
      <p class="http-test-detail-note">其中的账号与密码已替换为 <code>***</code>。</p>
    </details>
  </div>
</template>

<style scoped>
.http-test-result {
  margin-top: var(--space-md);
  padding: var(--space-md);
  border-radius: var(--radius-md);
  border: 1px solid var(--danger-border);
  background: var(--danger-bg);
}

.http-test-result.success {
  border-color: var(--success-border);
  background: var(--success-bg);
}

.http-test-result-head {
  display: flex;
  align-items: center;
  gap: 10px;
}

.http-test-result-icon {
  display: flex;
  color: var(--error);
}

.http-test-result.success .http-test-result-icon {
  color: var(--success);
}

.http-test-result-icon svg {
  width: 22px;
  height: 22px;
}

.http-test-result-title {
  display: flex;
  align-items: baseline;
  flex-wrap: wrap;
  gap: var(--space-sm);
  min-width: 0;
}

.http-test-result-title strong {
  color: var(--text-primary);
  font-size: var(--text-base);
}

.http-test-result-title span {
  color: var(--text-muted);
  font-family: var(--font-mono);
  font-size: var(--text-xs);
}

.http-test-result-message {
  margin: 8px 0 0;
  color: var(--text-secondary);
  font-size: var(--text-sm);
  overflow-wrap: anywhere;
}

/* 「下一步该怎么办」：与结论分开呈现，避免用户只看结论就反复重试同一配置 */
.http-test-result-hint {
  margin: 8px 0 0;
  padding: 8px 10px;
  border-radius: var(--radius-sm);
  background: rgba(var(--slate-rgb), 0.08);
  color: var(--text-secondary);
  font-size: var(--text-sm);
  line-height: 1.6;
}

.http-test-script-error {
  display: flex;
  flex-direction: column;
  gap: 4px;
  margin-top: var(--space-sm);
  padding: 8px 10px;
  border-radius: var(--radius-sm);
  border: 1px solid var(--danger-border);
  background: rgba(var(--error-rgb), 0.06);
}

.http-test-script-error strong {
  color: var(--error);
  font-size: var(--text-sm);
}

.http-test-script-error code {
  color: var(--text-secondary);
  font-family: var(--font-mono);
  font-size: var(--text-xs);
  overflow-wrap: anywhere;
}

.http-test-detail {
  margin-top: var(--space-sm);
}

.http-test-detail summary {
  color: var(--accent);
  font-size: var(--text-sm);
  cursor: pointer;
}

.http-test-detail dl {
  display: grid;
  grid-template-columns: 76px minmax(0, 1fr);
  gap: 6px 10px;
  margin: var(--space-sm) 0 0;
}

.http-test-detail dt {
  color: var(--text-muted);
  font-size: var(--text-xs);
}

.http-test-detail dd {
  min-width: 0;
  margin: 0;
}

.http-test-detail dd code {
  display: block;
  max-height: 120px;
  overflow: auto;
  white-space: pre-wrap;
  overflow-wrap: anywhere;
  color: var(--text-primary);
  font-family: var(--font-mono);
  font-size: var(--text-xs);
}

.http-test-detail-note {
  margin: var(--space-sm) 0 0;
  color: var(--text-muted);
  font-size: var(--text-xs);
}

.http-test-detail-note code {
  font-family: var(--font-mono);
}

@media (max-width: 768px) {
  .http-test-detail dl {
    grid-template-columns: 1fr;
  }
}
</style>
