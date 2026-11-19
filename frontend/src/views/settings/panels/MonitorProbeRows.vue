<script setup lang="ts">
/**
 * 设置 · 网络状态检测面板体（schema 槽位）：204 门户 / TCP / URL 内容三类探测行。
 *
 * 三行都带「开关 + 启用后才展开的目标输入」结构，且行内含推荐等级徽标与
 * aria-describedby 关联说明，超出 schema 标准行的表达范围，保留手写槽位。
 */
import SettingsRow from "@/components/common/SettingsRow.vue";
import FieldHelp from "@/components/common/FieldHelp.vue";
import { computed } from "vue";
import { useConfig } from "@/composables/useConfig";

const config = useConfig();

// 204 检测目标文本：展示时一行一个，输入时兼容历史的英文逗号分隔。
const httpCheckText = computed({
  get: () => config.config.monitor.test_urls.join("\n"),
  set: (value: string) => {
    config.config.monitor.test_urls = value
      .split(/[\n,]+/)
      .map((item) => item.trim())
      .filter(Boolean);
  },
});

const urlCheckEnabled = computed({
  get: () => config.config.monitor.enable_url_check,
  set: (v: boolean) => {
    if (v && config.config.monitor.url_check_urls.length === 0) {
      // 首次开启时填入推荐目标；关闭只停用探测，不清空用户配置。
      config.config.monitor.url_check_urls = [...config.defaultUrlCheckUrls];
    }
    config.config.monitor.enable_url_check = v;
  },
});

// URL 检测目标文本（每行一个，格式：url|期望响应）
const urlCheckText = computed({
  get: () => config.config.monitor.url_check_urls.join('\n'),
  set: (v: string) => {
    config.config.monitor.url_check_urls = v
      .split('\n')
      .map((s) => s.trim())
      .filter(Boolean);
  },
});
</script>

<template>
  <div>
    <SettingsRow>
      <template #label>
        204 门户检测
        <span class="badge badge--sm badge--info">推荐开启</span>
        <FieldHelp text="默认开启。请求 generate_204 端点：204 表示公网在线，200 或跳转表示被认证门户劫持。这是自动恢复的主要证据。必须填写返回 204 的轻量端点，普通网页不能填在这里；每行一个地址，也兼容英文逗号分隔。" />
      </template>
      <template #description>
        <p id="settings-http-description">通过轻量请求识别公网连接和认证门户；仅填写返回 HTTP 204 的地址，每行一个</p>
      </template>
      <label class="toggle setting-row-switch">
        <input type="checkbox" v-model="config.config.monitor.enable_http_check" />
        <span class="toggle-slider"></span>
        <span class="sr-only">
          204 门户检测
          <span class="badge badge--sm badge--info">推荐开启</span>
        </span>
      </label>
      <template v-if="config.config.monitor.enable_http_check" #details>
        <div class="setting-row-control form-group">
          <textarea
            id="settings-http-targets"
            aria-label="204 门户检测目标"
            aria-describedby="settings-http-description"
            v-model="httpCheckText"
            rows="3"
            class="textarea--mono"
            placeholder="http://connect.rom.miui.com/generate_204&#10;http://www.gstatic.com/generate_204"
          ></textarea>
        </div>
      </template>
    </SettingsRow>
    <SettingsRow>
      <template #label>
        TCP 检测
        <span class="badge badge--sm">补充</span>
        <FieldHelp text="仅补充传输层证据。TCP 成功不代表网页可访问，因此它单独成功时不会判定公网在线，也不会直接触发登录。多个目标以英文逗号分隔；省略端口时默认为 53。" />
      </template>
      <template #description>
        <p id="settings-tcp-description">补充检查传输层连通性，不能单独判断公网在线；目标用英文逗号分隔，省略端口时使用 53</p>
      </template>
      <label class="toggle setting-row-switch">
        <input type="checkbox" v-model="config.config.monitor.enable_tcp_check" />
        <span class="toggle-slider"></span>
        <span class="sr-only">
          TCP 检测
          <span class="badge badge--sm">补充</span>
        </span>
      </label>
      <template v-if="config.config.monitor.enable_tcp_check" #details>
        <div class="setting-row-control form-group">
          <input
            id="settings-network-targets"
            aria-label="TCP 检测目标"
            aria-describedby="settings-tcp-description"
            :value="config.config.monitor.ping_targets.join(',')"
            @input="config.config.monitor.ping_targets = ($event.target as HTMLInputElement).value.split(',').map(s => s.trim()).filter(Boolean)"
            type="text"
            placeholder="8.8.8.8:53,114.114.114.114:53"
          />
        </div>
      </template>
    </SettingsRow>
    <SettingsRow>
      <template #label>
        URL 内容检测
        <span class="badge badge--sm">补充</span>
        <FieldHelp text="用页面关键字补充确认公网或门户劫持。适合 204 端点在本网络不稳定时启用；每行一条：地址|关键字。" />
      </template>
      <template #description>
        <p id="settings-url-description">通过页面关键字辅助判断网络状态；每行一条，格式为“地址|关键字”</p>
      </template>
      <label class="toggle setting-row-switch">
        <input type="checkbox" v-model="urlCheckEnabled" />
        <span class="toggle-slider"></span>
        <span class="sr-only">
          URL 内容检测
          <span class="badge badge--sm">补充</span>
        </span>
      </label>
      <template v-if="urlCheckEnabled" #details>
        <div class="setting-row-control form-group">
          <textarea
            id="settings-url-check"
            aria-label="URL 检测目标"
            aria-describedby="settings-url-description"
            v-model="urlCheckText"
            rows="4"
            class="textarea--mono"
            placeholder="https://captive.apple.com|Success&#10;https://detectportal.firefox.com|success&#10;https://msftconnecttest.com|Microsoft Connect Test"
          ></textarea>
        </div>
      </template>
    </SettingsRow>
  </div>
</template>
