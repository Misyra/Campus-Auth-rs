<script setup lang="ts">
/**
 * 设置 · 高级设置面板体（schema 槽位）：纯净模式开关 + 关闭后展开的窗口尺寸/UA/启动参数。
 *
 * 纯净模式开关经 confirm 走 pureModeApi（不直接写草稿），展开区与开关是同一手写单元。
 */
import SettingsRow from "@/components/common/SettingsRow.vue";
import { onMounted } from "vue";
import { useConfig } from "@/composables/useConfig";
import { BROWSER_ARGS_DEFAULT } from "@/utils/constants";

const config = useConfig();

onMounted(() => { void config.fetchPureMode(); });

/** 加载推荐启动参数：将 BROWSER_ARGS_DEFAULT 逐行并入现有输入（去重，
 * 保留用户手写参数与注释行；后端默认为空不启用，点按钮才显式写入） */
function loadRecommendedArgs() {
  const current = config.config.browser.browser_args
    .split("\n")
    .map((s) => s.trim())
    .filter((s) => s.length > 0);
  const seen = new Set(current);
  for (const line of BROWSER_ARGS_DEFAULT.split("\n")) {
    const flag = line.trim();
    if (flag && !seen.has(flag)) {
      seen.add(flag);
      current.push(flag);
    }
  }
  config.config.browser.browser_args = current.join("\n");
}

const pureMode = config.pureMode;
async function togglePureMode() { await config.togglePureMode(); }
</script>

<template>
  <div>
    <SettingsRow
      help="启用后忽略下方自定义参数。登录异常时建议先恢复纯净模式排查。"
      description="开启后使用默认参数；关闭后可自定义窗口尺寸、用户代理和启动参数"
    >
      <template #label>纯净模式（推荐）</template>
      <label class="toggle setting-row-switch">
        <input type="checkbox" :checked="pureMode" @click.prevent="togglePureMode()" />
        <span class="toggle-slider"></span>
        <span class="sr-only">纯净模式（推荐）</span>
      </label>
      <template v-if="!pureMode" #details>
        <SettingsRow help="登录页面的渲染分辨率。默认 1280×720。">
          <template #label>
            <!-- 组标题：控件槽是宽/高两个各带 for 的输入框，标题不指向单一控件
                 （label-has-for 在此为行标题模式的误报） -->
            <!-- eslint-disable-next-line vuejs-accessibility/label-has-for -->
            <label>浏览器窗口尺寸</label>
          </template>
          <div class="viewport-input-row">
            <div class="viewport-input-group">
              <label for="settings-vp-w" class="viewport-label">宽</label>
              <input
                id="settings-vp-w"
                v-model.number="config.config.browser.viewport_width"
                type="number"
                min="320"
                max="3840"
                class="viewport-input"
              />
            </div>
            <span class="viewport-separator">×</span>
            <div class="viewport-input-group">
              <label for="settings-vp-h" class="viewport-label">高</label>
              <input
                id="settings-vp-h"
                v-model.number="config.config.browser.viewport_height"
                type="number"
                min="240"
                max="2160"
                class="viewport-input"
              />
            </div>
          </div>
        </SettingsRow>
        <SettingsRow help="伪装的浏览器标识。留空使用当前通道的默认值。">
          <template #label>
            <label for="settings-browser-ua">用户代理（UA）</label>
          </template>
          <input
            id="settings-browser-ua"
            v-model.trim="config.config.browser.user_agent"
            type="text"
            placeholder="留空使用当前浏览器的默认值"
          />
        </SettingsRow>
        <SettingsRow
          help="每行一个参数，附加到浏览器启动命令，# 开头为注释。--proxy-server、--load-extension、--remote-debugging-port 等安全敏感参数会被自动过滤；非 Chromium 引擎下 Chromium 专属参数不生效。"
        >
          <template #label>
            <label for="settings-browser-args">额外启动参数</label>
          </template>
          <template #details>
            <div class="setting-row-control form-group">
              <textarea
                id="settings-browser-args"
                v-model="config.config.browser.browser_args"
                rows="4"
                class="textarea--mono"
                placeholder="每行一个，例如：--disable-notifications"
              ></textarea>
              <button
                type="button"
                class="btn btn-sm btn-secondary"
                title="将推荐反检测参数并入上方输入框（去重保留手写内容）"
                @click="loadRecommendedArgs"
              >
                加载推荐参数
              </button>
            </div>
          </template>
        </SettingsRow>
      </template>
    </SettingsRow>
  </div>
</template>
