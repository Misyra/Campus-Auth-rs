<script setup lang="ts">
/** HTTP 登录流程编辑器：有序步骤、响应变量和独立的结果判断。 */
import { computed, nextTick, ref, watch } from "vue";
import CustomSelect from "@/components/common/CustomSelect.vue";
import HttpResponseValues from "@/components/common/HttpResponseValues.vue";
import FieldHelp from "@/components/common/FieldHelp.vue";
import IconApp from "@/components/common/IconApp.vue";
import type { HttpFlowStep, HttpFailureAction, HttpSuccessCheck } from "@/api/types";
import { emptyHttpRequestStep, HTTP_IP_EXAMPLE_SCRIPT, HTTP_TASK_PLACEHOLDER_URL, type HttpTaskDraft } from "@/utils/httpTask";
import {
  HTTP_CERT_POLICY_OPTIONS,
  HTTP_FAILURE_ACTION_OPTIONS,
  HTTP_METHOD_OPTIONS,
  certPolicyFromValue,
  certPolicyToValue,
  isCredentialExposedViaGet,
} from "@/utils/loginChannel";
import type { HttpCertPolicy } from "@/utils/loginChannel";

const props = defineProps<{ model: HttpTaskDraft; resultTarget?: string }>();
const uid = `http-flow-${Math.random().toString(36).slice(2, 8)}`;
const errorOptions = [
  { value: "stop", label: "停止登录" },
  { value: "continue", label: "记录后继续" },
];
const successOptions = [
  { value: "network", label: "能访问外网（推荐）" },
  { value: "response", label: "返回内容符合预期" },
];
const commonValues = [
  { name: "username", label: "账号" },
  { name: "password", label: "密码" },
  { name: "isp", label: "运营商" },
];
const otherValues = [
  { name: "auth_url", label: "认证地址" },
  { name: "local_ip", label: "本机 IP" },
  { name: "local_mac", label: "本机 MAC" },
];
type RequestField = "url" | "body" | "headers";
// 插入位置属于界面状态，避免进入自动保存载荷。
const activeFields = ref<Record<string, RequestField>>({});
function availableValues(index: number) {
  const earlier = props.model.steps.slice(0, index).flatMap(step => extractedVariables(step));
  const values = [...otherValues, ...earlier.map(name => ({ name, label: "前面取得的值" }))];
  return Array.from(new Map(values.map(value => [value.name, value])).values());
}
async function insertValue(step: HttpFlowStep, name: string): Promise<void> {
  const fallback: RequestField = step.method === "POST" ? "body" : "url";
  let field = activeFields.value[step.id] ?? fallback;
  const findInput = (target: RequestField) => document.getElementById(`${uid}-${step.id}-${target === "body" ? "request-body" : target}`) as HTMLInputElement | HTMLTextAreaElement | null;
  let element = findInput(field);
  if (!element?.getClientRects().length) { field = fallback; element = findInput(field); }
  activeFields.value[step.id] = field;
  const value = step[field];
  const start = element?.selectionStart ?? value.length;
  const end = element?.selectionEnd ?? start;
  const token = `{${name}}`;
  step[field] = value.slice(0, start) + token + value.slice(end);
  enableFlow();
  await nextTick();
  element?.focus();
  element?.setSelectionRange(start + token.length, start + token.length);
}
function hasAdvancedSettings(step: HttpFlowStep): boolean {
  return !!step.headers.trim() || step.stop_on_redirect || step.on_error === "continue" || step.wait_secs > 0;
}

const requestSteps = computed(() => props.model.steps.filter((step) => step.kind === "request"));
const expandedStepIds = ref<Set<string>>(new Set());
watch(() => props.model.id, () => {
  const first = props.model.steps[0];
  expandedStepIds.value = new Set(first ? [first.id] : []);
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

function placeholder(name: string): string { return `{${name}}`; }

function extractedVariables(step: HttpFlowStep): string[] {
  return step.kind === "request" ? step.extracts.map((rule) => rule.name.trim()).filter(Boolean) : [];
}

function stepSummary(step: HttpFlowStep): string {
  if (step.kind === "transform") return step.script.trim() ? "计算后面请求需要的值" : "待填写计算脚本";
  const url = step.url.trim() === HTTP_TASK_PLACEHOLDER_URL ? "" : step.url.trim();
  const variable = extractedVariables(step).length ? `  ·  后面可用 ${extractedVariables(step).map((name) => `{${name}}`).join("、")}` : "";
  return `${url || "待填写请求地址"}${variable}`;
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
    <details class="flow-options flow-connection" :open="!!model.auth_url || model.ignore_https_errors != null">
      <summary>认证地址与证书 <span>默认跟随方案和全局设置</span></summary>
      <div class="form-row">
      <div class="form-group">
        <label :for="`${uid}-auth-url`">认证页面地址（可选）</label>
        <input :id="`${uid}-auth-url`" v-model.trim="model.auth_url" type="text"
          placeholder="http://10.0.0.1/（留空时使用方案的认证地址）" />
      </div>
      <div class="form-group flow-cert">
        <label>HTTPS 证书</label>
        <CustomSelect v-model="certPolicy" :options="HTTP_CERT_POLICY_OPTIONS" />
      </div>
      </div>
    </details>
    <div class="flow-workspace">
    <div class="flow-sequence">
    <div class="flow-list-heading">
      <div>
        <strong>请求顺序</strong>
        <span class="flow-step-count">{{ model.steps.length }} / 16</span>

      </div>
      <button v-if="expandedStepIds.size" type="button" class="flow-collapse-all" @click="expandedStepIds = new Set()">全部收起</button>
    </div>

    <p class="flow-intro">填写门户的登录接口和请求内容。只有登录前需要获取 token、清理会话等操作时，才需要添加更多步骤。</p>

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
            <label>请求方式</label>
            <CustomSelect :model-value="step.method" :options="HTTP_METHOD_OPTIONS" aria-label="请求方式" @update:model-value="setStepMethod(step, $event)" />
          </div>
          <div class="form-group">
            <label :for="`${uid}-${step.id}-url`">请求地址</label>
            <input :id="`${uid}-${step.id}-url`" v-model.trim="step.url" type="text" placeholder="http://10.0.0.1/login" @focus="activeFields[step.id] = 'url'" @input="enableFlow" />
          </div>
        </div>
        <p v-if="isCredentialExposedViaGet(step.method, step.url)" class="note note--warn">
          此步骤把密码放进 URL，网关或代理的日志可能记录完整地址；门户支持时优先使用 POST。
        </p>
        <p class="flow-help">{{ step.method === 'POST' ? 'POST：地址填写接口，请求内容写在下面。' : 'GET：参数直接写在地址里，例如 /login?username={username}。' }}</p>
        <div v-if="step.method === 'POST'" class="form-group">
          <label :for="`${uid}-${step.id}-request-body`">请求内容</label>
          <textarea :id="`${uid}-${step.id}-request-body`" v-model="step.body" rows="3" placeholder="username={username}&password={password}" @focus="activeFields[step.id] = 'body'" @input="enableFlow"></textarea>
          <span class="hint">通常按门户的格式填写；表单内容未指定类型时会自动补齐 Content-Type。</span>
        </div>
        <div class="flow-values">
          <div class="flow-value-buttons">
            <span>点击填入</span>
            <button v-for="value in commonValues" :key="value.name" type="button" class="flow-value-button" :aria-label="`填入${value.label}占位符`" @mousedown.prevent @click="insertValue(step, value.name)">{{ value.label }} <code>{{ placeholder(value.name) }}</code></button>
          </div>
          <p class="flow-help">插入到刚才编辑的输入框；正式登录时自动使用方案里的值，账号密码不用写进任务。</p>
          <details class="flow-other-values">
            <summary>其他可用值</summary>
            <div class="flow-value-buttons">
              <button v-for="value in availableValues(index)" :key="value.name" type="button" class="flow-value-button" :aria-label="`填入 ${value.name}`" @mousedown.prevent @click="insertValue(step, value.name)">{{ value.label }} <code>{{ placeholder(value.name) }}</code></button>
            </div>
            <p class="flow-help">计算步骤返回的值也可手写为 {名字}。占位符不会自动编码特殊字符，需要时使用计算脚本。</p>
          </details>
        </div>
        <HttpResponseValues :step="step" @change="enableFlow" />
        <details class="flow-options flow-advanced" :open="hasAdvancedSettings(step)">
          <summary>请求进阶设置 <span>请求头、跳转、失败处理与等待</span></summary>
          <div class="form-group">
            <label :for="`${uid}-${step.id}-headers`">请求头（可选，每行一项）</label>
            <textarea :id="`${uid}-${step.id}-headers`" v-model="step.headers" rows="2" placeholder="Content-Type: application/x-www-form-urlencoded" @focus="activeFields[step.id] = 'headers'" @input="enableFlow"></textarea>
          </div>
          <label class="flow-redirect-toggle">
            <input v-model="step.stop_on_redirect" type="checkbox" @change="enableFlow" />
            <span>停在跳转响应（3xx），方便读取 Location；默认自动跳转</span>
          </label>
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
        </details>
      </template>
      <template v-else>
        <div class="form-group">
          <div class="field-label-row">
            <label :for="`${uid}-${step.id}-script`">计算脚本</label>
            <FieldHelp text="定义 function transform(ctx)，返回对象。ctx.vars 包含前面步骤提取的变量；ctx.username、ctx.password、ctx.isp、ctx.auth_url、ctx.page、ctx.local_ip、ctx.local_mac 继续可用。脚本在无网络与文件访问的沙箱中运行。" wide />
          </div>
          <textarea :id="`${uid}-${step.id}-script`" v-model="step.script" class="flow-script" rows="7" @input="enableFlow"></textarea>
          <span v-if="step.script === HTTP_IP_EXAMPLE_SCRIPT" class="flow-script-hint">示例中 <code>ctx.local_ip</code> 是本机 IP，原样保存为 <code>ip</code>；后面的请求需要时填 <code>{ip}</code>。</span>
          <button type="button" class="btn btn-sm" @click="step.script = HTTP_IP_EXAMPLE_SCRIPT; enableFlow()">填入本机 IP 示例</button>
          <span class="flow-script-hint">例如返回 <code>{ signed: ... }</code>，后续请求可写 <code>{signed}</code>，后续计算可读 <code>ctx.vars.signed</code>。</span>
        </div>
      </template>
      </div>
    </div>

    <div class="flow-add">
      <button type="button" class="btn btn-sm" :disabled="model.steps.length >= 16" @click="addRequest"><IconApp name="plus" class="icon-sm" /> 添加下一步请求</button>
      <button type="button" class="btn btn-sm" :disabled="model.steps.length >= 16" @click="addTransform"><IconApp name="plus" class="icon-sm" /> 添加计算脚本（进阶）</button>
    </div>
    </div>

    <!-- 侧栏与步骤同时挂载，延后定位目标；没有侧栏容器时仍在流程下方显示。 -->
    <Teleport defer :to="resultTarget || 'body'" :disabled="!resultTarget">
    <section class="flow-result" :class="{ 'flow-result--side': resultTarget }" :aria-labelledby="`${uid}-result-heading`">
      <div class="flow-result-heading">
        <span class="flow-result-icon"><IconApp name="target" class="icon-sm" /></span>
        <div>
          <h4 :id="`${uid}-result-heading`">怎样判断登录成功</h4>
          <p>先选登录请求，再选择判断方式。拿不准返回内容时使用联网检测。</p>
        </div>
      </div>
      <div class="form-row">
        <div class="form-group">
          <label :for="`${uid}-result-step`">哪个请求提交了登录</label>
          <select :id="`${uid}-result-step`" :value="model.result_step_id" @change="setResultStep">
            <option v-for="step in requestSteps" :key="step.id" :value="step.id">{{ model.steps.indexOf(step) + 1 }}. {{ step.name || step.id }}</option>
          </select>
        </div>
        <div class="form-group">
          <label>判断方式</label>
          <CustomSelect v-model="successCheck" :options="successOptions" aria-label="判断方式" />
        </div>
      </div>
      <p v-if="successCheck === 'network'" class="flow-help flow-result-help">正式登录后检查能否访问外网。下方试运行只检查请求与响应，联网结果需要在方案中登录确认。</p>
      <div v-else class="form-group">
        <label :for="`${uid}-success-pattern`">返回内容包含什么才算成功</label>
        <input :id="`${uid}-success-pattern`" v-model="model.success_pattern" type="text" placeholder="如 login success（留空按 HTTP 2xx 判断）" />
        <span class="hint">填写原文关键字；正式登录仍会验证网络是否连通。</span>
      </div>
      <details class="flow-options flow-failure" :open="!!model.failure_pattern || model.failure_action !== 'credential'">
        <summary>失败提示与处理 <span>可选</span></summary>
        <div class="form-group">
          <label :for="`${uid}-failure-pattern`">返回内容包含什么表示失败</label>
          <input :id="`${uid}-failure-pattern`" v-model="model.failure_pattern" type="text" placeholder="如密码错误" />
          <span class="hint">如果同时出现成功与失败提示，优先按失败处理。</span>
        </div>
        <div class="form-group">
          <label>出现该提示后</label>
          <CustomSelect v-model="failureAction" :options="HTTP_FAILURE_ACTION_OPTIONS" aria-label="失败处理方式" />
        </div>
      </details>
    </section>
    </Teleport>
    </div>
  </div>
</template>

<style scoped>
.flow-editor { display: flex; flex-direction: column; gap: 22px; }
.flow-editor > .flow-connection { max-width: 1280px; }
.flow-workspace { display: grid; grid-template-columns: minmax(0, 1fr); align-items: start; gap: 24px; }
.flow-sequence { display: flex; flex-direction: column; gap: 12px; min-width: 0; }
.flow-list-heading { display: flex; align-items: center; justify-content: space-between; gap: 14px; margin-bottom: 3px; }
.flow-list-heading > div { display: flex; align-items: center; flex-wrap: wrap; gap: 10px; }
.flow-list-heading strong { font-size: 15px; font-weight: 700; }
.flow-step-count { padding: 3px 8px; border-radius: 999px; background: rgba(var(--accent-rgb), .1); color: var(--text-primary); font-size: 12px; font-variant-numeric: tabular-nums; font-weight: 650; }
.flow-list-heading .hint { font-size: 12px; }
.flow-collapse-all { flex: none; padding: 6px 9px; border: 0; border-radius: 7px; background: transparent; color: var(--text-secondary); font: inherit; font-size: 12px; white-space: nowrap; cursor: pointer; }
.flow-collapse-all:hover { color: var(--text-primary); background: var(--bg-card-elevated); }
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
.flow-step-body { display: flex; flex-direction: column; gap: 14px; padding: 20px 24px 24px; border-top: 1px solid var(--border); }
.flow-step-body > .form-group, .flow-step-body > .form-row, .flow-step-body > .note { width: 100%; }
.flow-redirect-toggle { display: flex; align-items: center; gap: 9px; color: var(--text-secondary); font-size: 12px; cursor: pointer; }
.flow-redirect-toggle input { width: 16px; height: 16px; accent-color: var(--accent); }
.flow-script-hint { color: var(--text-secondary); font-size: 12px; line-height: 1.6; }
.flow-step-body > .flow-address-row { grid-template-columns: 110px minmax(0, 1fr); }
.flow-address-row > .form-group { min-width: 0; }
.flow-add { display: flex; align-items: center; flex-wrap: wrap; gap: 9px; padding: 9px 0 0; }
.flow-result { padding: 24px; border: 1px solid var(--border); border-radius: 12px; background: var(--bg-card); }
.flow-result-heading { display: flex; align-items: flex-start; gap: 12px; padding-bottom: 18px; margin-bottom: 20px; border-bottom: 1px solid var(--border); }
.flow-result-icon { display: inline-flex; align-items: center; justify-content: center; flex: 0 0 36px; width: 36px; height: 36px; border-radius: 9px; color: var(--on-accent); background: var(--accent); }
.flow-result-icon :deep(svg) { width: 18px; height: 18px; }
.flow-result h4 { margin: 0 0 4px; font-size: 15px; }
.flow-result-heading p { margin: 0; color: var(--text-secondary); font-size: 12px; line-height: 1.5; }
.flow-result > .form-row, .flow-result > .form-group { max-width: 940px; }
.flow-result--side { position: static; padding: 20px; }
.flow-result--side .form-row { grid-template-columns: minmax(0, 1fr); gap: 0; }
.flow-result--side > .form-group:last-child { margin-bottom: 0; }
.flow-intro, .flow-help { margin: 0; color: var(--text-secondary); font-size: 12px; line-height: 1.7; overflow-wrap: anywhere; }
.flow-intro { max-width: 75ch; margin-bottom: 4px; }
.flow-step-body .form-group { margin-bottom: 0; }
.flow-options { border-top: 1px solid var(--border); }
.flow-options > summary { padding: 15px 0; cursor: pointer; font-size: 13px; font-weight: 650; }
.flow-options > summary span { margin-left: 8px; color: var(--text-secondary); font-size: 12px; font-weight: 400; }
.flow-connection { border: 0; }
.flow-connection > summary { padding-top: 0; }
.flow-advanced[open] { display: flex; flex-direction: column; gap: 14px; }
.flow-advanced[open] > summary { padding-bottom: 0; }
.flow-failure .form-group:last-child { margin-bottom: 0; }
.flow-result-help { margin-bottom: 16px; }
.flow-values { display: flex; flex-direction: column; gap: 8px; }
.flow-value-buttons { display: flex; align-items: center; flex-wrap: wrap; gap: 6px; font-size: 12px; color: var(--text-secondary); }
.flow-value-button { padding: 6px 8px; border: 1px solid var(--border); border-radius: 6px; background: var(--bg-card); color: var(--text-primary); font: inherit; font-size: 12px; cursor: pointer; }
.flow-value-button:hover { border-color: var(--accent); }
.flow-value-button code { font-size: 11px; color: var(--text-secondary); overflow-wrap: anywhere; }
.flow-other-values summary { font-size: 12px; cursor: pointer; color: var(--text-secondary); }
.flow-other-values[open] { display: flex; flex-direction: column; gap: 8px; }
.flow-options > summary:focus-visible, .flow-other-values summary:focus-visible, .flow-value-button:focus-visible { outline: 2px solid var(--accent); outline-offset: 3px; }
.flow-script { font-family: var(--font-mono); }
.flow-script-hint code { font-family: var(--font-mono); color: var(--text-primary); }
@media (min-width: 1800px) {
  .flow-workspace:has(> .flow-result) { grid-template-columns: minmax(0, 1.35fr) minmax(360px, .85fr); }
  .flow-workspace > .flow-result { position: sticky; top: 104px; }
  .flow-workspace > .flow-result .form-row { grid-template-columns: minmax(0, 1fr); gap: 0; }
}
@media (max-width: 700px) {
  .flow-step-head { align-items: stretch; flex-wrap: wrap; }
  .flow-step-toggle { min-height: 72px; }
  .flow-step-titleline { flex-wrap: wrap; row-gap: 4px; }
  .flow-step-titleline strong { white-space: normal; }
  .flow-step-actions { width: 100%; justify-content: flex-end; padding: 0 10px 8px; }
  .flow-step-body { padding: 18px 14px; }
  .flow-step-body > .flow-address-row { grid-template-columns: 96px minmax(0, 1fr); gap: 12px; }
  .flow-add { padding-left: 0; }
  .flow-result { padding: 16px; }
}
@media (max-width: 640px) {
  .flow-step-body > .flow-address-row { grid-template-columns: minmax(0, 1fr); }
  .flow-address-row > .flow-method { width: 96px; }
}
@media (prefers-reduced-motion: reduce) { .flow-step-chevron { transition: none; } }
</style>
