<script setup lang="ts">
import { ref, computed, onMounted } from "vue";
import { useConfig } from "@/composables/useConfig";
import FieldHelp from "@/components/common/FieldHelp.vue";
import { browsersApi, configApi, workerApi } from "@/api";

const config = useConfig();

const browsers = ref<{ channel: string; name: string; description: string; installed: boolean; icon: string }[]>([]);
const browserLoading = ref(true);
const playwrightDownloading = ref(false);
const stoppingBrowser = ref(false);

onMounted(async () => {
  try {
    const data = await browsersApi.fetch();
    browsers.value = data.browsers;
  } catch { /* */ }
  browserLoading.value = false;
  // 从共享状态加载纯净模式，确保与 TasksSettings 同步
  await config.fetchPureMode();
});

function handleBrowserClick(b: typeof browsers.value[0]) {
  if (!b.installed && b.channel !== "chromium" && b.channel !== "custom") return;
  if (b.channel === "chromium" && !b.installed) {
    void installPlaywright();
    return;
  }
  config.config.browser.browser_channel = b.channel;
}

async function installPlaywright() {
  playwrightDownloading.value = true;
  try { await browsersApi.installPlaywright(); } catch { /* */ }
  playwrightDownloading.value = false;
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
  } catch { /* */ }
  stoppingBrowser.value = false;
}
</script>

<template>
  <div class="settings-panel-grid">
    <!-- 浏览器选择 -->
    <section class="card settings-panel">
      <div class="settings-card-header">
        <svg class="settings-card-icon" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
          <circle cx="12" cy="12" r="10"/><circle cx="12" cy="12" r="4"/><line x1="21.17" y1="8" x2="12" y2="8"/>
        </svg>
        <h2>浏览器类型</h2>
      </div>
      <div class="card-body">
        <p class="form-help-text">选择用于自动登录的浏览器</p>
        <div class="browser-selection">
          <div v-if="browserLoading" class="loading">正在检测浏览器...</div>
          <div v-else class="browser-cards">
            <div v-for="b in browsers" :key="b.channel" class="browser-card"
              :class="{ active: config.config.browser.browser_channel === b.channel, disabled: !b.installed }"
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
                    <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" width="14" height="14"><polyline points="20 6 9 17 4 12"/></svg> 已安装
                  </span>
                  <span v-else-if="playwrightDownloading && b.channel === 'chromium'" class="status-downloading">
                    <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" width="14" height="14" class="spin"><path d="M21 12a9 9 0 11-6.219-8.56"/></svg> 下载中...
                  </span>
                  <span v-else class="status-not-installed">
                    <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" width="14" height="14"><path d="M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4"/><polyline points="7 10 12 15 17 10"/><line x1="12" y1="15" x2="12" y2="3"/></svg> 未安装
                  </span>
                </div>
              </div>
              <div v-if="config.config.browser.browser_channel === b.channel" class="browser-check">
                <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" width="20" height="20"><polyline points="20 6 9 17 4 12"/></svg>
              </div>
            </div>
          </div>
        </div>
      </div>
    </section>

    <!-- 基本设置 -->
    <section class="card settings-panel">
      <div class="settings-card-header">
        <svg class="settings-card-icon" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
          <line x1="4" y1="21" x2="4" y2="14"/><line x1="4" y1="10" x2="4" y2="3"/><line x1="12" y1="21" x2="12" y2="12"/><line x1="12" y1="8" x2="12" y2="3"/><line x1="20" y1="21" x2="20" y2="16"/><line x1="20" y1="12" x2="20" y2="3"/>
        </svg>
        <h2>基本设置</h2>
      </div>
      <div class="card-body">
        <div class="form-row">
          <div class="form-group">
            <label for="settings-browser-timeout">页面操作超时（秒）</label>
            <input id="settings-browser-timeout" v-model.number="config.config.browser.timeout" type="number" min="1" max="60" />
          </div>
          <div class="form-group">
            <label for="settings-browser-navigation-timeout">打开页面超时（秒）</label>
            <input id="settings-browser-navigation-timeout" v-model.number="config.config.browser.navigation_timeout" type="number" min="3" max="60" />
          </div>
        </div>
        <div class="toggle-group">
          <div class="toggle-with-help">
            <label class="toggle toggle-help-inline">
              <input type="checkbox" v-model="config.config.browser.headless" />
              <span class="toggle-slider"></span>
              <span class="toggle-label">无头浏览器模式</span>
            </label>
          </div>
        </div>
        <div class="toggle-group">
          <div class="toggle-with-help">
            <label class="toggle toggle-help-inline">
              <input type="checkbox" v-model="config.config.browser.low_resource_mode" />
              <span class="toggle-slider"></span>
              <span class="toggle-label">低资源模式（禁用图片）</span>
            </label>
          </div>
        </div>
        <div class="form-row">
          <div class="form-group">
            <label for="settings-browser-locale">浏览器语言（Locale）</label>
            <input id="settings-browser-locale" v-model.trim="config.config.browser.locale" type="text" placeholder="zh-CN" />
          </div>
          <div class="form-group">
            <label for="settings-browser-timezone">浏览器时区（Timezone）</label>
            <input id="settings-browser-timezone" v-model.trim="config.config.browser.timezone_id" type="text" placeholder="Asia/Shanghai" />
          </div>
        </div>
      </div>
    </section>

    <!-- 浏览器常驻 -->
    <section class="card settings-panel">
      <div class="settings-card-header">
        <svg class="settings-card-icon" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
          <rect x="2" y="3" width="20" height="14" rx="2" ry="2"/><line x1="8" y1="21" x2="16" y2="21"/><line x1="12" y1="17" x2="12" y2="21"/>
        </svg>
        <h2>浏览器常驻</h2>
      </div>
      <div class="card-body">
        <div class="toggle-group">
          <div class="toggle-with-help">
            <label class="toggle toggle-help-inline">
              <input type="checkbox" v-model="config.config.worker.keep_alive" />
              <span class="toggle-slider"></span>
              <span class="toggle-label">登录后保持浏览器</span>
            </label>
            <FieldHelp text="登录完成后不关闭浏览器进程，维持已认证的网页会话。关闭后浏览器将在空闲超时后自动回收。" />
          </div>
        </div>
        <div class="toggle-group" v-if="config.config.browser.browser_channel !== 'firefox'">
          <div class="toggle-with-help">
            <label class="toggle toggle-help-inline">
              <input type="checkbox" v-model="config.config.browser.persistent_context" />
              <span class="toggle-slider"></span>
              <span class="toggle-label">保留浏览器数据</span>
            </label>
            <FieldHelp text="使用独立的用户数据目录，保留 cookies 和登录状态。适用于需要存储登录状态的场景，不同浏览器的数据相互隔离。" />
          </div>
        </div>
        <div v-if="config.config.browser.persistent_context && config.config.browser.browser_channel !== 'firefox'" class="browser-info-tip">
          <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" width="16" height="16">
            <circle cx="12" cy="12" r="10"/><line x1="12" y1="16" x2="12" y2="12"/><line x1="12" y1="8" x2="12.01" y2="8"/>
          </svg>
          <div>数据目录: <code>config/browser-data/{{ config.config.browser.browser_channel }}/</code></div>
        </div>
        <div class="form-row" style="margin-top: 0.5rem;">
          <button type="button" class="btn btn-sm btn-secondary" :disabled="stoppingBrowser" @click="stopBrowser()">
            {{ stoppingBrowser ? "正在关闭..." : "立即关闭浏览器" }}
          </button>
        </div>
      </div>
    </section>

    <!-- 安全与反检测 -->
    <section class="card settings-panel">
      <div class="settings-card-header">
        <svg class="settings-card-icon" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
          <path d="M12 22s8-4 8-10V5l-8-3-8 3v7c0 6 8 10 8 10z"/>
        </svg>
        <h2>安全与反检测</h2>
      </div>
      <div class="card-body">
        <div class="toggle-group">
          <div class="toggle-with-help">
            <label class="toggle toggle-help-inline">
              <input type="checkbox" v-model="config.config.browser.disable_web_security" />
              <span class="toggle-slider"></span>
              <span class="toggle-label">禁用同源策略</span>
            </label>
          </div>
        </div>
        <div class="toggle-group">
          <div class="toggle-with-help">
            <label class="toggle toggle-help-inline">
              <input type="checkbox" v-model="config.config.browser.stealth_mode" />
              <span class="toggle-slider"></span>
              <span class="toggle-label">反检测模式</span>
            </label>
          </div>
        </div>
        <div v-show="config.config.browser.stealth_mode" class="form-group">
          <div class="stealth-script-actions">
            <button type="button" class="btn btn-sm btn-secondary" @click="loadDefaultStealthScript()">加载默认脚本</button>
          </div>
          <textarea v-model="config.config.browser.stealth_custom_script" rows="10" placeholder="留空使用内置默认脚本..." class="settings-monospace-textarea"></textarea>
        </div>
      </div>
    </section>

    <!-- 纯净模式与高级设置 -->
    <section class="card settings-panel">
      <div class="settings-card-header">
        <svg class="settings-card-icon" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
          <polygon points="12 2 15.09 8.26 22 9.27 17 14.14 18.18 21.02 12 17.77 5.82 21.02 7 14.14 2 9.27 8.91 8.26 12 2"/>
        </svg>
        <h2>纯净模式与高级设置</h2>
      </div>
      <div class="card-body">
        <div class="toggle-group">
          <div class="toggle-with-help">
            <label class="toggle toggle-help-inline">
              <input type="checkbox" :checked="pureMode" @click.prevent="togglePureMode()" />
              <span class="toggle-slider"></span>
              <span class="toggle-label">纯净模式</span>
            </label>
          </div>
        </div>
        <div v-if="pureMode" class="browser-safe-info"><p>纯净模式已开启，浏览器使用默认设置。</p></div>
        <div v-if="!pureMode" class="browser-safe-info browser-safe-info--warning"><p>当前为自定义模式。</p></div>
        <template v-if="!pureMode">
          <div class="form-group">
            <div class="field-label-row"><label>浏览器视口尺寸</label></div>
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
            <label for="settings-browser-ua">用户代理</label>
            <input id="settings-browser-ua" v-model.trim="config.config.browser.user_agent" type="text" placeholder="留空使用 Chromium 默认值" />
          </div>
        </template>
      </div>
    </section>
  </div>
</template>
