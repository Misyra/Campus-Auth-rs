<script setup lang="ts">
/** 从 HTTP 返回内容取值，说明来源、命名和后续使用的关系。 */
import { useId } from "vue";
import IconApp from "@/components/common/IconApp.vue";
import type { HttpFlowStep } from "@/api/types";

const props = defineProps<{ step: HttpFlowStep }>();
const emit = defineEmits<{ change: [] }>();
const uid = useId();
const sources = [
  { value: "json", label: "JSON 返回内容", field: "字段路径", example: "data.token", help: "例如返回 {\"data\":{\"token\":\"abc123\"}}，填写 data.token 就能取到 abc123；也支持 JSONP。" },
  { value: "header", label: "响应头", field: "响应头名称", example: "x-token", help: "填写服务器返回的响应头名称，例如 x-token。" },
  { value: "url", label: "最终页面地址", field: "地址中的参数名", example: "session", help: "例如最终地址是 /home?session=abc123，填写 session。" },
  { value: "redirect", label: "跳转地址（Location）", field: "跳转地址中的参数名", example: "token", help: "读取服务器要求跳转的地址，例如 /home?token=abc123 中的 token。需要停在跳转响应。" },
  { value: "html", label: "HTML 输入框", field: "输入框的 name", example: "sign", help: "例如页面中有 <input name=\"sign\" value=\"abc123\">，填写 sign。" },
  { value: "regex", label: "正文正则匹配", field: "正则表达式", example: "token=([^&]+)", help: "用括号标出要取的内容，保存第一个捕获组。例如 token=([^&]+)。" },
];
function kind(source: string): string { return source.split(":", 1)[0] || "json"; }
function detail(source: string): string { return source.includes(":") ? source.slice(source.indexOf(":") + 1) : ""; }
function option(source: string) { return sources.find((item) => item.value === kind(source)) ?? sources[0]!; }
function setKind(index: number, value: string): void {
  const rule = props.step.extracts[index];
  if (rule) { rule.source = `${value}:${detail(rule.source)}`; emit("change"); }
}
function setDetail(index: number, value: string): void {
  const rule = props.step.extracts[index];
  if (rule) { rule.source = `${kind(rule.source)}:${value}`; emit("change"); }
}
function addValue(): void {
  if (props.step.extracts.length >= 12) return;
  props.step.extracts.push({ source: "json:", name: "" });
  emit("change");
}
function removeValue(index: number): void { props.step.extracts.splice(index, 1); emit("change"); }
function stopAtRedirect(): void { props.step.stop_on_redirect = true; emit("change"); }
</script>

<template>
  <details class="response-values" :open="step.extracts.length > 0">
    <summary>从返回内容取值 <span>{{ step.extracts.length ? `已配置 ${step.extracts.length} 个` : '可选' }}</span></summary>
    <div class="response-values-body">
      <p class="response-help">后面的请求需要这一步返回的 token 等内容时，才需要取值。普通单次登录可以跳过。</p>
      <details class="response-example">
        <summary>看一个 token 示例</summary>
        <ol>
          <li>服务器返回 <code>{"data":{"token":"abc123"}}</code>。</li>
          <li>选择 JSON，字段路径填 <code>data.token</code>，名字填 <code>token</code>。</li>
          <li>下一步请求内容写 <code>token={token}</code>，发送时就会填入取到的值。</li>
        </ol>
      </details>
      <div v-for="(rule, index) in step.extracts" :key="index" class="response-rule">
        <div class="response-rule-fields">
          <div class="form-group">
            <label :for="`${uid}-source-${index}`">从哪里取</label>
            <select :id="`${uid}-source-${index}`" :value="kind(rule.source)" @change="setKind(index, ($event.target as HTMLSelectElement).value)">
              <option v-for="source in sources" :key="source.value" :value="source.value">{{ source.label }}</option>
            </select>
          </div>
          <div class="form-group">
            <label :for="`${uid}-detail-${index}`">{{ option(rule.source).field }}</label>
            <input :id="`${uid}-detail-${index}`" :value="detail(rule.source)" :placeholder="option(rule.source).example" type="text" @input="setDetail(index, ($event.target as HTMLInputElement).value)" />
          </div>
          <div class="form-group">
            <label :for="`${uid}-name-${index}`">给这个值起个名字</label>
            <input :id="`${uid}-name-${index}`" v-model.trim="rule.name" type="text" placeholder="token" @input="emit('change')" />
          </div>
          <button type="button" class="response-remove" :aria-label="`删除第 ${index + 1} 个取值`" @click="removeValue(index)"><IconApp name="trash" class="icon-sm" /></button>
        </div>
        <p class="response-help">{{ option(rule.source).help }}</p>
        <p v-if="rule.name.trim()" class="response-use">下一步起可填写 <code v-text="'{' + rule.name.trim() + '}'"></code>，例如 <code v-text="'token={' + rule.name.trim() + '}'"></code>。</p>
      </div>
      <div v-if="step.extracts.some(rule => kind(rule.source) === 'redirect') && !step.stop_on_redirect" class="response-redirect">
        <span>读取跳转地址需要让请求停在 3xx 响应。</span>
        <button type="button" class="btn btn-sm" @click="stopAtRedirect">启用停在跳转响应</button>
      </div>
      <div class="response-add">
        <button type="button" class="btn btn-sm" :disabled="step.extracts.length >= 12" @click="addValue">+ 取一个值</button>
        <span>每步最多 12 个{{ step.extracts.length ? `，已用 ${step.extracts.length} 个` : '' }}</span>
      </div>
    </div>
  </details>
</template>

<style scoped>
.response-values { border-top: 1px solid var(--border); }
.response-values > summary { padding: 15px 0; cursor: pointer; font-size: 13px; font-weight: 650; }
.response-values > summary span { margin-left: 8px; color: var(--text-secondary); font-size: 12px; font-weight: 400; }
summary:focus-visible, .response-remove:focus-visible { outline: 2px solid var(--accent); outline-offset: 3px; }
.response-values-body { display: flex; flex-direction: column; gap: 12px; padding-bottom: 14px; }
.response-help, .response-use { margin: 0; color: var(--text-secondary); font-size: 12px; line-height: 1.7; overflow-wrap: anywhere; }
.response-example { font-size: 12px; color: var(--text-secondary); }
.response-example summary { cursor: pointer; color: var(--text-primary); }
.response-example ol { padding-left: 22px; margin: 8px 0 0; line-height: 1.9; }
code { color: var(--text-primary); font-family: var(--font-mono); overflow-wrap: anywhere; }
.response-rule { display: flex; flex-direction: column; gap: 8px; padding: 14px; border: 1px solid var(--border); border-radius: 8px; }
.response-rule-fields { display: grid; grid-template-columns: minmax(0, 1fr) minmax(0, 1.25fr) minmax(0, 1fr) 28px; align-items: end; gap: 10px; }
.response-rule-fields .form-group { margin: 0; min-width: 0; }
.response-rule-fields input, .response-rule-fields select { width: 100%; }
.response-remove { width: 28px; height: 42px; display: inline-flex; align-items: center; justify-content: center; border: 0; border-radius: 6px; background: transparent; color: var(--text-secondary); cursor: pointer; }
.response-remove:hover { color: var(--danger); background: var(--danger-bg); }
.response-add, .response-redirect { display: flex; flex-wrap: wrap; align-items: center; gap: 10px; font-size: 12px; color: var(--text-secondary); }
@media (max-width: 850px) {
  .response-rule-fields { grid-template-columns: minmax(0, 1fr) 28px; }
  .response-rule-fields .form-group { grid-column: 1; }
  .response-remove { grid-column: 2; grid-row: 1; }
}
</style>
