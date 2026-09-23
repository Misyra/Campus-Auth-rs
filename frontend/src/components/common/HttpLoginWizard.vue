<script setup lang="ts">
/**
 * 直连登录配置向导（分步）。
 *
 * 直连任务的字段因果链较长（请求地址 → 请求内容 → 判定关键字 → 可选脚本 → 验证），
 * 首次配置者常在只填了地址就点「发送测试请求」，拿到「未命中成功标识」后
 * 不知还差什么。本向导把这条链拆成四步，每步只暴露该步的字段与判断依据，
 * 并在最后一步当场发一次真实请求验证。
 *
 * 编辑对象是**任务的草稿**（宿主为任务页的直连任务编辑器）：向导不持有第二份状态，
 * 改的每个字段立即写回宿主，关闭向导不丢改动，宿主原有的「未保存」标记也能照常感知。
 * 任务名与备注不在向导里编辑（任务页编辑器才是全量编辑入口），故此处只读展示；
 * 认证地址是任务自带字段，向导里可填。
 *
 * 任务不含凭据：账号与密码只用于第 4 步的测试，由宿主经 `testUsername` /
 * `testPassword` 传入，不进任务、不落盘。
 *
 * 发送经 `useHttpTaskTest.runHttpTaskTest`（`POST /api/http-tasks/test`），用
 * `httpTaskPayload(draft)` 内联未落盘草稿（故不需要任务 ID 或方案上下文）；
 * 无状态、不落盘、不触发登录状态机，结论交给 `HttpTestResult` 渲染。
 */
import IconApp from "@/components/common/IconApp.vue";
import CustomSelect from "@/components/common/CustomSelect.vue";
import HttpTestResult from "@/components/common/HttpTestResult.vue";
import Modal from "@/components/common/Modal.vue";
import { computed, ref, watch } from "vue";
import { useHttpTaskTest } from "@/composables/useHttpTaskTest";
import { useToast } from "@/composables/useToast";
import { httpTaskPayload } from "@/utils/httpTask";
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
  isCredentialExposedViaGet,
} from "@/utils/loginChannel";
import type { HttpTaskDraft } from "@/utils/httpTask";

const props = defineProps<{
  /** 直连任务草稿（原地修改，宿主按同一份对象感知未保存改动） */
  draft: HttpTaskDraft;
  /** 打开状态（由宿主控制，宿主负责入口按钮的显隐） */
  open: boolean;
  /** 测试用账号：任务不含凭据，账号密码只在测试时用一次 */
  testUsername?: string;
  /** 测试用密码；留空且宿主声明已有已保存配置时，后端回退本机已保存凭据 */
  testPassword?: string;
  /** 仅用于「密码可留空」的提示口径：任务页没有已保存方案，应传 false 或不传 */
  hasSavedProfile?: boolean;
}>();

const emit = defineEmits<{
  close: [];
}>();

const { toastOnly } = useToast();
const { running, result: testResult, runHttpTaskTest, clearTestResult } = useHttpTaskTest();

/** 步骤定义：标题 + 一句话说明，步骤条与正文标题共用 */
const STEPS = [
  { key: "portal", title: "确认任务", hint: "看一眼任务名与认证地址，认证地址可留空" },
  { key: "request", title: "填写请求", hint: "照抄门户登录请求的方法、地址与内容" },
  { key: "verdict", title: "设定判定", hint: "告诉程序什么样的响应算登录成功" },
  { key: "verify", title: "发送测试", hint: "当场发一次真实请求，确认配置可用" },
] as const;

const current = ref(0);

/** 切换步骤：越界钳制，避免宿主重复点触发越界索引 */
function goStep(index: number): void {
  current.value = Math.min(Math.max(index, 0), STEPS.length - 1);
}

// 每次打开回到第一步并清掉上一次结论：停在「发送测试」页会让人以为已经验证过，
// 上一个任务留下的结论也会被误读成当前任务的
watch(
  () => props.open,
  (open) => {
    if (!open) return;
    current.value = 0;
    clearTestResult();
  },
);

const isLastStep = computed(() => current.value === STEPS.length - 1);

/**
 * 逐步前置缺口：进入第 N 步前必须补齐前 N-1 步的输入。
 *
 * 分步收敛而非一次列全的原因：请求地址为空时谈判定关键字没有意义；而密码缺失
 * 在「已有已保存配置」的情况下并不算缺失（后端回退已保存凭据），一次列全反而会
 * 给出错误提示。
 */
const gaps = computed(() =>
  httpConfigGaps(
    {
      url: props.draft.url,
      username: props.testUsername,
      password: props.testPassword,
    },
    { hasSavedProfile: Boolean(props.hasSavedProfile) },
  ),
);

/** 当前步骤的必填是否就绪（决定「下一步」是否可用） */
const stepReady = computed(() => {
  switch (STEPS[current.value].key) {
    // 第 1 步只是确认：任务名在编辑器里改、认证地址可留空，故没有必填项
    case "portal":
      return true;
    case "request":
    case "verdict":
      return Boolean(props.draft.url.trim());
    default:
      return gaps.value.length === 0;
  }
});

/** 「下一步」被禁用时的原因（禁用而不说明等于把用户卡住） */
const blockedReason = computed(() => {
  if (stepReady.value) return "";
  switch (STEPS[current.value].key) {
    case "request":
    case "verdict":
      return "请先填写请求地址";
    default:
      return `还缺：${gaps.value.join("、")}`;
  }
});

const passwordInUrl = computed(() =>
  isCredentialExposedViaGet(props.draft.method, props.draft.url),
);

async function runTest(): Promise<void> {
  // 任务里没有凭据，账号与密码由宿主传入；缺了后端只会回一个 400，
  // 先在此拦下并说清缺什么（缺口清单同时展示在本段上方）
  if (gaps.value.length > 0) {
    toastOnly(false, `无法发送测试：还缺 ${gaps.value.join("、")}`);
    return;
  }
  // 结果由 useHttpTaskTest 的共享 ref 承载（渲染交给 HttpTestResult）：
  // 未落盘草稿以 task 内联发送，故不需要任务 ID 或方案上下文
  await runHttpTaskTest({
    task: httpTaskPayload(props.draft),
    username: props.testUsername ?? "",
    password: props.testPassword ?? "",
    fetch_page: true,
  });
}

function fillUrlExample(): void {
  props.draft.url = "http://10.0.0.1/login?username={username}&password={password}";
}

function fillHeadersExample(): void {
  props.draft.headers = HTTP_HEADERS_EXAMPLE;
}

function fillBodyExample(): void {
  props.draft.body = HTTP_BODY_EXAMPLE;
}

function fillScriptSkeleton(): void {
  props.draft.crypto_script = HTTP_CRYPTO_SCRIPT_SKELETON;
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
      <!-- 步骤 1：任务是编辑对象，账号密码只在测试时用一次 -->
      <section v-if="STEPS[current].key === 'portal'" class="wz-page">
        <h4>确认这个直连任务与认证地址</h4>
        <p class="wz-lead">
          直连请求要打到校园网网关的登录接口上。本向导编辑的是<strong>直连任务</strong>本身
          （请求地址、判定关键字等），所以任务页上那一份草稿就是这里的编辑对象。
          账号与密码不属于任务——同一门户的不同账号共用一份任务——它们只在最后一步的测试里用一次。
        </p>

        <dl class="wz-facts">
          <div>
            <dt>任务名</dt>
            <dd>
              <template v-if="draft.name.trim()">{{ draft.name }}</template>
              <span v-else class="wz-missing">未命名</span>
              <span class="wz-muted"> · 任务名与备注请在右侧编辑器里改</span>
            </dd>
          </div>
          <div>
            <dt>测试账号</dt>
            <dd>
              <template v-if="testUsername">{{ testUsername }}</template>
              <span v-else class="wz-missing">
                未填写 —— 请在右侧编辑器的测试账号一栏填写（仅用于发送测试，不会存进任务）
              </span>
            </dd>
          </div>
          <div>
            <dt>测试密码</dt>
            <dd>
              <template v-if="testPassword">已填写</template>
              <template v-else-if="hasSavedProfile">未填写 —— 已有已保存配置，会使用本机已保存的密码</template>
              <span v-else class="wz-missing">
                未填写 —— 请在右侧编辑器的测试密码一栏填写（仅用于发送测试，不会存进任务）
              </span>
            </dd>
          </div>
        </dl>

        <div class="form-group">
          <label for="wz-auth-url">认证地址</label>
          <input id="wz-auth-url" v-model.trim="draft.auth_url" type="text"
            placeholder="http://10.0.0.1/（可选）" />
          <span class="hint">门户登录页的地址（浏览器里打开的那个页面），不是下一步的登录接口地址。</span>
        </div>

        <div class="wz-card">
          <strong>认证地址可以留空吗</strong>
          <p class="wz-note">
            可以。留空时运行时回退用方案的认证地址，自动登录照旧工作，所以本地自用不必填。
            它的用途有两个：作为脚本里的 <code>ctx.auth_url</code>，以及配置了加密脚本时抓取登录页原文。
          </p>
          <p class="wz-note">
            把任务分享给别人时建议填上——对方的方案里未必有认证页地址，否则接手后还得自己摸出来。
          </p>
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
            <CustomSelect id="wz-method" v-model="draft.method" :options="HTTP_METHOD_OPTIONS" />
            <span class="hint">开发者工具里 Request Method 是什么就选什么</span>
          </div>
          <div class="form-group">
            <div class="field-label-row">
              <label for="wz-url">请求地址</label>
              <button type="button" class="btn btn-link wz-fill-btn" @click="fillUrlExample">填入示例</button>
            </div>
            <input id="wz-url" v-model.trim="draft.url" type="text"
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
            <textarea id="wz-headers" v-model="draft.headers" rows="3"
              :placeholder="HTTP_HEADERS_EXAMPLE"></textarea>
            <span class="hint">每行一项，格式为“名称: 值”。GET 请求一般可以留空。</span>
          </div>
          <div v-if="draft.method === 'POST'" class="form-group">
            <div class="field-label-row">
              <label for="wz-body">请求内容</label>
              <button type="button" class="btn btn-link wz-fill-btn" @click="fillBodyExample">填入示例</button>
            </div>
            <textarea id="wz-body" v-model="draft.body" rows="3"
              :placeholder="HTTP_BODY_EXAMPLE"></textarea>
            <span class="hint">照抄表单字段名，把值换成占位符。</span>
          </div>
        </div>

        <div class="form-row">
          <div class="form-group">
            <label for="wz-success">成功关键字</label>
            <input id="wz-success" v-model="draft.success_pattern" type="text"
              placeholder="登录成功（留空则以 HTTP 2xx 判断）" />
          </div>
          <div class="form-group">
            <label for="wz-failure">失败关键字</label>
            <input id="wz-failure" v-model="draft.failure_pattern" type="text"
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
            <textarea id="wz-script" v-model="draft.crypto_script" class="wz-script-editor" rows="8"
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

      <!-- 步骤 4：发送测试（未落盘的任务草稿内联发出） -->
      <section v-else class="wz-page">
        <h4>发一次真实请求确认配置可用</h4>
        <p class="wz-lead">
          这一步会按上面的配置真的向门户发一次登录请求，用的是右侧编辑器里的测试账号与密码。
          <strong>不会保存任务</strong>，也不会改变自动登录状态；测试通过后再保存任务即可生效。
        </p>

        <ul v-if="gaps.length" class="wz-gaps">
          <li v-for="gap in gaps" :key="gap">还缺：{{ gap }}</li>
        </ul>

        <div class="wz-test-bar">
          <button type="button" class="btn btn-primary" :disabled="running" @click="runTest">
            <IconApp :name="running ? 'refresh' : 'play'" class="icon-sm" :class="{ spin: running }" />
            {{ running ? '正在发送…' : '发送测试请求' }}
          </button>
          <span class="wz-hint">仓库/网关响应较慢时最长等待 20 秒</span>
        </div>

        <HttpTestResult v-if="testResult" :result="testResult" />

        <div v-if="testResult?.outcome === 'success'" class="wz-done">
          <IconApp name="check-circle" />
          <div>
            <strong>配置可用</strong>
            <span>保存后，方案里选择这个直连任务即可直连登录，无需 Python 与浏览器。</span>
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
  .wz-facts > div {
    grid-template-columns: 1fr;
  }
}
</style>
