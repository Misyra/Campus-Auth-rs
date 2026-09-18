<script setup lang="ts">
/**
 * 直连登录配置向导（分步）。
 *
 * 直连面板的字段因果链较长（地址 → 请求内容 → 判定关键字 → 可选脚本 → 验证），
 * 首次配置者常在只填了地址就点「发送测试请求」，拿到「未命中成功标识」后
 * 不知还差什么。本向导把这条链拆成四步，每步只暴露该步的字段与判断依据，
 * 并在最后一步当场发一次真实请求验证。
 *
 * 与面板共用同一份草稿对象（`v-model` 原地修改）：向导里改的每个字段立即写回
 * 宿主（方案编辑器 / 设置页），关闭向导不丢改动，宿主原有的「未保存」标记
 * 也能照常感知 —— 向导不持有第二份状态，也不负责保存。
 *
 * 所有请求经 `useProfiles.testHttpLogin`（`POST /api/profiles/http-login-test`），
 * 无状态、不落盘、不触发登录状态机。
 */
import IconApp from "@/components/common/IconApp.vue";
import CustomSelect from "@/components/common/CustomSelect.vue";
import Modal from "@/components/common/Modal.vue";
import { computed, ref, watch } from "vue";
import { usePortalDetect } from "@/composables/usePortalDetect";
import { useProfiles } from "@/composables/useProfiles";
import {
  HTTP_BODY_EXAMPLE,
  HTTP_CRYPTO_BUILTINS,
  HTTP_CRYPTO_CTX_FIELDS,
  HTTP_CRYPTO_SCRIPT_SKELETON,
  HTTP_HEADERS_EXAMPLE,
  HTTP_MAC_FORMAT_NOTE,
  HTTP_METHOD_OPTIONS,
  HTTP_TEMPLATE_PLACEHOLDERS,
  httpConfigGaps,
  httpTestOutcomeHint,
  httpTestOutcomeLabel,
  isCredentialExposedViaGet,
} from "@/utils/loginChannel";
import type { HttpLoginTestResult } from "@/api/types";
import type { LoginChannelDraft } from "@/components/common/LoginChannelField.vue";

const props = defineProps<{
  /** 与登录方式面板共用的草稿对象（原地修改） */
  draft: LoginChannelDraft;
  /** 草稿账号（测试请求用） */
  username?: string;
  /** 草稿密码（留空且为已保存方案时，后端回退本机已保存凭据） */
  password?: string;
  /** 草稿认证地址：门户检测结果与脚本 ctx.auth_url 的来源 */
  authUrl?: string;
  /** 已保存方案 ID；无则测试必须手填密码 */
  profileId?: string;
  /** 打开状态（由宿主控制，宿主负责按钮的显隐） */
  open: boolean;
}>();

const emit = defineEmits<{
  close: [];
  /**
   * 检测到的认证地址。
   *
   * 认证地址不在本组件读写的草稿字段集里（草稿契约只管登录方式与直连参数），
   * 故由宿主自己写回，本组件只上报结果。
   */
  portalDetected: [url: string];
}>();

const p = useProfiles();
const portalDetect = usePortalDetect();

/** 步骤定义：标题 + 一句话说明，步骤条与正文标题共用 */
const STEPS = [
  { key: "portal", title: "找到登录请求", hint: "确认门户地址，并让程序认出这次登录" },
  { key: "request", title: "填写请求", hint: "照抄门户登录请求的方法、地址与内容" },
  { key: "verdict", title: "设定判定", hint: "告诉程序什么样的响应算登录成功" },
  { key: "verify", title: "发送测试", hint: "当场发一次真实请求，确认配置可用" },
] as const;

const current = ref(0);

/** 切换步骤：越界钳制，避免宿主重复点触发越界索引 */
function goStep(index: number): void {
  current.value = Math.min(Math.max(index, 0), STEPS.length - 1);
}

// 每次打开回到第一步：上次停在「发送测试」页会让人以为已经验证过
watch(
  () => props.open,
  (open) => {
    if (open) {
      current.value = 0;
      testResult.value = null;
    }
  },
);

const isLastStep = computed(() => current.value === STEPS.length - 1);

/** 打开向导时顺手把渠道切到直连：入口本身即「我要用直连」的明确意图 */
watch(
  () => props.open,
  (open) => {
    if (open) props.draft.login_channel = "http";
  },
);

const testResult = ref<HttpLoginTestResult | null>(null);
const testing = computed(() => p.httpTestRunning.value);

/**
 * 逐步前置缺口：进入第 N 步前必须补齐前 N-1 步的输入。
 *
 * 分步收敛而非一次列全的原因：地址为空时谈判定关键字没有意义；而密码缺失
 * 在「已保存方案」下并不算缺失（后端回退凭据），一次列全反而会给出错误提示。
 */
const gaps = computed(() =>
  httpConfigGaps(
    {
      username: props.username,
      password: props.password,
      http_url: props.draft.http_url,
    },
    { hasSavedProfile: Boolean(props.profileId) },
  ),
);

/** 当前步骤的必填是否就绪（决定「下一步」是否可用） */
const stepReady = computed(() => {
  switch (STEPS[current.value].key) {
    // 第 1 步本身就是「确认账号可用」：账号缺失时它已经把该行标红，
    // 此时仍放行会让人走到第 4 步才发现（多步白走）
    case "portal":
      return Boolean((props.username ?? "").trim());
    case "request":
      return Boolean(props.draft.http_url.trim());
    case "verdict":
      return Boolean(props.draft.http_url.trim());
    default:
      return gaps.value.length === 0;
  }
});

/** 「下一步」被禁用时的原因（禁用而不说明等于把用户卡住） */
const blockedReason = computed(() => {
  if (stepReady.value) return "";
  switch (STEPS[current.value].key) {
    case "portal":
      return "请先关闭向导，在上方页面的「账号」一栏填写账号并保存，再回来继续";
    case "request":
    case "verdict":
      return "请先填写请求地址";
    default:
      return `还缺：${gaps.value.join("、")}`;
  }
});

const passwordInUrl = computed(() =>
  isCredentialExposedViaGet(props.draft.http_method, props.draft.http_url),
);

async function detectPortal(): Promise<void> {
  const url = await portalDetect.detectPortal();
  if (url) {
    // `authUrl` 是宿主的草稿字段而非本组件的 prop 副本，故由宿主传入的
    // setter 语义在这里不成立——改用 emit 让宿主写回自己的草稿
    emit("portalDetected", url);
  }
}

async function runTest(): Promise<void> {
  testResult.value = null;
  testResult.value = await p.testHttpLogin({
    profileId: props.profileId,
    username: props.username ?? "",
    password: props.password ?? "",
    http_method: props.draft.http_method,
    http_url: props.draft.http_url,
    http_headers: props.draft.http_headers,
    http_body: props.draft.http_body,
    http_success_pattern: props.draft.http_success_pattern,
    http_failure_pattern: props.draft.http_failure_pattern,
    http_crypto_script: props.draft.http_crypto_script,
    auth_url: props.authUrl ?? "",
    httpIgnoreHttpsErrors: props.draft.http_ignore_https_errors ?? null,
  });
}

function fillUrlExample(): void {
  props.draft.http_url = "http://10.0.0.1/login?username={username}&password={password}";
}

function fillHeadersExample(): void {
  props.draft.http_headers = HTTP_HEADERS_EXAMPLE;
}

function fillBodyExample(): void {
  props.draft.http_body = HTTP_BODY_EXAMPLE;
}

function fillScriptSkeleton(): void {
  props.draft.http_crypto_script = HTTP_CRYPTO_SCRIPT_SKELETON;
}
</script>

<template>
  <Modal :open="open" title="直连登录配置向导" size="lg" @close="emit('close')">
    <!-- 步骤条：已完成步骤可点回看，未到达步骤禁用（避免跳过必填） -->
    <ol class="wz-steps">
      <li
        v-for="(step, i) in STEPS"
        :key="step.key"
        class="wz-step"
        :class="{ active: i === current, done: i < current }"
      >
        <button
          type="button"
          class="wz-step-btn"
          :disabled="i > current"
          @click="goStep(i)"
        >
          <span class="wz-step-num">
            <IconApp v-if="i < current" name="check" class="icon-sm" />
            <template v-else>{{ i + 1 }}</template>
          </span>
          <span class="wz-step-copy">
            <strong>{{ step.title }}</strong>
            <small>{{ step.hint }}</small>
          </span>
        </button>
      </li>
    </ol>

    <div class="wz-body">
      <!-- 步骤 1：门户地址与账号 -->
      <section v-if="STEPS[current].key === 'portal'" class="wz-page">
        <h4>确认你的校园网账号与认证地址</h4>
        <p class="wz-lead">
          直连请求要打到校园网网关的登录接口上，先确认账号与门户地址可用。地址只用于脚本读取页面原文，
          不填也能登录（除非脚本需要它）。
        </p>

        <dl class="wz-facts">
          <div>
            <dt>账号</dt>
            <dd>
              <template v-if="username">{{ username }}</template>
              <span v-else class="wz-missing">未填写 —— 请回到页面「账号」一栏填写后保存</span>
            </dd>
          </div>
          <div>
            <dt>密码</dt>
            <dd>
              <template v-if="password">已填写</template>
              <template v-else-if="profileId">未填写 —— 已有方案会使用本机已保存的密码</template>
              <span v-else class="wz-missing">未填写 —— 请回到页面「密码」一栏填写</span>
            </dd>
          </div>
          <div>
            <dt>认证地址</dt>
            <dd>
              <template v-if="authUrl"><code>{{ authUrl }}</code></template>
              <span v-else class="wz-muted">未填写（可选）</span>
            </dd>
          </div>
        </dl>

        <div class="wz-actions-inline">
          <button type="button" class="btn btn-sm btn-secondary" :disabled="portalDetect.detecting.value" @click="detectPortal">
            <IconApp name="globe" class="icon-sm" />
            {{ portalDetect.detecting.value ? '检测中…' : '自动检测认证地址' }}
          </button>
          <span class="wz-hint">需先断开校园网登录再检测（已在线时没有跳转可抓）</span>
        </div>

        <div class="wz-actions-inline">
          <a class="btn btn-sm btn-ghost" href="https://campus-auth.misyra.com/docs/profiles/http-login" target="_blank" rel="noopener noreferrer">
            <IconApp name="file-text" class="icon-sm" />
            打开完整使用文档
          </a>
        </div>
      </section>

      <!-- 步骤 2：请求方法与地址 -->
      <section v-else-if="STEPS[current].key === 'request'" class="wz-page">
        <h4>照抄门户的登录请求</h4>
        <p class="wz-lead">
          在浏览器里打开校园网登录页，按 <kbd>F12</kbd> 打开开发者工具 → 切到
          <strong>Network / 网络</strong> → 勾选 <strong>Preserve log / 保留日志</strong> →
          输入错误的密码并点登录 → 在请求列表里找到提交账号密码的那条（通常是
          <code>login</code> / <code>portal</code> 之类的地址）→ 右键复制它的
          <strong>URL</strong> 与 <strong>Form Data / 表单数据</strong>。
        </p>

        <div class="form-row wz-url-row">
          <div class="form-group">
            <label for="wz-method">请求方法</label>
            <CustomSelect id="wz-method" v-model="draft.http_method" :options="HTTP_METHOD_OPTIONS" />
            <span class="hint">开发者工具里 Request Method 是什么就选什么</span>
          </div>
          <div class="form-group">
            <div class="field-label-row">
              <label for="wz-url">请求地址</label>
              <button type="button" class="btn btn-link wz-fill-btn" @click="fillUrlExample">填入示例</button>
            </div>
            <input id="wz-url" v-model.trim="draft.http_url" type="text"
              placeholder="http://10.0.0.1/login?username={username}&password={password}" />
            <span class="hint">把地址里出现的账号、密码原样换成占位符（下方说明）</span>
          </div>
        </div>

        <div v-if="passwordInUrl" class="wz-warn">
          <IconApp name="alert-triangle" class="icon-sm" />
          <span>
            地址里带了密码，它会出现在网关、代理与系统网络日志中。程序只保证自身日志脱敏；
            如果门户同时支持 POST，建议改用 POST 把凭据放进请求内容。
          </span>
        </div>

        <div class="wz-card">
          <strong>可用的占位符</strong>
          <div class="wz-chips">
            <code v-for="ph in HTTP_TEMPLATE_PLACEHOLDERS" :key="ph" class="wz-chip">{{ ph }}</code>
          </div>
          <p class="wz-note">
            这些词在发送前会被替换成真实值。<strong>值原样替换、不做转义</strong>，
            需要 URL 编码时用脚本里的 <code>url_encode()</code>。写错的占位符会原样发出去，
            下一步的测试结果里能直接看到。
          </p>
          <p class="wz-note">{{ HTTP_MAC_FORMAT_NOTE }}</p>
        </div>
      </section>

      <!-- 步骤 3：请求内容与判定关键字 -->
      <section v-else-if="STEPS[current].key === 'verdict'" class="wz-page">
        <h4>补齐请求内容，并告诉程序怎么算成功</h4>
        <p class="wz-lead">
          从同一份 Form Data 里把字段名抄到请求内容里。登录请求多数还要带
          <code>Content-Type</code>，部分门户校验 <code>Referer</code>，缺了会被直接拒绝。
        </p>

        <div class="form-row">
          <div class="form-group">
            <div class="field-label-row">
              <label for="wz-headers">请求头</label>
              <button type="button" class="btn btn-link wz-fill-btn" @click="fillHeadersExample">填入示例</button>
            </div>
            <textarea id="wz-headers" v-model="draft.http_headers" rows="3"
              :placeholder="HTTP_HEADERS_EXAMPLE"></textarea>
            <span class="hint">每行一项，格式为“名称: 值”。GET 请求一般可以留空。</span>
          </div>
          <div v-if="draft.http_method === 'POST'" class="form-group">
            <div class="field-label-row">
              <label for="wz-body">请求内容</label>
              <button type="button" class="btn btn-link wz-fill-btn" @click="fillBodyExample">填入示例</button>
            </div>
            <textarea id="wz-body" v-model="draft.http_body" rows="3"
              :placeholder="HTTP_BODY_EXAMPLE"></textarea>
            <span class="hint">照抄表单字段名，把值换成占位符。</span>
          </div>
        </div>

        <div class="form-row">
          <div class="form-group">
            <label for="wz-success">成功关键字</label>
            <input id="wz-success" v-model="draft.http_success_pattern" type="text"
              placeholder="登录成功（留空则以 HTTP 2xx 判断）" />
          </div>
          <div class="form-group">
            <label for="wz-failure">失败关键字</label>
            <input id="wz-failure" v-model="draft.http_failure_pattern" type="text"
              placeholder="账号或密码错误" />
          </div>
        </div>

        <div class="wz-card wz-card--important">
          <strong>怎么填这两个关键字</strong>
          <p class="wz-note">
            先故意输错密码试一次，看响应里出现的文字。多数门户会返回「密码错误」「登录失败」之类的提示，
            那正是<strong>失败关键字</strong>；反过来，用正确密码登录后响应里出现的
            「登录成功」「认证成功」就是<strong>成功关键字</strong>。
          </p>
          <p class="wz-note">
            <strong>特别提醒：</strong>有些门户（Dr.COM / eportal 这类 JSONP 接口）无论成功失败都返回
            <code>HTTP 200</code>。这类门户<strong>必须</strong>填写成功与失败关键字，
            否则密码输错也会被判成登录成功。
          </p>
        </div>

        <details class="wz-advanced">
          <summary>门户要求密码加密？展开配置凭据变换脚本</summary>
          <p class="wz-note">
            如果密码不是明文提交（请求里的密码字段是一串看不懂的十六进制或 Base64），
            需要把门户的加密逻辑搬进脚本。步骤 4 的测试结果里能看到实际发出的请求，方便对照。
          </p>
          <div class="form-group">
            <div class="field-label-row">
              <label for="wz-script">脚本内容</label>
              <button type="button" class="btn btn-link wz-fill-btn" @click="fillScriptSkeleton">填入骨架</button>
            </div>
            <textarea id="wz-script" v-model="draft.http_crypto_script" class="wz-script-editor" rows="8"
              placeholder="function transform(ctx) {
  return { password: md5(ctx.password) };
}"></textarea>
          </div>
          <div class="wz-card">
            <strong>脚本契约</strong>
            <div class="wz-chips">
              <span class="wz-chip-label">可读入参 ctx</span>
              <code v-for="f in HTTP_CRYPTO_CTX_FIELDS" :key="f" class="wz-chip">{{ f }}</code>
            </div>
            <div class="wz-chips">
              <span class="wz-chip-label">可用函数</span>
              <code v-for="fn in HTTP_CRYPTO_BUILTINS" :key="fn" class="wz-chip wz-chip--fn">{{ fn }}</code>
            </div>
            <p class="wz-note">
              脚本需定义 <code>transform(ctx)</code>，返回对象的字段可直接用
              <code>{字段名}</code> 引用。没有网络与文件访问能力，执行上限 500 毫秒；
              <code>page</code>、<code>local_ip</code>、<code>local_mac</code> 取不到时是空串，脚本要能容忍。
            </p>
            <p class="wz-note">{{ HTTP_MAC_FORMAT_NOTE }}</p>
          </div>
        </details>
      </section>

      <!-- 步骤 4：发送测试 -->
      <section v-else class="wz-page">
        <h4>发一次真实请求确认配置可用</h4>
        <p class="wz-lead">
          这一步会按上面的配置真的向门户发一次登录请求。<strong>不会保存方案</strong>，
          也不会改变自动登录状态；测试成功后再点页面上的「保存方案」即可启用。
        </p>

        <ul v-if="gaps.length" class="wz-gaps">
          <li v-for="gap in gaps" :key="gap">还缺：{{ gap }}</li>
        </ul>

        <div class="wz-test-bar">
          <button type="button" class="btn btn-primary" :disabled="testing || gaps.length > 0" @click="runTest">
            <IconApp :name="testing ? 'refresh' : 'play'" class="icon-sm" :class="{ spin: testing }" />
            {{ testing ? '正在发送…' : '发送测试请求' }}
          </button>
          <span class="wz-hint">仓库/网关响应较慢时最长等待 20 秒</span>
        </div>

        <div v-if="testResult" class="wz-result" :class="testResult.outcome === 'success' ? 'success' : 'failed'">
          <div class="wz-result-head">
            <IconApp :name="testResult.outcome === 'success' ? 'check-circle' : 'alert-triangle'" />
            <strong>{{ httpTestOutcomeLabel(testResult.outcome) }}</strong>
            <span>{{ testResult.status ? `HTTP ${testResult.status}` : "无响应" }} · {{ testResult.duration_ms }} ms</span>
          </div>
          <p class="wz-result-message">{{ testResult.message }}</p>
          <p class="wz-result-hint">{{ httpTestOutcomeHint(testResult.outcome) }}</p>

          <div v-if="testResult.script_error" class="wz-result-error">
            <strong>脚本错误</strong>
            <code>{{ testResult.script_error }}</code>
          </div>

          <details class="wz-detail">
            <summary>查看实际发出的请求与响应</summary>
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
            <p class="wz-note">账号与密码已替换为 <code>***</code>，脚本产出的值同样已脱敏。</p>
          </details>
        </div>

        <div v-if="testResult?.outcome === 'success'" class="wz-done">
          <IconApp name="check-circle" />
          <div>
            <strong>配置可用</strong>
            <span>关闭本向导并点页面上的「保存方案」，自动登录就会走直连，无需 Python 与浏览器。</span>
          </div>
        </div>
      </section>
    </div>

    <!-- 底部导航：步骤条可点回看，但「下一步」仍按当前步的必填把关，
         避免跳步后在第 4 步才发现必填项没填 -->
    <template #footer>
      <span v-if="blockedReason" class="wz-blocked">{{ blockedReason }}</span>
      <button type="button" class="btn btn-secondary" :disabled="current === 0" @click="goStep(current - 1)">
        上一步
      </button>
      <button v-if="!isLastStep" type="button" class="btn btn-primary" :disabled="!stepReady" @click="goStep(current + 1)">
        下一步
      </button>
      <button v-else type="button" class="btn btn-primary" @click="emit('close')">
        {{ testResult?.outcome === "success" ? "完成" : "关闭" }}
      </button>
    </template>
  </Modal>
</template>

<style scoped>
/* 向导样式：步骤条纵向排列在左、正文在右的紧凑布局在窄弹窗里会挤压，
   故改为步骤条横排 + 正文通栏，与 Modal 的宽度（lg=720px）匹配。 */
.wz-steps {
  display: grid;
  grid-template-columns: repeat(4, minmax(0, 1fr));
  gap: var(--space-sm);
  margin: 0 0 var(--space-lg);
  padding: 0;
  list-style: none;
}

.wz-step {
  min-width: 0;
}

.wz-step-btn {
  display: flex;
  align-items: flex-start;
  gap: 8px;
  width: 100%;
  height: 100%;
  padding: 10px;
  border: 1px solid var(--border);
  border-radius: var(--radius-md);
  background: var(--bg-glass-light);
  color: var(--text-secondary);
  text-align: left;
  cursor: pointer;
  transition: border-color var(--dur-base) var(--ease-out), background var(--dur-base) var(--ease-out);
}

.wz-step-btn:disabled {
  cursor: not-allowed;
  opacity: 0.55;
}

.wz-step.active .wz-step-btn {
  border-color: var(--accent);
  background: rgba(var(--accent-rgb), 0.08);
}

.wz-step.done .wz-step-btn {
  border-color: var(--success-border);
}

.wz-step-num {
  flex-shrink: 0;
  display: inline-flex;
  align-items: center;
  justify-content: center;
  width: 20px;
  height: 20px;
  border-radius: var(--radius-full);
  background: rgba(var(--slate-rgb), 0.15);
  color: var(--text-muted);
  font-size: var(--text-sm);
  font-weight: 700;
}

.wz-step.active .wz-step-num {
  background: rgba(var(--accent-rgb), 0.18);
  color: var(--accent);
}

.wz-step.done .wz-step-num {
  background: var(--success);
  color: var(--text-on-accent);
}

.wz-step-copy {
  display: flex;
  flex-direction: column;
  gap: 2px;
  min-width: 0;
}

.wz-step-copy strong {
  color: var(--text-primary);
  font-size: var(--text-md);
  font-weight: 600;
}

.wz-step-copy small {
  color: var(--text-muted);
  font-size: var(--text-xs);
  line-height: 1.4;
}

.wz-page h4 {
  margin: 0 0 var(--space-sm);
  color: var(--text-primary);
  font-size: var(--text-lg);
}

.wz-lead {
  margin: 0 0 var(--space-md);
  color: var(--text-secondary);
  font-size: var(--text-md);
  line-height: 1.75;
}

.wz-lead kbd {
  padding: 1px 5px;
  border: 1px solid var(--border);
  border-radius: var(--radius-xs);
  background: var(--bg-secondary);
  color: var(--text-primary);
  font-family: var(--font-mono);
  font-size: var(--text-xs);
}

.wz-lead code,
.wz-note code,
.wz-card strong code {
  font-family: var(--font-mono);
  font-size: 0.95em;
}

.wz-facts {
  display: flex;
  flex-direction: column;
  gap: 8px;
  margin: 0 0 var(--space-md);
  padding: var(--space-md);
  border: 1px solid var(--border);
  border-radius: var(--radius-md);
  background: var(--bg-glass-light);
}

.wz-facts > div {
  display: grid;
  grid-template-columns: 76px minmax(0, 1fr);
  gap: 10px;
  align-items: baseline;
}

.wz-facts dt {
  color: var(--text-muted);
  font-size: var(--text-sm);
}

.wz-facts dd {
  margin: 0;
  color: var(--text-primary);
  font-size: var(--text-md);
  overflow-wrap: anywhere;
}

.wz-facts dd code {
  font-family: var(--font-mono);
  font-size: var(--text-sm);
}

.wz-missing {
  color: var(--warning-text);
}

.wz-muted {
  color: var(--text-muted);
}

.wz-actions-inline {
  display: flex;
  align-items: center;
  gap: var(--space-sm);
  flex-wrap: wrap;
  margin-top: var(--space-sm);
}

.wz-hint {
  color: var(--text-muted);
  font-size: var(--text-sm);
}

/* 「下一步」被禁用的原因：靠左推到底部导航行首，与右侧按钮拉开距离 */
.wz-blocked {
  flex: 1;
  margin-right: auto;
  color: var(--warning-text);
  font-size: var(--text-sm);
  line-height: 1.5;
}

.wz-url-row {
  grid-template-columns: minmax(120px, 0.3fr) minmax(0, 1.7fr);
}

.wz-fill-btn {
  padding: 0;
  font-size: var(--text-xs);
  font-weight: 600;
}

.wz-warn {
  display: flex;
  align-items: flex-start;
  gap: var(--space-sm);
  padding: 10px 12px;
  margin-bottom: var(--space-md);
  border: 1px solid rgba(var(--warning-rgb), 0.25);
  border-radius: var(--radius-md);
  background: var(--warning-bg);
  color: var(--warning-text);
  font-size: var(--text-sm);
  line-height: 1.6;
}

.wz-warn svg {
  flex-shrink: 0;
  margin-top: 2px;
}

.wz-card {
  margin-top: var(--space-md);
  padding: var(--space-md);
  border: 1px solid var(--border);
  border-radius: var(--radius-md);
  background: rgba(var(--slate-rgb), 0.05);
}

.wz-card strong {
  display: block;
  margin-bottom: 8px;
  color: var(--text-primary);
  font-size: var(--text-md);
}

.wz-card--important {
  border-color: var(--border-accent-strong);
  background: rgba(var(--accent-rgb), 0.05);
}

.wz-note {
  margin: 8px 0 0;
  color: var(--text-secondary);
  font-size: var(--text-sm);
  line-height: 1.7;
}

.wz-chips {
  display: flex;
  align-items: center;
  flex-wrap: wrap;
  gap: 6px;
  margin-top: 6px;
}

.wz-chip-label {
  color: var(--text-muted);
  font-size: var(--text-xs);
}

.wz-chip {
  padding: 2px 8px;
  border: 1px solid var(--border);
  border-radius: var(--radius-sm);
  background: var(--bg-secondary);
  color: var(--text-primary);
  font-family: var(--font-mono);
  font-size: var(--text-xs);
}

.wz-chip--fn {
  color: var(--accent);
}

.wz-advanced {
  margin-top: var(--space-md);
  padding: 10px 12px;
  border: 1px dashed var(--border);
  border-radius: var(--radius-md);
}

.wz-advanced summary {
  color: var(--accent);
  font-size: var(--text-md);
  font-weight: 600;
  cursor: pointer;
}

.wz-script-editor {
  min-height: 150px;
}

.wz-gaps {
  margin: 0 0 var(--space-md);
  padding: 10px 12px 10px 28px;
  border: 1px solid rgba(var(--warning-rgb), 0.25);
  border-radius: var(--radius-md);
  background: var(--warning-bg);
  color: var(--warning-text);
  font-size: var(--text-sm);
  line-height: 1.7;
}

.wz-test-bar {
  display: flex;
  align-items: center;
  gap: var(--space-md);
  flex-wrap: wrap;
}

.wz-result {
  margin-top: var(--space-md);
  padding: var(--space-md);
  border: 1px solid var(--danger-border);
  border-radius: var(--radius-md);
  background: var(--danger-bg);
}

.wz-result.success {
  border-color: var(--success-border);
  background: var(--success-bg);
}

.wz-result-head {
  display: flex;
  align-items: center;
  gap: var(--space-sm);
  flex-wrap: wrap;
}

.wz-result-head svg {
  width: 20px;
  height: 20px;
  color: var(--error);
}

.wz-result.success .wz-result-head svg {
  color: var(--success);
}

.wz-result-head strong {
  color: var(--text-primary);
  font-size: var(--text-base);
}

.wz-result-head span {
  color: var(--text-muted);
  font-family: var(--font-mono);
  font-size: var(--text-xs);
}

.wz-result-message {
  margin: 8px 0 0;
  color: var(--text-secondary);
  font-size: var(--text-sm);
  overflow-wrap: anywhere;
}

.wz-result-hint {
  margin: 8px 0 0;
  padding: 8px 10px;
  border-radius: var(--radius-sm);
  background: rgba(var(--slate-rgb), 0.08);
  color: var(--text-secondary);
  font-size: var(--text-sm);
  line-height: 1.65;
}

.wz-result-error {
  display: flex;
  flex-direction: column;
  gap: 4px;
  margin-top: var(--space-sm);
  padding: 8px 10px;
  border-radius: var(--radius-sm);
  border: 1px solid var(--danger-border);
  background: rgba(var(--error-rgb), 0.06);
}

.wz-result-error strong {
  color: var(--error);
  font-size: var(--text-sm);
}

.wz-result-error code {
  color: var(--text-secondary);
  font-family: var(--font-mono);
  font-size: var(--text-xs);
  overflow-wrap: anywhere;
}

.wz-detail {
  margin-top: var(--space-sm);
}

.wz-detail summary {
  color: var(--accent);
  font-size: var(--text-sm);
  cursor: pointer;
}

.wz-detail dl {
  display: grid;
  grid-template-columns: 76px minmax(0, 1fr);
  gap: 6px 10px;
  margin: var(--space-sm) 0 0;
}

.wz-detail dt {
  color: var(--text-muted);
  font-size: var(--text-xs);
}

.wz-detail dd {
  min-width: 0;
  margin: 0;
}

.wz-detail dd code {
  display: block;
  max-height: 120px;
  overflow: auto;
  white-space: pre-wrap;
  overflow-wrap: anywhere;
  color: var(--text-primary);
  font-family: var(--font-mono);
  font-size: var(--text-xs);
}

.wz-done {
  display: flex;
  align-items: flex-start;
  gap: var(--space-sm);
  margin-top: var(--space-md);
  padding: 12px var(--space-md);
  border: 1px solid var(--success-border);
  border-radius: var(--radius-md);
  background: var(--success-bg);
}

.wz-done svg {
  flex-shrink: 0;
  width: 20px;
  height: 20px;
  color: var(--success);
}

.wz-done strong {
  display: block;
  color: var(--text-primary);
  font-size: var(--text-md);
}

.wz-done span {
  color: var(--text-secondary);
  font-size: var(--text-sm);
  line-height: 1.6;
}

@media (max-width: 640px) {
  .wz-steps {
    grid-template-columns: 1fr 1fr;
  }

  .wz-url-row,
  .wz-facts > div,
  .wz-detail dl {
    grid-template-columns: 1fr;
  }
}
</style>
