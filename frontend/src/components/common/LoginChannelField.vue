<script setup lang="ts">
/**
 * 登录方式选择器（渠道 + 浏览器任务）+ 直连请求参数面板（可复用）。
 *
 * 从 ProfilesView 编辑器抽出：登录方式此前只在该编辑器内可选，其他入口
 * （设置页、引导向导）无法切换。以 `v-model` 绑定一个含登录渠道与直连字段的
 * 草稿对象，宿主只负责提供草稿与保存，本组件只管编辑与测试。
 *
 * 草稿对象契约（与后端 ProfileData 同名字段）：
 *   active_task / login_channel / http_method / http_url / http_headers /
 *   http_body / http_success_pattern / http_failure_pattern / http_crypto_script
 *
 * 浏览器任务选择内聚在此处：它是「怎么登录」的一部分（浏览器渠道要指定用哪个
 * 任务操作网页），且**按方案绑定**——切方案即切任务。任务管理页只负责编辑，
 * 不再承担"启用哪个"的职责。
 *
 * `showTest` 供不落盘的场景使用；测试请求经 `useProfiles.testHttpLogin`
 * 发送，不会保存方案、也不会触发登录状态机。
 *
 * 「测试」需要账号与密码可用：账号取草稿的 username，密码留空时由后端按
 * profileId 回退该方案本机已保存凭据（见 POST /api/profiles/http-login-test）。
 */
import IconApp from "@/components/common/IconApp.vue";
import CustomSelect from "@/components/common/CustomSelect.vue";
import FieldHelp from "@/components/common/FieldHelp.vue";
import { computed, ref } from "vue";
import { useProfiles } from "@/composables/useProfiles";
import { useTaskDirectory } from "@/composables/useTaskDirectory";
import { HTTP_METHOD_OPTIONS, httpTestOutcomeLabel } from "@/utils/loginChannel";
import type { HttpLoginTestResult } from "@/api/types";
import type { SelectOption } from "@/components/common/CustomSelect.vue";

/** 本组件读写的最小字段集（宿主草稿类型可含更多字段） */
export interface LoginChannelDraft {
  /** 浏览器渠道使用的任务 ID（空 = 未绑定，登录时回退内置默认任务） */
  active_task: string;
  login_channel: "browser" | "http";
  http_method: "GET" | "POST";
  http_url: string;
  http_headers: string;
  http_body: string;
  http_success_pattern: string;
  http_failure_pattern: string;
  http_crypto_script: string;
}

const props = withDefaults(
  defineProps<{
    /** 草稿对象（原地修改，宿主脏检测基于全量序列化比对，可直接感知） */
    modelValue: LoginChannelDraft;
    /** 已保存方案 ID；无（新建/未保存）时不传，测试需手填密码 */
    profileId?: string;
    /** 草稿中的账号，用于测试请求；留空则测试按钮提示先填账号 */
    username?: string;
    /** 草稿中的密码；留空时后端回退已保存凭据 */
    password?: string;
    /** 草稿中的认证地址，作为脚本 ctx.auth_url 与登录页抓取来源 */
    authUrl?: string;
    /** 是否展示「发送测试请求」与其结果面板 */
    showTest?: boolean;
    /** 区段标题；传 null 表示由宿主自行渲染标题（无标题的紧凑场景） */
    title?: string | null;
  }>(),
  {
    profileId: undefined,
    username: "",
    password: "",
    authUrl: "",
    showTest: true,
    title: "登录方式",
  },
);

const p = useProfiles();

// 浏览器任务清单：与任务页同源（useTaskDirectory 单次拉取），此处只读展示
const { browserTasks } = useTaskDirectory();

/** 任务下拉选项：空值项显式表达"未绑定 → 登录时用内置默认任务" */
const taskOptions = computed<SelectOption[]>(() => {
  const opts: SelectOption[] = [{ value: "", label: "使用内置默认任务" }];
  for (const t of browserTasks.value) {
    opts.push({ value: t.id, label: t.name || t.id });
  }
  return opts;
});

// 测试结果自持：多实例（不同宿主/多方案）各自展示，互不覆盖
const testResult = ref<HttpLoginTestResult | null>(null);

/** 切换渠道：原地写回草稿（宿主序列化比对即可感知为未保存改动） */
function setChannel(channel: "browser" | "http"): void {
  props.modelValue.login_channel = channel;
}

const isHttp = computed(() => props.modelValue.login_channel === "http");

async function runTest(): Promise<void> {
  const draft = props.modelValue;
  if (!draft.http_url.trim()) {
    p.toastHttpTestPrecondition("请填写直连请求地址");
    return;
  }
  if (!(props.username ?? "").trim()) {
    p.toastHttpTestPrecondition("请填写账号后再测试");
    return;
  }
  testResult.value = null;
  testResult.value = await p.testHttpLogin({
    profileId: props.profileId,
    username: props.username ?? "",
    password: props.password ?? "",
    http_method: draft.http_method,
    http_url: draft.http_url,
    http_headers: draft.http_headers,
    http_body: draft.http_body,
    http_success_pattern: draft.http_success_pattern,
    http_failure_pattern: draft.http_failure_pattern,
    http_crypto_script: draft.http_crypto_script,
    auth_url: props.authUrl ?? "",
  });
}
</script>

<template>
  <div class="channel-section">
    <div v-if="title !== null" class="editor-section-label">
      {{ title }}
      <FieldHelp text="浏览器自动化兼容验证码、动态表单等复杂门户；直连请求无需 Python 与浏览器，适合可直接调用登录接口的门户。" />
    </div>

    <div class="segmented profile-channel-switch" role="group" aria-label="登录方式">
      <button type="button" :class="{ active: modelValue.login_channel === 'browser' }" @click="setChannel('browser')">
        浏览器自动化
      </button>
      <button type="button" :class="{ active: isHttp }" @click="setChannel('http')">
        直连请求
      </button>
    </div>

    <div v-if="!isHttp" class="browser-channel-panel">
      <div class="form-group">
        <div class="field-label-row">
          <label for="login-active-task">浏览器任务</label>
          <FieldHelp text="本方案自动登录时执行的任务。任务内容在「任务」页编辑；每个方案可各绑定一个，切换方案即切换任务；留空则使用内置默认任务。" />
        </div>
        <CustomSelect id="login-active-task" v-model="modelValue.active_task" :options="taskOptions" />
        <span class="hint">按方案绑定：切换方案会同时切换任务</span>
      </div>
      <p class="channel-note">
        按选定任务的步骤操作网页，适合验证码、动态表单和复杂交互。
      </p>
    </div>

    <div v-else class="http-channel-panel">
      <div class="http-channel-intro">
        <strong>直接向校园网网关发送登录请求</strong>
        <span>不启动浏览器；失败后按普通登录策略重试，不会自动切回浏览器。</span>
      </div>

      <div class="form-row http-url-row">
        <div class="form-group http-method-field">
          <label for="http-login-method">方法</label>
          <CustomSelect id="http-login-method" v-model="modelValue.http_method" :options="HTTP_METHOD_OPTIONS" />
        </div>
        <div class="form-group">
          <label for="http-login-url">请求地址</label>
          <input id="http-login-url" v-model.trim="modelValue.http_url" type="text"
            placeholder="http://10.0.0.1/login?user={username}&pass={password}" />
        </div>
      </div>
      <div v-if="modelValue.http_method === 'GET'" class="http-risk-note">
        GET 会把凭据放进地址栏。程序会脱敏自身日志，但网关、代理或系统网络日志仍可能记录完整地址；能用 POST 时优先用 POST。
      </div>

      <div class="form-group">
        <label for="http-login-headers">请求头</label>
        <textarea id="http-login-headers" v-model="modelValue.http_headers" rows="3"
          placeholder="Content-Type: application/x-www-form-urlencoded&#10;Referer: {auth_url}"></textarea>
        <span class="hint">每行一项，格式为“名称: 值”。</span>
      </div>
      <div v-if="modelValue.http_method === 'POST'" class="form-group">
        <label for="http-login-body">请求内容</label>
        <textarea id="http-login-body" v-model="modelValue.http_body" rows="4"
          placeholder="username={username}&password={password}"></textarea>
      </div>

      <div class="form-row">
        <div class="form-group">
          <label for="http-login-success">成功关键字</label>
          <input id="http-login-success" v-model="modelValue.http_success_pattern" type="text"
            placeholder="登录成功（留空则以 HTTP 2xx 判断）" />
        </div>
        <div class="form-group">
          <label for="http-login-failure">失败关键字</label>
          <input id="http-login-failure" v-model="modelValue.http_failure_pattern" type="text"
            placeholder="账号或密码错误" />
        </div>
      </div>

      <div class="form-group">
        <label for="http-login-script">凭据变换脚本（可选）</label>
        <textarea id="http-login-script" v-model="modelValue.http_crypto_script" class="http-script-editor" rows="8"
          placeholder="function transform(ctx) {&#10;  return { password: md5(ctx.password) };&#10;}"></textarea>
        <span class="hint">
          定义 transform(ctx)，可读取 username、password、auth_url、page。可用 md5、sha1、sha256、hmac_sha256、base64_encode、base64_decode、hex_encode、url_encode、now_ms。
        </span>
      </div>

      <div class="http-template-help">
        地址、请求头和请求内容支持 <code>{username}</code>、<code>{password}</code>、<code>{auth_url}</code>
        及脚本返回字段。值会原样替换，特殊字符请在脚本中使用 <code>url_encode()</code>。
      </div>

      <template v-if="showTest">
        <div class="http-test-actions">
          <button type="button" class="btn btn-secondary" :disabled="p.httpTestRunning.value" @click="runTest">
            <IconApp name="play" class="icon-sm" />
            {{ p.httpTestRunning.value ? '正在发送…' : '发送测试请求' }}
          </button>
          <span>测试不会保存方案，也不会改变自动登录状态。</span>
        </div>

        <div v-if="testResult" class="http-test-result"
          :class="testResult.outcome === 'success' ? 'success' : 'failed'">
          <div class="http-test-result-head">
            <strong>{{ httpTestOutcomeLabel(testResult.outcome) }}</strong>
            <span>{{ testResult.status ? 'HTTP ' + testResult.status : '无响应' }} · {{ testResult.duration_ms }} ms</span>
          </div>
          <p>{{ testResult.message }}</p>
          <dl>
            <template v-if="testResult.rendered_url">
              <dt>请求地址</dt><dd><code>{{ testResult.rendered_url }}</code></dd>
            </template>
            <template v-if="testResult.rendered_headers">
              <dt>请求头</dt><dd><code>{{ testResult.rendered_headers }}</code></dd>
            </template>
            <template v-if="testResult.rendered_body">
              <dt>请求内容</dt><dd><code>{{ testResult.rendered_body }}</code></dd>
            </template>
            <template v-if="testResult.response_snippet">
              <dt>响应片段</dt><dd><code>{{ testResult.response_snippet }}</code></dd>
            </template>
          </dl>
        </div>
      </template>
    </div>
  </div>
</template>

<style scoped>
/* 直连登录：保持宿主卡片的玻璃质感，用“请求报文”预览强化这是网络通道而非第二套任务编辑器。
   样式随组件迁移自 pages/profiles.css，避免仅宿主可用。 */
.profile-channel-switch {
  margin-bottom: var(--space-sm);
}

.channel-note {
  margin: 0;
  color: var(--text-muted);
  font-size: var(--text-sm);
}

/* 浏览器渠道：任务选择面板（与直连面板同构，保持两渠道视觉对等） */
.browser-channel-panel {
  margin-top: var(--space-sm);
  padding: var(--space-md);
  border: 1px solid var(--border);
  border-radius: var(--radius-lg);
  background: var(--bg-glass-light);
}

.browser-channel-panel .form-group {
  margin-bottom: var(--space-sm);
}

.http-channel-panel {
  margin-top: var(--space-sm);
  padding: var(--space-md);
  border: 1px solid var(--border-accent);
  border-radius: var(--radius-lg);
  background:
    linear-gradient(135deg, rgba(var(--accent-rgb), 0.06), transparent 42%),
    var(--bg-glass-light);
}

.http-channel-intro {
  display: flex;
  flex-direction: column;
  gap: 3px;
  margin-bottom: var(--space-md);
}

.http-channel-intro strong {
  color: var(--text-primary);
  font-size: var(--text-base);
}

.http-channel-intro span,
.http-test-actions > span {
  color: var(--text-muted);
  font-size: var(--text-sm);
}

.http-url-row {
  grid-template-columns: minmax(110px, 0.25fr) minmax(0, 1.75fr);
}

.http-method-field {
  min-width: 0;
}

.http-risk-note,
.http-template-help {
  margin: calc(-1 * var(--space-sm)) 0 var(--space-md);
  padding: 10px 12px;
  border-radius: var(--radius-md);
  font-size: var(--text-sm);
  line-height: 1.6;
}

.http-risk-note {
  color: var(--warning-text);
  border: 1px solid rgba(var(--warning-rgb), 0.25);
  background: var(--warning-bg);
}

.http-template-help {
  color: var(--text-secondary);
  border: 1px dashed var(--border-accent-strong);
  background: rgba(var(--accent-rgb), 0.04);
}

.http-template-help code,
.http-test-result code {
  font-family: var(--font-mono);
}

.http-script-editor {
  min-height: 170px;
}

.http-test-actions {
  display: flex;
  align-items: center;
  gap: var(--space-md);
  flex-wrap: wrap;
}

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
  justify-content: space-between;
  gap: var(--space-md);
  color: var(--text-primary);
}

.http-test-result-head span {
  color: var(--text-muted);
  font-family: var(--font-mono);
  font-size: var(--text-xs);
}

.http-test-result > p {
  margin: 8px 0 0;
  color: var(--text-secondary);
  font-size: var(--text-sm);
}

.http-test-result dl {
  display: grid;
  grid-template-columns: 76px minmax(0, 1fr);
  gap: 6px 10px;
  margin: var(--space-sm) 0 0;
}

.http-test-result dt {
  color: var(--text-muted);
  font-size: var(--text-xs);
}

.http-test-result dd {
  min-width: 0;
  margin: 0;
}

.http-test-result dd code {
  display: block;
  max-height: 120px;
  overflow: auto;
  white-space: pre-wrap;
  overflow-wrap: anywhere;
  color: var(--text-primary);
  font-size: var(--text-xs);
}

@media (max-width: 768px) {
  .http-url-row {
    grid-template-columns: 1fr;
  }

  .http-test-result dl {
    grid-template-columns: 1fr;
  }
}
</style>
