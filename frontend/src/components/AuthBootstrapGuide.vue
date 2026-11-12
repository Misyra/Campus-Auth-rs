<script setup lang="ts">
/**
 * 全屏鉴权引导页：应用完成首检（isLoading 翻转）仍无可用 token 时由 App.vue 挂载，
 * 覆盖在主界面之上接管交互（免鉴权 GET /api/auth/token 已移除后的新信任模型）。
 *
 * 两种模式按访问来源分流（isRemoteAccess）：
 * - 本地（回环）：指引从系统托盘「打开界面 / 复制界面链接」重开本页；另给一个
 *   粘贴框（接受完整带 #auth= 的 URL 或裸秘钥），让高级用户不重启托盘也能恢复本页。
 * - 远程（非回环，Docker/LAN）：托盘不可用，改用管理密码换取 token；
 *   服务端未配置管理密码时透传 AUTH_NOT_CONFIGURED 并给出配置指引。
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
</script>

<template>
  <div class="auth-bootstrap" role="dialog" aria-modal="true" aria-labelledby="auth-bootstrap-title">
    <div class="auth-bootstrap-card">
      <span class="logo-mark auth-bootstrap-logo" aria-hidden="true"></span>
      <h2 id="auth-bootstrap-title" class="auth-bootstrap-title">界面需要重新引导</h2>
      <p class="auth-bootstrap-desc">当前页面没有可用的访问凭据，无法连接本程序的后端服务。</p>

      <template v-if="!remote">
        <p class="auth-bootstrap-main">
          请从系统托盘菜单选择「打开界面」或「复制界面链接」后重新打开本页。
        </p>
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
          高级方式：粘贴托盘「复制界面链接」得到的完整地址，或只粘贴其中的引导秘钥，无需重启托盘即可恢复本页。
        </p>
      </template>

      <template v-else>
        <p class="auth-bootstrap-main">请输入管理密码以继续访问。</p>
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
        <p class="auth-bootstrap-hint">
          管理密码在「设置 · 系统与更新 · 访问与安全」中配置；本机经托盘打开界面时无需此密码。
        </p>
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

.auth-bootstrap-form .btn {
  white-space: nowrap;
}

.auth-bootstrap-error {
  margin: 0;
  color: var(--error);
  font-size: var(--text-sm);
}
</style>
