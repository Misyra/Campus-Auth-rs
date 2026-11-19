<script setup lang="ts">
/**
 * 全屏鉴权引导页：应用完成首检（isLoading 翻转）仍无可用 token 时由 App.vue 挂载，
 * 覆盖在主界面之上接管交互（免鉴权 GET /api/auth/token 已移除后的新信任模型）。
 *
 * 两种模式按访问来源分流（isRemoteAccess）：
 * - 本地（回环）：本机访问通常自动登录，引导页只在「后端未就绪 / local-token 不可用」
 *   时出现；给出三条获取凭据的途径——托盘「打开界面/复制界面链接」、终端启动输出里的
 *   「界面入口（含引导秘钥）」整行、数据目录 `config/.auth_bootstrap` 内容——并附粘贴框
 *   （接受完整带 #auth= 的 URL 或裸秘钥）与「重新检查」（后端刚启动时刷新即恢复）。
 * - 远程（非回环，Docker/LAN）：托盘不可用，改用管理密码换取 token；同时说明密码的
 *   三种来源（启动参数 / 环境变量 / 服务端设置页）；未配置时透传 AUTH_NOT_CONFIGURED
 *   并给出配置指引。
 *
 * 成功引导后整页刷新恢复应用：useUi.init() 有单次守卫（initialized 标志）不可重入，
 * 各 store 的订阅/轮询/WS 均在 init 时建立，就地恢复无法保证等价时序；而此时
 * fragment 已剥除、sessionStorage 已有 token，reload 后 ensureAuthToken 直接命中
 * 缓存，走的就是正常启动路径。
 */
import { ref } from "vue";
import {
  isRemoteAccess,
  parseBootstrapSecret,
  submitBootstrapSecret,
  submitManagementPassword,
  type BootstrapExchangeOutcome,
} from "../api/client";

const remote = isRemoteAccess();

/** 服务端未配置管理密码时的固定指引（契约文案，含环境变量注入路径） */
const NOT_CONFIGURED_HINT =
  "需在服务端配置 management_password 或环境变量 CAMPUS_AUTH_MANAGEMENT_PASSWORD 后重启";

// 粘贴框预填当前 URL 里可能残留的秘钥：fragment 交换失败（如秘钥过期）时用户
// 点一下「引导」即可重试，不必再回托盘复制一遍
const keyInput = ref(parseBootstrapSecret(location.hash) ?? "");
const passwordInput = ref("");
const showPassword = ref(false);
const submitting = ref(false);
const errorText = ref("");

/** 统一结算引导结果：成功刷新整页恢复应用，失败就地展示中文错误 */
async function settle(outcome: BootstrapExchangeOutcome): Promise<void> {
  if (outcome.ok) {
    window.location.reload();
    return;
  }
  errorText.value =
    outcome.code === "AUTH_NOT_CONFIGURED" ? NOT_CONFIGURED_HINT : outcome.message;
}

async function submitKey(): Promise<void> {
  if (submitting.value) return;
  errorText.value = "";
  const raw = keyInput.value.trim();
  if (!raw) {
    errorText.value = "请粘贴完整的界面链接或引导秘钥";
    return;
  }
  const secret = parseBootstrapSecret(raw);
  if (!secret) {
    errorText.value = "无法识别引导信息：请粘贴包含 #auth= 的完整链接，或直接粘贴引导秘钥";
    return;
  }
  submitting.value = true;
  try {
    await settle(await submitBootstrapSecret(secret));
  } finally {
    submitting.value = false;
  }
}

async function submitPassword(): Promise<void> {
  if (submitting.value) return;
  errorText.value = "";
  if (!passwordInput.value) {
    errorText.value = "请输入管理密码";
    return;
  }
  submitting.value = true;
  try {
    await settle(await submitManagementPassword(passwordInput.value));
  } finally {
    submitting.value = false;
  }
}

/** 后端可能刚启动（或刚重启）：重走一次正常启动路径，命中 local-token / 缓存即恢复 */
function retry(): void {
  window.location.reload();
}
</script>

<template>
  <div class="auth-bootstrap" role="dialog" aria-modal="true" aria-labelledby="auth-bootstrap-title">
    <div class="auth-bootstrap-card">
      <span class="logo-mark auth-bootstrap-logo" aria-hidden="true"></span>
      <h2 id="auth-bootstrap-title" class="auth-bootstrap-title">界面需要重新引导</h2>
      <p class="auth-bootstrap-desc">当前页面没有可用的访问凭据。</p>

      <template v-if="!remote">
        <p class="auth-bootstrap-main">
          本机访问通常会自动登录。看到本页说明后端尚未就绪：确认程序正在运行后点「重新检查」；
          仍无法进入时，按下面任一方式获取访问凭据。
        </p>
        <ul class="auth-bootstrap-steps">
          <li>系统托盘菜单 →「打开界面」；或「复制界面链接」后粘贴到下方输入框；</li>
          <li>从终端启动（含无托盘 / 无桌面环境）时，复制启动输出里「界面入口（含引导秘钥，勿转发）」的整行地址；</li>
          <li>打开程序数据目录下的 <code>config/.auth_bootstrap</code>，粘贴文件内容（64 位十六进制）。</li>
        </ul>
        <div class="auth-bootstrap-form">
          <input
            id="auth-bootstrap-key"
            v-model="keyInput"
            type="text"
            placeholder="粘贴界面链接或引导秘钥"
            aria-label="粘贴界面链接或引导秘钥"
            autocomplete="off"
            spellcheck="false"
            @keyup.enter="submitKey"
          />
          <button
            type="button"
            class="btn btn-primary"
            :disabled="submitting"
            @click="submitKey"
          >
            {{ submitting ? "验证中..." : "引导" }}
          </button>
        </div>
        <p class="auth-bootstrap-hint">
          粘贴完整界面链接、或只粘贴其中的引导秘钥均可，无需重启托盘。
        </p>
        <button type="button" class="btn btn-secondary btn-sm auth-bootstrap-retry" @click="retry">
          后端已启动，重新检查
        </button>
      </template>

      <template v-else>
        <p class="auth-bootstrap-main">远程访问需要使用管理密码换取访问凭据。</p>
        <ul class="auth-bootstrap-steps">
          <li>管理密码由服务端启动参数 <code>--management-password</code> 或环境变量 <code>CAMPUS_AUTH_MANAGEMENT_PASSWORD</code> 配置；</li>
          <li>也可在服务端「设置 · 系统与更新 · 访问与安全」中设置，修改后即时生效；</li>
          <li>本机（127.0.0.1 / localhost）打开界面无需密码；服务端未配置密码时会拒绝远程访问。</li>
        </ul>
        <div class="auth-bootstrap-form">
          <input
            id="auth-bootstrap-password"
            v-model="passwordInput"
            :type="showPassword ? 'text' : 'password'"
            placeholder="管理密码"
            aria-label="管理密码"
            autocomplete="current-password"
            @keyup.enter="submitPassword"
          />
          <button
            type="button"
            class="btn btn-ghost"
            :aria-pressed="showPassword"
            @click="showPassword = !showPassword"
          >
            {{ showPassword ? "隐藏" : "显示" }}
          </button>
          <button
            type="button"
            class="btn btn-primary"
            :disabled="submitting"
            @click="submitPassword"
          >
            {{ submitting ? "验证中..." : "解锁" }}
          </button>
        </div>
        <p class="auth-bootstrap-hint">不知道密码时请联系部署者。</p>
      </template>

      <p v-if="errorText" class="auth-bootstrap-error" role="alert">{{ errorText }}</p>
    </div>
  </div>
</template>

<style scoped>
/* 全屏引导遮罩：与 .exit-overlay / .init-overlay 同层的最顶层覆盖，
   颜色全部走主题 token，深浅主题自动适配（无 [data-theme] 分支，故用 scoped） */
.auth-bootstrap {
  position: fixed;
  inset: 0;
  z-index: var(--z-max);
  display: flex;
  align-items: center;
  justify-content: center;
  padding: var(--space-lg);
  background: var(--bg-primary);
  color: var(--text-primary);
}

.auth-bootstrap-card {
  display: flex;
  flex-direction: column;
  gap: var(--space-md);
  width: 100%;
  max-width: 480px;
}

.auth-bootstrap-logo {
  width: 48px;
  height: 48px;
}

.auth-bootstrap-title {
  margin: 0;
  font-size: var(--text-xl);
}

.auth-bootstrap-desc,
.auth-bootstrap-hint {
  margin: 0;
  color: var(--text-secondary);
  font-size: var(--text-base);
}

.auth-bootstrap-main {
  margin: 0;
  font-size: var(--text-md);
  font-weight: 500;
}

/* 获取凭据的三条途径：托盘链接 / 终端启动行 / 数据目录秘钥文件 */
.auth-bootstrap-steps {
  margin: 0;
  padding-left: 1.2em;
  display: flex;
  flex-direction: column;
  gap: 6px;
  color: var(--text-secondary);
  font-size: var(--text-sm);
  line-height: 1.6;
}

.auth-bootstrap-steps code {
  padding: 1px 5px;
  border-radius: var(--radius-xs);
  background: var(--bg-hover);
  font-family: var(--font-mono);
  font-size: var(--text-xs);
  word-break: break-all;
}

.auth-bootstrap-retry {
  align-self: flex-start;
}

.auth-bootstrap-hint {
  font-size: var(--text-sm);
}

.auth-bootstrap-form {
  display: flex;
  gap: var(--space-sm);
  align-items: stretch;
}

.auth-bootstrap-form input {
  flex: 1;
  min-width: 0;
  padding: 12px 14px;
  background: var(--bg-glass);
  border: 1px solid var(--border);
  border-radius: var(--radius-md);
  color: var(--text-primary);
  font-size: var(--text-base);
  color-scheme: inherit;
}

.auth-bootstrap-form input:focus-visible {
  outline: 2px solid var(--accent-text, var(--accent));
  outline-offset: 2px;
}

/* 共享 .btn 默认即 nowrap；此处只经分叉变量声明同一取值（纪律：scoped 不改写共享声明值） */
.auth-bootstrap-form .btn {
  --btn-white-space: nowrap;
}

.auth-bootstrap-error {
  margin: 0;
  color: var(--error);
  font-size: var(--text-sm);
}
</style>
