<script setup lang="ts">
/**
 * 设置 · 会话保持面板体（schema 槽位）：Worker 常驻与空闲回收、浏览器数据持久化、立即关闭。
 */
import SettingsRow from "@/components/common/SettingsRow.vue";
import { computed, ref } from "vue";
import { useConfig } from "@/composables/useConfig";
import { workerApi } from "@/api";
import { frontendLogger } from "@/utils/logger";

const config = useConfig();

const stoppingBrowser = ref(false);

/** 手动停止 Python Worker 子进程（释放内存/解除浏览器占用）；失败仅记日志，不影响页面状态 */
async function stopBrowser() {
  stoppingBrowser.value = true;
  try { await workerApi.stop(); } catch (e) { frontendLogger.warn("browser", "停止 Worker 失败", e as Error); }
  stoppingBrowser.value = false;
}

/**
 * 保存浏览器数据的行短说明（收进 ? 气泡）：启用持久化后展示实际数据目录，
 * 未启用时保留原说明文字——目录随所选浏览器渠道变化，故为计算值。
 */
const persistentDescription = computed(() =>
  config.config.browser.persistent_context
    ? `数据目录：config/browser-data/${config.config.browser.browser_channel}/`
    : "将 Cookie 等浏览数据持久化到独立目录，重启后仍保持登录态",
);
</script>

<template>
  <div>
    <SettingsRow
      help="常驻进程可加快下次登录，但持续占用内存。关闭后空闲超时自动回收。"
      description="保持进程可加快下次登录，但持续占用内存；关闭后按空闲超时回收"
    >
      <template #label>登录完成后保持浏览器进程</template>
      <label class="toggle setting-row-switch">
        <input type="checkbox" v-model="config.config.worker.keep_alive" />
        <span class="toggle-slider"></span>
        <span class="sr-only">登录完成后保持浏览器进程</span>
      </label>
      <template #details>
        <SettingsRow
          help="Worker 空闲达到该时长后自动回收，释放内存。仅在未保持常驻时生效，默认 300 秒。"
          description="默认 300 秒，关闭进程保持后生效"
        >
          <template #label>
            <label for="settings-worker-idle-timeout">空闲超时（秒）</label>
          </template>
          <input
            id="settings-worker-idle-timeout"
            v-model.number="config.config.worker.idle_timeout_seconds"
            type="number"
            min="60"
            max="3600"
            :disabled="config.config.worker.keep_alive"
          />
        </SettingsRow>
      </template>
    </SettingsRow>
    <SettingsRow
      v-if="config.config.browser.browser_channel !== 'firefox'"
      help="将 Cookie 等浏览数据持久化到独立目录，重启后仍保持登录态。不同浏览器数据相互隔离，Firefox 不支持。"
      :description="persistentDescription"
    >
      <template #label>保存浏览器数据</template>
      <label class="toggle setting-row-switch">
        <input type="checkbox" v-model="config.config.browser.persistent_context" />
        <span class="toggle-slider"></span>
        <span class="sr-only">保存浏览器数据</span>
      </label>
    </SettingsRow>
    <SettingsRow label="关闭浏览器" description="会中断正在进行的登录，仅在调试或更换配置时使用">
      <button
        type="button"
        class="btn btn-sm btn-danger-ghost"
        :disabled="stoppingBrowser"
        @click="stopBrowser()"
      >
        {{ stoppingBrowser ? "正在关闭..." : "立即关闭浏览器" }}
      </button>
    </SettingsRow>
  </div>
</template>
