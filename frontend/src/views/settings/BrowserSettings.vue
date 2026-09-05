<script setup lang="ts">
import IconApp from "@/components/common/IconApp.vue";
import { ref, computed, onMounted } from "vue";
import { useRouter } from "vue-router";
import { useConfig } from "@/composables/useConfig";
import { useEnvironment } from "@/composables/useEnvironment";
import FieldHelp from "@/components/common/FieldHelp.vue";
import { browsersApi, configApi, workerApi, extractApiError } from "@/api";
import { frontendLogger } from "@/utils/logger";

const config = useConfig();
const router = useRouter();
const { envStatus } = useEnvironment();
const browsers = ref<{ channel: string; name: string; description: string; installed: boolean; icon: string }[]>([]);
const browserLoading = ref(true);
const installingBrowser = ref<string | null>(null);
const browserInstallError = ref("");
const stoppingBrowser = ref(false);
const playwrightInstallable = new Set(["chromium", "firefox", "webkit"]);
const currentNotInstalled = computed(() => {
  const cur = config.config.browser.browser_channel;
  if (!cur || browsers.value.length === 0) return false;
  const found = browsers.value.find((b) => b.channel === cur);
  return found ? !found.installed : false;
});
const currentOfficialUrl = computed(() => OFFICIAL_URL[config.config.browser.browser_channel] ?? "");

onMounted(async () => {
  try {
    const data = await browsersApi.fetch();
    browsers.value = data.browsers;
  } catch (error) {
    frontendLogger.error("browser", "获取浏览器列表失败", error);
  }
  browserLoading.value = false;
  // 从共享状态加载纯净模式，确保与 TasksSettings 同步
  await config.fetchPureMode();
});

const OFFICIAL_URL: Record<string, string> = {
  msedge: "https://www.microsoft.com/edge/download",
  chrome: "https://www.google.com/chrome/",
};

function handleBrowserClick(b: typeof browsers.value[0]) {
  if (!b.installed) {
    if (playwrightInstallable.has(b.channel)) {
      void installPlaywright(b.channel);
      return;
    }
    // 系统浏览器未安装：给明确提示与官网链接
    const url = OFFICIAL_URL[b.channel];
    const name = b.name || b.channel;
    if (url) {
      // 用 confirm 风格的 toast 提示，用户可点链接跳转
      const msg = `${name} 未安装，请先从官网下载安装：${url}`;
      browserInstallError.value = msg;
      frontendLogger.warn("browser", msg);
      // 同时用 window.open 兜底时由用户主动点击错误区的链接，避免弹窗被拦截
    } else {
      browserInstallError.value = `${name} 未安装`;
    }
    return;
  }
  config.config.browser.browser_channel = b.channel;
}

async function installPlaywright(channel: string) {
  if (installingBrowser.value) return;
  installingBrowser.value = channel;
  browserInstallError.value = "";
  try {
    await browsersApi.installPlaywright(channel);
    const data = await browsersApi.fetch();
    browsers.value = data.browsers;
    if (browsers.value.find((b) => b.channel === channel)?.installed) {
      config.config.browser.browser_channel = channel;
      return;
    }
    browserInstallError.value = `${channel} 安装完成，但未检测到浏览器，请检查日志后重试`;
  } catch (error) {
    browserInstallError.value = `${channel} 安装失败：${extractApiError(error, "未知错误")}`;
  } finally {
    installingBrowser.value = null;
  }
}

// Stealth script
async function loadDefaultStealthScript() {
  try {
    const data = await configApi.fetchStealthScript();
    config.config.browser.stealth_custom_script = data.script;
  } catch { /* */ }
}

// 纯净模式 — 复用 useConfig 单一状态源，避免多页面间状态不同步
const pureMode = config.pureMode;

async function togglePureMode() {
  await config.togglePureMode();
}

async function stopBrowser() {
  stoppingBrowser.value = true;
  try {
    await workerApi.stop();
  } catch (error) {
    frontendLogger.warn("browser", "停止 Worker 失败", error);
  }
  stoppingBrowser.value = false;
}
</script>

<template>
  <div class="settings-panel-grid settings-panel-grid--cols2">
    <!-- 浏览器选择 -->
    <section class="card settings-panel settings-panel--wide">
      <div class="settings-card-header">
        <IconApp name="chrome" class="settings-card-icon" />
        <h2>浏览器选择</h2>
      </div>
      <div class="card-body">
        <div v-if="pythonNotReady" class="browser-safe-info browser-safe-info--warning browser-notice--top">
          <p>Python 环境未就绪，浏览器功能不可用。请先前往 <a class="inline-link" @click.prevent="router.push({ name: 'settings-system' })">设置 · 系统 → Python 环境</a> 初始化。</p>
        </div>
        <p class="form-help-text">选择用于自动登录的浏览器，推荐 Chromium / Edge / Chrome。</p>
        <div class="browser-selection">
          <div v-if="browserLoading" class="loading">正在检测浏览器...</div>
          <div v-else class="browser-cards">
            <div v-for="b in browsers" :key="b.channel" class="browser-card"
              :class="{ active: config.config.browser.browser_channel === b.channel, disabled: !b.installed && !playwrightInstallable.has(b.channel) }"
              @click="handleBrowserClick(b)">
              <div class="browser-icon">
                <img v-if="b.channel === 'chromium'" src="/icons/chromium.svg" width="32" height="32" alt="chromium" />
                <img v-else-if="b.channel === 'msedge'" src="/icons/edge.svg" width="32" height="32" alt="edge" />
                <img v-else-if="b.channel === 'chrome'" src="/icons/chrome.svg" width="32" height="32" alt="chrome" />
                <img v-else-if="b.channel === 'firefox'" src="/icons/firefox.svg" width="32" height="32" alt="firefox" />
                <img v-else-if="b.channel === 'webkit'" src="/icons/webkit.svg" width="32" height="32" alt="webkit" />
                <svg v-else viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" width="32" height="32">
                  <circle cx="12" cy="12" r="10"/><text x="12" y="16" text-anchor="middle" font-size="10" fill="currentColor">W</text>
                </svg>
              </div>
              <div class="browser-info">
                <div class="browser-name">{{ b.name }}</div>
                <div class="browser-status">
                  <span v-if="b.installed" class="status-installed">
                    <IconApp name="check" width="14" height="14" /> 已安装
                  </span>
                  <span v-else-if="installingBrowser === b.channel" class="status-downloading">
                    <IconApp name="refresh" width="14" height="14" class="spin" /> 下载中...
                  </span>
                  <span v-else-if="playwrightInstallable.has(b.channel)" class="status-not-installed">
                    <IconApp name="upload" width="14" height="14" /> 点击安装
                  </span>
                  <span v-else class="status-not-installed">
                    <IconApp name="upload" width="14" height="14" /> 未安装
                  </span>
                </div>
              </div>
              <div v-if="config.config.browser.browser_channel === b.channel" class="browser-check">
                <IconApp name="check" width="20" height="20" />
              </div>
            </div>
          </div>
        </div>
        <p v-if="browserInstallError" class="form-help-text">
          <template v-if="browserInstallError.includes('https://')">
            {{ browserInstallError.split('https://')[0] }}<a :href="'https://' + browserInstallError.split('https://')[1]" target="_blank" rel="noopener noreferrer" class="inline-link">{{ 'https://' + browserInstallError.split('https://')[1] }}</a>
          </template>
          <template v-else>{{ browserInstallError }}</template>
        </p>
        <div v-if="currentNotInstalled" class="browser-safe-info browser-safe-info--warning">
          <p>当前选择的浏览器未安装，启动将失败。请先安装 <strong>{{ config.config.browser.browser_channel }}</strong><template v-if="currentOfficialUrl">，或前往 <a :href="currentOfficialUrl" target="_blank" rel="noopener noreferrer" class="inline-link">官网下载</a></template>，或切换到已安装的浏览器后保存。</p>
        </div>
      </div>
    </section>
    <!-- 基本设置 -->
    <section class="card settings-panel">
      <div class="settings-card-header">
        <IconApp name="sliders" class="settings-card-icon" />
        <h2>基本设置</h2>
      </div>
      <div class="card-body">
        <div class="form-row">
          <div class="form-group">
            <div class="field-label-row">
              <label for="settings-browser-timeout">页面操作超时（秒）</label>
              <FieldHelp text="单次页面操作的等待上限。默认 30 秒。" />
            </div>
            <input id="settings-browser-timeout" v-model.number="config.config.browser.timeout" type="number" min="1" max="120" />
          </div>
          <div class="form-group">
            <div class="field-label-row">
              <label for="settings-browser-navigation-timeout">页面打开超时（秒）</label>
              <FieldHelp text="打开认证页面的最长等待时间。默认 15 秒。" />
            </div>
            <input id="settings-browser-navigation-timeout" v-model.number="config.config.browser.navigation_timeout" type="number" min="3" max="120" />
          </div>
        </div>
        <div class="form-row">
          <div class="form-group">
            <div class="field-label-row">
              <label for="settings-login-timeout">登录等待上限（秒）</label>
              <FieldHelp text="手动登录的最长等待时间。默认 120 秒。" />
            </div>
            <input id="settings-login-timeout" v-model.number="config.config.browser.login_timeout" type="number" min="10" max="600" />
          </div>
        </div>
        <div class="toggle-group">
          <div class="toggle-with-help">
            <label class="toggle toggle-help-inline">
              <input type="checkbox" v-model="config.config.browser.headless" />
              <span class="toggle-slider"></span>
              <span class="toggle-label">后台运行</span>
            </label>
            <FieldHelp text="不显示浏览器窗口。首次配置任务或排查问题时可关闭。" />
          </div>
        </div>
        <div class="toggle-group">
          <div class="toggle-with-help">
            <label class="toggle toggle-help-inline">
              <input type="checkbox" v-model="config.config.browser.low_resource_mode" />
              <span class="toggle-slider"></span>
              <span class="toggle-label">低资源模式</span>
            </label>
            <FieldHelp text="不加载图片，降低资源占用。登录页使用图片验证码时请关闭。" />
          </div>
        </div>
        <div class="form-row">
          <div class="form-group">
            <div class="field-label-row">
              <label for="settings-browser-locale">浏览器语言</label>
              <FieldHelp text="发送给网站的语言偏好。默认 zh-CN。" />
            </div>
            <input id="settings-browser-locale" v-model.trim="config.config.browser.locale" type="text" placeholder="zh-CN" />
          </div>
          <div class="form-group">
            <div class="field-label-row">
              <label for="settings-browser-timezone">浏览器时区</label>
              <FieldHelp text="标准时区名称，留空跟随系统。默认 Asia/Shanghai。" />
            </div>
            <input id="settings-browser-timezone" v-model.trim="config.config.browser.timezone_id" type="text" placeholder="Asia/Shanghai" />
          </div>
        </div>
      </div>
    </section>

    <!-- 会话保持 -->
    <section class="card settings-panel">
      <div class="settings-card-header">
        <IconApp name="monitor" class="settings-card-icon" />
        <h2>会话保持</h2>
      </div>
      <div class="card-body">
        <div class="toggle-group">
          <div class="toggle-with-help">
            <label class="toggle toggle-help-inline">
              <input type="checkbox" v-model="config.config.worker.keep_alive" />
              <span class="toggle-slider"></span>
              <span class="toggle-label">登录完成后保持浏览器进程</span>
            </label>
            <FieldHelp text="常驻进程可加快下次登录，但持续占用内存。关闭后空闲超时自动回收。" />
          </div>
        </div>
        <div class="toggle-group" v-if="config.config.browser.browser_channel !== 'firefox'">
          <div class="toggle-with-help">
            <label class="toggle toggle-help-inline">
              <input type="checkbox" v-model="config.config.browser.persistent_context" />
              <span class="toggle-slider"></span>
              <span class="toggle-label">记住登录状态</span>
            </label>
            <FieldHelp text="将 Cookie 持久化到独立目录。不同浏览器数据相互隔离，Firefox 不支持。" />
          </div>
        </div>
        <div v-if="config.config.browser.persistent_context && config.config.browser.browser_channel !== 'firefox'" class="browser-info-tip">
          <IconApp name="info" width="16" height="16" />
          <div>数据目录: <code>config/browser-data/{{ config.config.browser.browser_channel }}/</code></div>
        </div>
        <div class="browser-close-row">
          <button type="button" class="btn btn-sm btn-danger-ghost" :disabled="stoppingBrowser" @click="stopBrowser()">
            {{ stoppingBrowser ? "正在关闭..." : "立即关闭浏览器" }}
          </button>
          <span class="hint">仅在调试或更换配置时使用，会中断正在进行的登录</span>
        </div>
      </div>
    </section>
    <!-- 安全与反检测 -->
    <section class="card settings-panel">
      <div class="settings-card-header">
        <IconApp name="shield" class="settings-card-icon" />
        <h2>安全与反检测</h2>
      </div>
      <div class="card-body">
        <div class="toggle-group">
          <div class="toggle-with-help">
            <label class="toggle toggle-help-inline">
              <input type="checkbox" v-model="config.config.browser.disable_web_security" />
              <span class="toggle-slider"></span>
              <span class="toggle-label">禁用网页安全限制</span>
            </label>
            <FieldHelp text="仅在登录页出现跨域错误时启用，日常保持关闭。" />
          </div>
        </div>
        <div class="toggle-group">
          <div class="toggle-with-help">
            <label class="toggle toggle-help-inline">
              <input type="checkbox" v-model="config.config.browser.stealth_mode" />
              <span class="toggle-slider"></span>
              <span class="toggle-label">反自动化检测</span>
            </label>
            <FieldHelp text="隐藏自动化特征。仅在被登录页拦截时启用。" />
          </div>
        </div>
        <div v-show="config.config.browser.stealth_mode" class="form-group">
          <div class="stealth-script-actions">
            <span class="hint">自定义脚本，留空使用内置脚本</span>
            <button type="button" class="btn btn-sm btn-secondary" @click="loadDefaultStealthScript()">填入内置脚本</button>
          </div>
          <textarea v-model="config.config.browser.stealth_custom_script" rows="6" placeholder="留空使用内置默认脚本..." class="settings-monospace-textarea"></textarea>
        </div>
      </div>
    </section>

    <!-- 高级设置 -->
    <section class="card settings-panel">
      <div class="settings-card-header">
        <IconApp name="star" class="settings-card-icon" />
        <h2>高级设置</h2>
      </div>
      <div class="card-body">
        <div class="toggle-group">
          <div class="toggle-with-help">
            <label class="toggle toggle-help-inline">
              <input type="checkbox" :checked="pureMode" @click.prevent="togglePureMode()" />
              <span class="toggle-slider"></span>
              <span class="toggle-label">纯净模式（推荐）</span>
            </label>
            <FieldHelp text="启用后忽略下方自定义参数。登录异常时建议先恢复纯净模式排查。" />
          </div>
        </div>
        <template v-if="!pureMode">
          <div class="form-group">
            <div class="field-label-row">
              <label>浏览器窗口尺寸</label>
              <FieldHelp text="登录页面的渲染分辨率。默认 1280×720。" />
            </div>
            <div class="viewport-input-row">
              <div class="viewport-input-group">
                <label for="settings-vp-w" class="viewport-label">宽</label>
                <input id="settings-vp-w" v-model.number="config.config.browser.viewport_width" type="number" min="320" max="3840" class="viewport-input" />
              </div>
              <span class="viewport-separator">×</span>
              <div class="viewport-input-group">
                <label for="settings-vp-h" class="viewport-label">高</label>
                <input id="settings-vp-h" v-model.number="config.config.browser.viewport_height" type="number" min="240" max="2160" class="viewport-input" />
              </div>
            </div>
          </div>
          <div class="form-group">
            <div class="field-label-row">
              <label for="settings-browser-ua">用户代理（UA）</label>
              <FieldHelp text="伪装的浏览器标识。留空使用当前通道的默认值。" />
            </div>
            <input id="settings-browser-ua" v-model.trim="config.config.browser.user_agent" type="text" placeholder="留空使用当前浏览器的默认值" />
          </div>
        </template>
      </div>
    </section>
  </div>
</template>
