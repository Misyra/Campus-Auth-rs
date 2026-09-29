<script setup lang="ts">
/** HTTP 登录流程编辑器：有序步骤、响应变量和独立的结果判断。 */
import { computed, ref, watch } from "vue";
import CustomSelect from "@/components/common/CustomSelect.vue";
import FieldHelp from "@/components/common/FieldHelp.vue";
import IconApp from "@/components/common/IconApp.vue";
import type { HttpFlowStep, HttpFailureAction, HttpSuccessCheck } from "@/api/types";
import { emptyHttpRequestStep, HTTP_IP_EXAMPLE_SCRIPT, HTTP_TASK_PLACEHOLDER_URL, type HttpTaskDraft } from "@/utils/httpTask";
import {
  HTTP_CERT_POLICY_OPTIONS,
  HTTP_FAILURE_ACTION_OPTIONS,
  HTTP_METHOD_OPTIONS,
  HTTP_SUCCESS_CHECK_OPTIONS,
  certPolicyFromValue,
  certPolicyToValue,
  isCredentialExposedViaGet,
} from "@/utils/loginChannel";
import type { HttpCertPolicy } from "@/utils/loginChannel";

const props = defineProps<{ model: HttpTaskDraft }>();
const uid = `http-flow-${Math.random().toString(36).slice(2, 8)}`;
const errorOptions = [
  { value: "stop", label: "停止登录" },
  { value: "continue", label: "记录后继续" },
];
const extractSources = [
  { value: "json", label: "JSON 字段", example: "data.challenge" },
  { value: "header", label: "响应头", example: "x-token" },
  { value: "url", label: "最终地址参数", example: "session" },
  { value: "redirect", label: "跳转地址参数", example: "token" },
  { value: "html", label: "表单隐藏字段", example: "sign" },
  { value: "regex", label: "正文正则", example: "sign=([^&]+)" },
] as const;

const requestSteps = computed(() => props.model.steps.filter((step) => step.kind === "request"));
const expandedStepIds = ref<Set<string>>(new Set());
watch(() => props.model.id, () => {
  const first = props.model.steps[0];
  expandedStepIds.value = new Set(props.model._isNew && first ? [first.id] : []);
}, { immediate: true });

function isExpanded(id: string): boolean {
  return expandedStepIds.value.has(id);
}

function toggleStep(id: string): void {
  const next = new Set(expandedStepIds.value);
  if (next.has(id)) next.delete(id);
  else next.add(id);
  expandedStepIds.value = next;
}

function openOnly(id: string): void {
  expandedStepIds.value = new Set([id]);
}

function extractedVariables(step: HttpFlowStep): string[] {
  return step.kind === "request" ? step.extracts.map((rule) => rule.name.trim()).filter(Boolean) : [];
}
function placeholder(name: string): string { return `{${name}}`; }

function stepSummary(step: HttpFlowStep): string {
  if (step.kind === "transform") return step.script.trim() ? "生成后续请求使用的字段" : "待填写计算脚本";
  const url = step.url.trim() === HTTP_TASK_PLACEHOLDER_URL ? "" : step.url.trim();
  const variable = extractedVariables(step).length ? `  ·  产出 ${extractedVariables(step).map((name) => `{${name}}`).join("、")}` : "";
  return `${url || "待填写请求地址"}${variable}`;
}

function sourceKind(source: string): string { return source.split(":", 1)[0] || "json"; }
function sourceDetail(source: string): string { return source.includes(":") ? source.slice(source.indexOf(":") + 1) : ""; }
function sourceExample(source: string): string { return extractSources.find((item) => item.value === sourceKind(source))?.example || "字段名"; }
function setSourceKind(step: HttpFlowStep, index: number, kind: string): void {
  const rule = step.extracts[index];
  if (!rule) return;
  rule.source = `${kind}:${sourceDetail(rule.source)}`;
  enableFlow();
}
function setSourceDetail(step: HttpFlowStep, index: number, detail: string): void {
  const rule = step.extracts[index];
  if (!rule) return;
  rule.source = `${sourceKind(rule.source)}:${detail}`;
  enableFlow();
}
function addExtract(step: HttpFlowStep): void {
  if (step.extracts.length >= 12) return;
  step.extracts.push({ source: "json:", name: "" });
  enableFlow();
}
function removeExtract(step: HttpFlowStep, index: number): void {
  step.extracts.splice(index, 1);
  enableFlow();
}
const certPolicy = computed<HttpCertPolicy>({
  get: () => certPolicyFromValue(props.model.ignore_https_errors),
  set: (value) => { props.model.ignore_https_errors = certPolicyToValue(value); },
});
const successCheck = computed<HttpSuccessCheck>({
  get: () => props.model.success_check,
  set: (value) => { props.model.success_check = value === "network" ? "network" : "response"; },
});
const failureAction = computed<HttpFailureAction>({
  get: () => props.model.failure_action,
  set: (value) => { props.model.failure_action = value; },
});

function enableFlow(): void {
  props.model._flowEnabled = true;
}

function nextStepId(): string {
  const taken = new Set(props.model.steps.map((step) => step.id));
  let number = 1;
  while (taken.has(`step_${number}`)) number += 1;
  return `step_${number}`;
}

function addRequest(): void {
  if (props.model.steps.length >= 16) return;
  const step = emptyHttpRequestStep(nextStepId(), `请求 ${requestSteps.value.length + 1}`);
  props.model.steps.push(step);
  if (!props.model.result_step_id) props.model.result_step_id = step.id;
  openOnly(step.id);
  enableFlow();
}

function addTransform(): void {
  if (props.model.steps.length >= 16) return;
  const step: HttpFlowStep = {
    ...emptyHttpRequestStep(nextStepId(), "计算字段"),
    kind: "transform",
    script: "function transform(ctx) {\n  return { };\n}",
  };
  props.model.steps.push(step);
  openOnly(step.id);
  enableFlow();
}

function moveStep(index: number, direction: -1 | 1): void {
  const target = index + direction;
  if (target < 0 || target >= props.model.steps.length) return;
  const [step] = props.model.steps.splice(index, 1);
  if (!step) return;
  props.model.steps.splice(target, 0, step);
  enableFlow();
}

function duplicateStep(index: number): void {
  if (props.model.steps.length >= 16) return;
  const source = props.model.steps[index];
  if (!source) return;
  const copy = { ...source, extracts: source.extracts.map((rule) => ({ ...rule })), id: nextStepId(), name: `${source.name || "步骤"} 副本` };
  props.model.steps.splice(index + 1, 0, copy);
  openOnly(copy.id);
  enableFlow();
}

function deleteStep(index: number): void {
  const step = props.model.steps[index];
  if (!step || (step.kind === "request" && requestSteps.value.length <= 1)) return;
  props.model.steps.splice(index, 1);
  if (expandedStepIds.value.has(step.id)) {
    const next = new Set(expandedStepIds.value);
    next.delete(step.id);
    expandedStepIds.value = next;
  }
  if (props.model.result_step_id === step.id) {
    props.model.result_step_id = requestSteps.value.at(-1)?.id ?? "";
  }
  enableFlow();
}

function setStepMethod(step: HttpFlowStep, value: string): void {
  step.method = value === "POST" ? "POST" : "GET";
  enableFlow();
}

function setStepError(step: HttpFlowStep, value: string): void {
  step.on_error = value === "continue" ? "continue" : "stop";
  enableFlow();
}

function setResultStep(event: Event): void {
  props.model.result_step_id = (event.target as HTMLSelectElement).value;
  const selected = props.model.steps.find((step) => step.id === props.model.result_step_id);
  if (selected) selected.on_error = "stop";
  enableFlow();
}
</script>

<template>
  <div class="flow-editor">
    <div class="form-row">
      <div class="form-group">
        <label :for="`${uid}-auth-url`">认证页面地址</label>
        <input :id="`${uid}-auth-url`" v-model.trim="model.auth_url" type="text"
          placeholder="http://10.0.0.1/（留空时使用方案的认证地址）" />
      </div>
      <div class="form-group flow-cert">
        <label>HTTPS 证书</label>
        <CustomSelect v-model="certPolicy" :options="HTTP_CERT_POLICY_OPTIONS" />
      </div>
    </div>
    <div class="flow-workspace">
    <div class="flow-sequence">
    <div class="flow-list-heading">
      <div>
        <strong>执行步骤</strong>
        <span class="flow-step-count">{{ model.steps.length }} / 16</span>
        <span class="hint">按顺序发送请求并传递变量</span>
      </div>
      <button v-if="expandedStepIds.size" type="button" class="flow-collapse-all" @click="expandedStepIds = new Set()">全部收起</button>
    </div>

    <div class="flow-vars-guide">
      <div class="flow-vars-guide-title">示例：把本机 IP 用到登录请求</div>
      <div class="flow-vars-example">
        <span>计算步骤把本机 IP 存为 <code>ip</code></span>
        <span>登录请求需要 IP 时，填 <code>{ip}</code></span>
      </div>
      <p>发送时会自动填入本机 IP。其他步骤取得的值也能这样传给后面的请求。</p>
    </div>

    <div v-for="(step, index) in model.steps" :key="step.id" class="flow-step" :class="{ 'flow-step--open': isExpanded(step.id) }">
      <div class="flow-step-head">
        <button type="button" class="flow-step-toggle" :aria-expanded="isExpanded(step.id)" :aria-controls="`${uid}-${step.id}-body`" :aria-label="`${isExpanded(step.id) ? '收起' : '展开'}步骤 ${index + 1}：${step.name || (step.kind === 'request' ? 'HTTP 请求' : '计算字段')}`" @click="toggleStep(step.id)">
          <span class="flow-step-number">{{ index + 1 }}</span>
          <span class="flow-step-copy">
            <span class="flow-step-titleline">
              <span class="flow-step-kind">{{ step.kind === 'request' ? step.method : '计算' }}</span>
              <strong>{{ step.name || (step.kind === 'request' ? 'HTTP 请求' : '计算字段') }}</strong>
              <span v-if="step.id === model.result_step_id" class="flow-step-badge">结果来源</span>
            </span>
            <span class="flow-step-summary" :title="stepSummary(step)">{{ stepSummary(step) }}</span>
          </span>
          <IconApp name="chevron-down" class="flow-step-chevron icon-sm" :class="{ 'flow-step-chevron--open': isExpanded(step.id) }" />
        </button>
        <div class="flow-step-actions" role="group" :aria-label="`步骤 ${index + 1} 操作`">
          <button type="button" class="flow-action" :disabled="index === 0" :aria-label="`上移步骤 ${index + 1}`" title="上移" @click="moveStep(index, -1)"><IconApp name="arrow-up" class="icon-sm" /></button>
          <button type="button" class="flow-action" :disabled="index === model.steps.length - 1" :aria-label="`下移步骤 ${index + 1}`" title="下移" @click="moveStep(index, 1)"><IconApp name="arrow-down" class="icon-sm" /></button>
          <span class="flow-action-divider" aria-hidden="true"></span>
          <button type="button" class="flow-action" :disabled="model.steps.length >= 16" :aria-label="`复制步骤 ${index + 1}`" title="复制" @click="duplicateStep(index)"><IconApp name="copy" class="icon-sm" /></button>
          <button type="button" class="flow-action flow-action--danger" :disabled="step.kind === 'request' && requestSteps.length <= 1" :aria-label="`删除步骤 ${index + 1}`" title="删除" @click="deleteStep(index)"><IconApp name="trash" class="icon-sm" /></button>
        </div>
      </div>

      <div v-show="isExpanded(step.id)" :id="`${uid}-${step.id}-body`" class="flow-step-body">
      <div class="form-group">
        <label :for="`${uid}-${step.id}-name`">步骤名称</label>
        <input :id="`${uid}-${step.id}-name`" v-model.trim="step.name" type="text" placeholder="给这一步起个易懂的名称" @input="enableFlow" />
      </div>

      <template v-if="step.kind === 'request'">
        <div class="form-row flow-address-row">
          <div class="form-group flow-method">
            <label>方法</label>
            <CustomSelect :model-value="step.method" :options="HTTP_METHOD_OPTIONS" @update:model-value="setStepMethod(step, $event)" />
          </div>
          <div class="form-group">
            <label :for="`${uid}-${step.id}-url`">请求地址</label>
            <input :id="`${uid}-${step.id}-url`" v-model.trim="step.url" type="text" placeholder="http://10.0.0.1/login" @input="enableFlow" />
          </div>
        </div>
        <p v-if="isCredentialExposedViaGet(step.method, step.url)" class="note note--warn">
          此步骤把密码放进 URL，网关或代理的日志可能记录完整地址；门户支持时优先使用 POST。
        </p>
        <div class="form-group">
          <label :for="`${uid}-${step.id}-headers`">请求头（可选，每行一项）</label>
          <textarea :id="`${uid}-${step.id}-headers`" v-model="step.headers" rows="2" placeholder="Content-Type: application/x-www-form-urlencoded" @input="enableFlow"></textarea>
        </div>
        <div v-if="step.method === 'POST'" class="form-group">
          <label :for="`${uid}-${step.id}-request-body`">请求内容</label>
          <textarea :id="`${uid}-${step.id}-request-body`" v-model="step.body" rows="3" placeholder="username={username}&password={password}" @input="enableFlow"></textarea>
        </div>
        <label class="flow-redirect-toggle">
          <input v-model="step.stop_on_redirect" type="checkbox" @change="enableFlow" />
          <span>收到 3xx 时停在此处，读取 Location（默认自动跳转）</span>
        </label>
        <div class="flow-extracts">
          <div class="flow-extracts-heading">
            <div><strong>保存响应字段</strong><span>后续步骤可引用，最多 12 个</span></div>
            <button type="button" class="btn btn-sm" :disabled="step.extracts.length >= 12" @click="addExtract(step)">+ 添加字段</button>
          </div>
          <div v-for="(rule, ruleIndex) in step.extracts" :key="ruleIndex" class="flow-extract-rule">
            <div class="form-group">
              <label :for="`${uid}-${step.id}-extract-kind-${ruleIndex}`">取自</label>
              <select :id="`${uid}-${step.id}-extract-kind-${ruleIndex}`" :value="sourceKind(rule.source)" @change="setSourceKind(step, ruleIndex, ($event.target as HTMLSelectElement).value)">
                <option v-for="option in extractSources" :key="option.value" :value="option.value">{{ option.label }}</option>
              </select>
            </div>
            <div class="form-group">
              <label :for="`${uid}-${step.id}-extract-detail-${ruleIndex}`">字段或表达式</label>
              <input :id="`${uid}-${step.id}-extract-detail-${ruleIndex}`" :value="sourceDetail(rule.source)" type="text" :placeholder="sourceExample(rule.source)" @input="setSourceDetail(step, ruleIndex, ($event.target as HTMLInputElement).value)" />
            </div>
            <div class="form-group">
              <label :for="`${uid}-${step.id}-extract-name-${ruleIndex}`">存为变量</label>
              <input :id="`${uid}-${step.id}-extract-name-${ruleIndex}`" v-model.trim="rule.name" type="text" placeholder="challenge" @input="enableFlow" />
            </div>
            <button type="button" class="flow-extract-remove" :aria-label="`删除第 ${ruleIndex + 1} 个取值字段`" title="删除字段" @click="removeExtract(step, ruleIndex)"><IconApp name="trash" class="icon-sm" /></button>
          </div>
          <p v-if="step.extracts.some((rule) => sourceKind(rule.source) === 'redirect') && !step.stop_on_redirect" class="hint">取跳转地址参数时，请启用上方“停在 3xx”。</p>
          <div v-if="extractedVariables(step).length" class="flow-var-output">
            <span>后续请求可写</span>
            <code v-for="name in extractedVariables(step)" :key="name">{{ placeholder(name) }}</code>
            <span>计算脚本可读 <code>ctx.vars.变量名</code></span>
          </div>
        </div>
        <div class="form-row">
          <div class="form-group">
            <label>请求失败时</label>
            <CustomSelect :model-value="step.on_error" :options="errorOptions" :disabled="step.id === model.result_step_id"
              @update:model-value="setStepError(step, $event)" />
            <span v-if="step.id === model.result_step_id" class="hint">结果来源步骤必须停止并报告失败。</span>
          </div>
          <div class="form-group">
            <label :for="`${uid}-${step.id}-wait`">请求后等待（秒）</label>
            <input :id="`${uid}-${step.id}-wait`" v-model.number="step.wait_secs" type="number" min="0" max="30" step="0.5" @input="enableFlow" />
          </div>
        </div>
      </template>
      <template v-else>
        <div class="form-group">
          <div class="field-label-row">
            <label :for="`${uid}-${step.id}-script`">计算脚本</label>
            <FieldHelp text="定义 function transform(ctx)，返回对象。ctx.vars 包含前面步骤提取的变量；ctx.username、ctx.password、ctx.isp、ctx.auth_url、ctx.page、ctx.local_ip、ctx.local_mac 继续可用。脚本在无网络与文件访问的沙箱中运行。" wide />
          </div>
          <textarea :id="`${uid}-${step.id}-script`" v-model="step.script" class="flow-script" rows="7" @input="enableFlow"></textarea>
          <span v-if="step.script === HTTP_IP_EXAMPLE_SCRIPT" class="flow-script-hint">示例中 <code>ctx.local_ip</code> 是本机 IP，原样保存为 <code>ip</code>；后面的请求需要时填 <code>{ip}</code>。</span>
          <span v-else class="flow-script-hint">例如返回 <code>{ signed: ... }</code>，后续请求可写 <code>{signed}</code>，后续计算可读 <code>ctx.vars.signed</code>。</span>
        </div>
      </template>
      </div>
    </div>

    <div class="flow-add">
      <button type="button" class="btn btn-sm" :disabled="model.steps.length >= 16" @click="addRequest"><IconApp name="plus" class="icon-sm" /> 添加请求步骤</button>
      <button type="button" class="btn btn-sm" :disabled="model.steps.length >= 16" @click="addTransform"><IconApp name="plus" class="icon-sm" /> 添加计算步骤</button>
    </div>
    </div>

    <section class="flow-result">
      <div class="flow-result-heading">
        <span class="flow-result-icon"><IconApp name="target" class="icon-sm" /></span>
        <div>
          <h4>结果判断</h4>
          <p>使用指定请求的响应判断结果，之后再验证网络连通性。</p>
        </div>
      </div>
      <div class="form-row">
        <div class="form-group">
          <label :for="`${uid}-result-step`">依据哪个请求步骤</label>
          <select :id="`${uid}-result-step`" :value="model.result_step_id" @change="setResultStep">
            <option v-for="step in requestSteps" :key="step.id" :value="step.id">{{ step.name || step.id }}</option>
          </select>
        </div>
        <div class="form-group">
          <label>成功依据</label>
          <CustomSelect v-model="successCheck" :options="HTTP_SUCCESS_CHECK_OPTIONS" />
        </div>
      </div>
      <div class="form-row">
        <div class="form-group">
          <label :for="`${uid}-success-pattern`">成功标识（可选）</label>
          <input :id="`${uid}-success-pattern`" v-model="model.success_pattern" type="text" placeholder="留空时按 HTTP 2xx 判断" />
        </div>
        <div class="form-group">
          <label :for="`${uid}-failure-pattern`">失败标识（可选）</label>
          <input :id="`${uid}-failure-pattern`" v-model="model.failure_pattern" type="text" placeholder="如密码错误" />
        </div>
      </div>
      <div class="form-group">
        <label>命中失败标识后</label>
        <CustomSelect v-model="failureAction" :options="HTTP_FAILURE_ACTION_OPTIONS" />
      </div>
    </section>
    </div>
  </div>
</template>

<style scoped>
.flow-editor { display: flex; flex-direction: column; gap: 22px; }
.flow-editor > .form-row { max-width: 1280px; }
.flow-workspace { display: grid; grid-template-columns: minmax(0, 1fr); align-items: start; gap: 24px; padding-top: 20px; border-top: 1px solid var(--border); }
.flow-sequence { display: flex; flex-direction: column; gap: 12px; min-width: 0; }
.flow-list-heading { display: flex; align-items: center; justify-content: space-between; gap: 14px; margin-bottom: 3px; }
.flow-list-heading > div { display: flex; align-items: center; flex-wrap: wrap; gap: 10px; }
.flow-list-heading strong { font-size: 15px; font-weight: 700; }
.flow-step-count { padding: 3px 8px; border-radius: 999px; background: rgba(var(--accent-rgb), .1); color: var(--text-primary); font-size: 12px; font-variant-numeric: tabular-nums; font-weight: 650; }
.flow-list-heading .hint { font-size: 12px; }
.flow-collapse-all { flex: none; padding: 6px 9px; border: 0; border-radius: 7px; background: transparent; color: var(--text-secondary); font: inherit; font-size: 12px; white-space: nowrap; cursor: pointer; }
.flow-collapse-all:hover { color: var(--text-primary); background: var(--bg-card-elevated); }
.flow-vars-guide { padding: 15px 18px; border: 1px solid var(--border-accent-strong); border-radius: 12px; background: rgba(var(--accent-rgb), .045); }
.flow-vars-guide-title { margin-bottom: 9px; font-size: 13px; font-weight: 700; }
.flow-vars-guide p { margin: 9px 0 0; color: var(--text-secondary); font-size: 12px; line-height: 1.6; }
.flow-vars-example { display: flex; flex-direction: column; gap: 4px; color: var(--text-secondary); font-size: 12px; line-height: 1.6; }
.flow-vars-guide code, .flow-var-output code, .flow-script-hint code { padding: 2px 5px; border-radius: 5px; background: rgba(var(--accent-rgb), .1); color: var(--text-primary); font-family: var(--font-mono); font-size: 11px; }
.flow-step { border: 1px solid var(--border); border-radius: 12px; background: var(--bg-card); }
.flow-step--open { border-color: var(--border-accent-strong); }
.flow-step:has(.custom-select.open) { position: relative; z-index: 1; }
.flow-step-head { display: flex; align-items: center; min-height: 88px; overflow: hidden; border-radius: 11px 11px 0 0; }
.flow-step:not(.flow-step--open) .flow-step-head { border-radius: 11px; }
.flow-step-toggle { display: flex; flex: 1 1 auto; min-width: 0; align-items: center; gap: 16px; align-self: stretch; padding: 16px 15px 16px 18px; border: 0; background: transparent; color: inherit; font: inherit; text-align: left; cursor: pointer; }
.flow-step-toggle:hover { background: rgba(var(--accent-rgb), .045); }
.flow-step-toggle:focus-visible, .flow-action:focus-visible, .flow-collapse-all:focus-visible { outline: 2px solid var(--accent); outline-offset: -3px; }
.flow-step-number { display: inline-flex; flex: 0 0 38px; align-items: center; justify-content: center; width: 38px; height: 38px; border-radius: 11px; color: var(--on-accent); background: var(--accent); font-size: 15px; font-weight: 750; font-variant-numeric: tabular-nums; }
.flow-step-copy { display: flex; flex: 1 1 auto; flex-direction: column; min-width: 0; gap: 6px; }
.flow-step-titleline { display: flex; align-items: center; min-width: 0; gap: 9px; }
.flow-step-titleline strong { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; font-size: 15px; font-weight: 700; }
.flow-step-kind { flex: none; padding: 3px 7px; border-radius: 5px; color: var(--text-secondary); background: rgba(var(--slate-rgb), .12); font-family: var(--font-mono); font-size: 11px; font-weight: 700; line-height: 1.3; }
.flow-step-badge { flex: none; padding: 3px 8px; border-radius: 5px; color: var(--accent); background: rgba(var(--accent-rgb), .1); font-size: 11px; font-weight: 700; }
.flow-step-summary { overflow: hidden; color: var(--text-secondary); font-family: var(--font-mono); font-size: 12px; line-height: 1.4; text-overflow: ellipsis; white-space: nowrap; }
.flow-step-chevron { flex: none; color: var(--text-secondary); transition: transform .16s ease; }
.flow-step-chevron--open { transform: rotate(180deg); }
.flow-step-actions { display: flex; flex: none; align-items: center; gap: 3px; padding: 0 13px 0 6px; }
.flow-action { display: inline-flex; align-items: center; justify-content: center; width: 36px; height: 36px; padding: 0; border: 0; border-radius: 8px; background: transparent; color: var(--text-secondary); cursor: pointer; }
.flow-action :deep(svg) { width: 17px; height: 17px; }
.flow-action:hover:not(:disabled) { color: var(--text-primary); background: rgba(var(--accent-rgb), .1); }
.flow-action:disabled { opacity: .35; cursor: not-allowed; }
.flow-action--danger:hover:not(:disabled) { color: var(--danger); background: rgba(239, 68, 68, .09); }
.flow-action-divider { width: 1px; height: 18px; margin: 0 4px; background: var(--border); }
.flow-step-body { display: flex; flex-direction: column; gap: var(--space-md); padding: 22px 24px 24px 72px; border-top: 1px solid var(--border); }
.flow-step-body > .form-group, .flow-step-body > .form-row, .flow-step-body > .note { width: 100%; }
.flow-var-output { display: flex; align-items: center; flex-wrap: wrap; gap: 5px 8px; padding: 10px 12px; border-radius: 9px; background: rgba(var(--accent-rgb), .055); color: var(--text-secondary); font-size: 12px; line-height: 1.5; }
.flow-var-output > span:first-child { color: var(--text-primary); font-weight: 700; }
.flow-redirect-toggle { display: flex; align-items: center; gap: 9px; color: var(--text-secondary); font-size: 12px; cursor: pointer; }
.flow-redirect-toggle input { width: 16px; height: 16px; accent-color: var(--accent); }
.flow-extracts { display: flex; flex-direction: column; gap: 10px; padding: 14px; border: 1px solid var(--border); border-radius: 10px; background: rgba(var(--slate-rgb), .025); }
.flow-extracts-heading { display: flex; align-items: center; justify-content: space-between; gap: 12px; }
.flow-extracts-heading > div { display: flex; flex-direction: column; gap: 3px; }
.flow-extracts-heading strong { font-size: 13px; }
.flow-extracts-heading span { color: var(--text-secondary); font-size: 11px; }
.flow-extract-rule { display: grid; grid-template-columns: minmax(115px, .6fr) minmax(150px, 1.3fr) minmax(125px, 1fr) 30px; gap: 9px; align-items: end; padding: 10px; border: 1px solid var(--border); border-radius: 8px; background: var(--bg-card); }
.flow-extract-rule .form-group { min-width: 0; }
.flow-extract-rule select, .flow-extract-rule input { width: 100%; }
.flow-extract-remove { display: inline-flex; align-items: center; justify-content: center; width: 30px; height: 36px; border: 0; border-radius: 7px; background: transparent; color: var(--text-secondary); cursor: pointer; }
.flow-extract-remove:hover { background: var(--danger-bg); color: var(--danger); }
.flow-script-hint { color: var(--text-secondary); font-size: 12px; line-height: 1.6; }
.flow-method { flex: 0 0 110px; }
.flow-address-row > .form-group:last-child { min-width: 0; flex: 1; }
.flow-add { display: flex; align-items: center; flex-wrap: wrap; gap: 9px; padding: 9px 0 0 54px; }
.flow-result { padding: 24px; border: 1px solid var(--border); border-radius: 12px; background: var(--bg-card); }
.flow-result-heading { display: flex; align-items: flex-start; gap: 12px; padding-bottom: 18px; margin-bottom: 20px; border-bottom: 1px solid var(--border); }
.flow-result-icon { display: inline-flex; align-items: center; justify-content: center; flex: 0 0 36px; width: 36px; height: 36px; border-radius: 9px; color: var(--on-accent); background: var(--accent); }
.flow-result-icon :deep(svg) { width: 18px; height: 18px; }
.flow-result h4 { margin: 0 0 4px; font-size: 15px; }
.flow-result-heading p { margin: 0; color: var(--text-secondary); font-size: 12px; line-height: 1.5; }
.flow-result > .form-row, .flow-result > .form-group { max-width: 940px; }
.flow-script { font-family: var(--font-mono); }
@media (min-width: 1800px) {
  .flow-workspace { grid-template-columns: minmax(0, 1.35fr) minmax(360px, .85fr); }
  .flow-result { position: sticky; top: 104px; }
  .flow-result .form-row { flex-direction: column; gap: 14px; }
}
@media (max-width: 700px) {
  .flow-step-head { align-items: stretch; flex-wrap: wrap; }
  .flow-step-toggle { min-height: 72px; }
  .flow-step-titleline { flex-wrap: wrap; row-gap: 4px; }
  .flow-step-titleline strong { white-space: normal; }
  .flow-step-actions { width: 100%; justify-content: flex-end; padding: 0 10px 8px; }
  .flow-step-body { padding: 18px 14px; }
  .flow-method { flex: 1 1 100%; }
  .flow-add { padding-left: 0; }
  .flow-result { padding: 16px; }
  .flow-extract-rule { grid-template-columns: minmax(0, 1fr) minmax(0, 1fr) 30px; }
  .flow-extract-rule > .form-group:nth-child(2) { grid-column: 1 / 3; grid-row: 2; }
  .flow-extract-rule > .form-group:nth-child(3) { grid-column: 2; grid-row: 1; }
  .flow-extract-remove { grid-column: 3; grid-row: 1; }
}
@media (prefers-reduced-motion: reduce) { .flow-step-chevron { transition: none; } }
</style>
