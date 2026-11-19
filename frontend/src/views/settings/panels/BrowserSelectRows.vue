<script setup lang="ts">
/**
 * 设置 · 浏览器选择面板体（schema 槽位）：浏览器卡片选择 / Playwright 安装 / 未安装告警。
 *
 * 交互全部围绕浏览器卡片（role=group + aria-pressed），非标准设置行形态，保留手写槽位。
 */
import SettingsRow from "@/components/common/SettingsRow.vue";
import BrowserIcon from "@/components/common/BrowserIcon.vue";
import IconApp from "@/components/common/IconApp.vue";
import { ref, computed, onMounted } from "vue";
import { useRouter } from "vue-router";
import { useConfig } from "@/composables/useConfig";
import { useEnvironment } from "@/composables/useEnvironment";
import { browsersApi } from "@/api";
import { extractApiError } from "@/api/client";
import type { BrowserInfo } from "@/api/types";
import { frontendLogger } from "@/utils/logger";

const config = useConfig();
const router = useRouter();
const { envStatus, refreshEnv } = useEnvironment();
const browsers = ref<BrowserInfo[]>([]);
const browserLoading = ref(true);
/** 列表加载失败（FE2-5）：展示错误文案与重试入口，避免渲染空白网格无声失败 */
const browserError = ref("");
const installingBrowser = ref<string | null>(null);
const browserInstallError = ref("");
const playwrightInstallable = new Set(["chromium", "firefox", "webkit"]);

/** 各浏览器渠道的官方下载地址（Playwright 不可安装时的兜底指引） */
const OFFICIAL_URL: Record<string, string> = {
  msedge: "https://www.microsoft.com/edge/download",
  chrome: "https://www.google.com/chrome/",
};

const firstErrorUrl = computed(() => {
  const text = browserInstallError.value;
  const idx = text.indexOf("https://");
  if (idx === -1) return null;
  const match = /^https:\/\/[^\s，。；）]+/i.exec(text.slice(idx));
  if (!match) return null;
  return { index: idx, url: match[0] };
});

const currentNotInstalled = computed(() => {
  const cur = config.config.browser.browser_channel;
  if (!cur || browsers.value.length === 0) return false;
  const found = browsers.value.find((b) => b.channel === cur);
  return found ? !found.installed : false;
});
const currentOfficialUrl = computed(() => OFFICIAL_URL[config.config.browser.browser_channel] ?? "");
const pythonNotReady = computed(() => envStatus.value != null && !envStatus.value.capability_ready);
const installedChromium = computed(() => browsers.value.find((b) => b.channel === "chromium" && b.installed));

/** 拉取浏览器列表（FE2-5）：失败置 browserError 供模板展示并支持重试 */
async function fetchBrowsers(): Promise<void> {
  browserLoading.value = true;
  browserError.value = "";
  try {
    const data = await browsersApi.fetch();
    browsers.value = data.browsers;
  } catch (error) {
    frontendLogger.error("browser", "获取浏览器列表失败", error);
    browserError.value = extractApiError(error, "浏览器列表检测失败");
  } finally {
    browserLoading.value = false;
  }
}

onMounted(async () => {
  await Promise.allSettled([fetchBrowsers(), refreshEnv()]);
});

/** 浏览器卡片点击分派：已安装→选为当前浏览器；未安装→Playwright 可装的走安装，否则提示去官网下载 */
function handleBrowserClick(b: typeof browsers.value[0]) {
  if (!b.installed) {
    if (playwrightInstallable.has(b.channel)) {
      if (pythonNotReady.value) return;
      void installPlaywright(b.channel);
      return;
    }
    const url = OFFICIAL_URL[b.channel];
    const name = b.name || b.channel;
    if (url) {
      const msg = `${name} 未安装，请先从官网下载安装：${url}`;
      browserInstallError.value = msg;
      frontendLogger.warn("browser", msg);
    } else {
      browserInstallError.value = `${name} 未安装`;
    }
    return;
  }
  config.config.browser.browser_channel = b.channel;
}

/** 用户主动确认后切换到已安装的 Chromium；只修改待保存配置，不静默落盘。 */
function switchToInstalledChromium(): void {
  if (!installedChromium.value) return;
  config.config.browser.browser_channel = installedChromium.value.channel;
  browserInstallError.value = "";
}

/** 经 Playwright 安装指定 channel：装完重查列表；装完却未检测到（后端探测失败）与
 *  安装请求失败分两路提示，前者建议查日志重试，后者直接展示后端错误消息 */
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
</script>

<template>
  <div>
    <div v-if="pythonNotReady" class="browser-safe-info browser-safe-info--warning browser-notice--top">
      <p>
        运行环境未就绪，浏览器功能不可用。请先前往
        <a class="inline-link" @click.prevent="router.push({ name: 'settings-tasks' })">设置 · 任务与环境 → 运行环境</a>
        初始化。
      </p>
    </div>
    <SettingsRow label="登录浏览器" description="选择用于自动登录的浏览器，推荐 Chromium / Edge / Chrome">
      <span v-if="browserLoading" class="hint">正在检测浏览器…</span>
      <div v-else-if="browserError" class="hint">
        {{ browserError }}
        <button type="button" class="btn btn-sm btn-link" @click="fetchBrowsers()">重试</button>
      </div>
      <div v-else-if="browsers.some(b => b.installed)" class="browser-choices" role="group" aria-label="登录浏览器">
        <button
          v-for="b in browsers.filter(b => b.installed)"
          :key="b.channel"
          type="button"
          class="browser-choice"
          :aria-pressed="config.config.browser.browser_channel === b.channel"
          :disabled="installingBrowser !== null"
          @click="handleBrowserClick(b)"
        >
          <BrowserIcon :channel="b.channel" :size="30" aria-hidden="true" />
          <span>{{ b.name }}</span>
          <IconApp
            v-if="config.config.browser.browser_channel === b.channel"
            name="check"
            class="browser-choice-check"
            aria-hidden="true"
          />
        </button>
      </div>
      <span v-else class="hint">未检测到已安装的浏览器，请先安装</span>
    </SettingsRow>
    <SettingsRow
      v-if="!browserLoading && !browserError && browsers.some(b => !b.installed)"
      label="安装浏览器"
      description="Playwright 浏览器可直接安装；Edge 与 Chrome 请从官网下载"
    >
      <button
        v-for="b in browsers.filter(b => !b.installed)"
        :key="b.channel"
        type="button"
        class="btn btn-secondary btn-sm"
        :disabled="installingBrowser !== null || (pythonNotReady && playwrightInstallable.has(b.channel))"
        @click="handleBrowserClick(b)"
      >
        <BrowserIcon :channel="b.channel" :size="20" aria-hidden="true" />
        {{ installingBrowser === b.channel ? '安装中…' : b.name }}
      </button>
    </SettingsRow>
    <p v-if="browserInstallError" class="form-help-text">
      <template v-if="firstErrorUrl">
        {{ browserInstallError.slice(0, firstErrorUrl.index) }}
        <a :href="firstErrorUrl.url" target="_blank" rel="noopener noreferrer" class="inline-link">
          {{ firstErrorUrl.url }}
        </a>
        {{ browserInstallError.slice(firstErrorUrl.index + firstErrorUrl.url.length) }}
      </template>
      <template v-else>{{ browserInstallError }}</template>
    </p>
    <div v-if="currentNotInstalled" class="browser-safe-info browser-safe-info--warning">
      <div class="browser-warning-copy">
        <p>
          当前选择的浏览器未安装，启动将失败。请先安装
          <strong>{{ config.config.browser.browser_channel }}</strong>
          <template v-if="currentOfficialUrl">
            ，或前往
            <a :href="currentOfficialUrl" target="_blank" rel="noopener noreferrer" class="inline-link">官网下载</a>
          </template>
          ，或切换到已安装的浏览器后保存。
        </p>
        <button
          v-if="installedChromium"
          type="button"
          class="btn btn-sm btn-secondary"
          @click="switchToInstalledChromium"
        >
          切换到 Chromium
        </button>
      </div>
    </div>
  </div>
</template>
