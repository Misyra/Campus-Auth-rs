<script setup lang="ts">
/** 设置 · 检测页：检测与重试、暂停时段及网络检测方式配置 */
import SettingsRow from "@/components/common/SettingsRow.vue";
import { computed } from "vue";
import { useConfig } from "@/composables/useConfig";
import FieldHelp from "@/components/common/FieldHelp.vue";

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
  <div class="settings-list-page monitor-settings-page">
    <section class="settings-panel">
      <div class="settings-card-header">
        <h2>检测频率</h2>
      </div>
      <div class="card-body">
        <SettingsRow description="较短间隔能更快发现断线，建议 60–300 秒，默认 300 秒">
          <template #label>
            <label for="settings-interval">检测间隔（秒）</label>
            <FieldHelp text="两次网络检测之间的间隔。过短增加资源消耗，过长延迟断线发现。建议 60~300 秒，默认 300 秒（后端钳制 20~1200 秒）。" />
          </template>
          <input
            id="settings-interval"
            v-model.number="config.config.monitor.check_interval_seconds"
            type="number"
            min="20"
            max="1200"
          />
        </SettingsRow>
        <SettingsRow description="弱网可适当调高，默认 2 秒">
          <template #label>
            <label for="settings-network-check-timeout">检测超时（秒）</label>
            <FieldHelp text="单次检测的等待上限。默认 2 秒，弱网环境可放宽至 5 秒。" />
          </template>
          <input
            id="settings-network-check-timeout"
            v-model.number="config.config.monitor.network_check_timeout"
            type="number"
            min="1"
            max="30"
          />
        </SettingsRow>
      </div>
    </section>
    <section class="settings-panel">
      <div class="settings-card-header">
        <h2>登录重试</h2>
      </div>
      <div class="card-body">
        <SettingsRow description="连续失败后结束本轮尝试，默认 3 次">
          <template #label>
            <label for="settings-max-retries">最大重试次数</label>
            <FieldHelp text="登录失败后的最大重试次数。默认 3 次。" />
          </template>
          <input
            id="settings-max-retries"
            v-model.number="config.config.retry.max_retries"
            type="number"
            min="1"
            max="5"
          />
        </SettingsRow>
        <SettingsRow description="失败后逐次延长等待时间，如 5 → 10 → 20 秒">
          <template #label>
            <label for="settings-retry-interval">重试间隔（秒）</label>
            <FieldHelp text="首次重试的等待间隔，之后每次重试翻倍（如 5 → 10 → 20 秒）。过短可能触发登录页限流。默认 5 秒。" />
          </template>
          <input
            id="settings-retry-interval"
            v-model.number="config.config.retry.retry_interval"
            type="number"
            min="1"
            max="300"
          />
        </SettingsRow>
        <SettingsRow description="等待认证生效，再复查网络连接">
          <template #label>
            <label for="settings-post-login-delay">登录后延迟（秒）</label>
            <FieldHelp text="登录完成后等待认证生效的时间，之后再复查网络。默认 5 秒。" />
          </template>
          <input
            id="settings-post-login-delay"
            v-model.number="config.config.monitor.post_login_delay"
            type="number"
            min="0"
            max="60"
          />
        </SettingsRow>
      </div>
    </section>
    <section class="settings-panel">
      <div class="settings-card-header">
        <h2>暂停时段</h2>
      </div>
      <div class="card-body">
        <SettingsRow description="每天在指定时段暂停检测和自动登录">
          <template #label>
            启用暂停时段
            <FieldHelp text="启用后在该时段内暂停检测与登录，适用于定时断网时段。" />
          </template>
          <label class="toggle setting-row-switch">
            <input type="checkbox" v-model="config.config.pause.enabled" />
            <span class="toggle-slider"></span>
            <span class="sr-only">启用暂停时段</span>
          </label>
          <template #details>
            <SettingsRow description="使用 24 小时制，范围 0–23">
              <template #label>
                <label for="settings-pause-start">开始时间（时）</label>
              </template>
              <input
                id="settings-pause-start"
                v-model.number="config.config.pause.start_hour"
                type="number"
                min="0"
                max="23"
                :disabled="!config.config.pause.enabled"
              />
            </SettingsRow>
            <SettingsRow description="支持跨天，如 22 点开始、次日 6 点结束">
              <template #label>
                <label for="settings-pause-end">结束时间（时）</label>
              </template>
              <input
                id="settings-pause-end"
                v-model.number="config.config.pause.end_hour"
                type="number"
                min="0"
                max="23"
                :disabled="!config.config.pause.enabled"
              />
            </SettingsRow>
          </template>
        </SettingsRow>
      </div>
    </section>
    <section class="settings-panel">
      <div class="settings-card-header">
        <h2>网络状态检测</h2>
      </div>
      <div class="card-body">
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
    </section>
    <section class="settings-panel">
      <div class="settings-card-header">
        <h2>诊断与恢复辅助</h2>
      </div>
      <div class="card-body">
        <SettingsRow description="仅在确认门户劫持后自动登录，减少误触发">
          <template #label>
            登录严格模式
            <span class="badge badge--sm badge--info">推荐开启</span>
            <FieldHelp
              text="默认开启。只有探测到明确的门户劫持证据才自动登录，证据不足时保持观察、不打扰。若遇到断网后未自动登录，请关闭本开关：关闭后只要网卡已连接、探测未确认在线就尝试登录，适用于「先登录学校门户、再进校园网认证选运营商」两级认证的网络——这类网关可能直接放行探测请求，严格模式下会误判为已在线而永不尝试。注意：关闭后若认证地址填错或门户无需登录，也会真的拉起浏览器，请配合暂停时段使用。"
            />
          </template>
          <label class="toggle setting-row-switch">
            <input type="checkbox" v-model="config.config.monitor.strict_login_mode" />
            <span class="toggle-slider"></span>
            <span class="sr-only">
              登录严格模式
              <span class="badge badge--sm badge--info">推荐开启</span>
            </span>
          </label>
        </SettingsRow>
        <SettingsRow description="直接检测真实网络，避免代理故障影响判断">
          <template #label>
            检测不走代理
            <FieldHelp text="启用后公网检测流量直连，避免系统代理故障造成误判；关闭则跟随系统代理。保存后下一轮检测生效。" />
          </template>
          <label class="toggle setting-row-switch">
            <input type="checkbox" v-model="config.config.monitor.disable_proxy" />
            <span class="toggle-slider"></span>
            <span class="sr-only">检测不走代理</span>
          </label>
        </SettingsRow>
      </div>
    </section>
  </div>
</template>
