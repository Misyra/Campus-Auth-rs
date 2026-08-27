<script setup lang="ts">
import IconApp from "@/components/common/IconApp.vue";
import { computed } from "vue";
import { useConfig } from "@/composables/useConfig";
import FieldHelp from "@/components/common/FieldHelp.vue";

const config = useConfig();

const urlCheckEnabled = computed({
  get: () => config.config.monitor.url_check_urls.length > 0,
  set: (v: boolean) => {
    if (v) {
      // 开启时设置默认检测目标
      config.config.monitor.url_check_urls = [...config.defaultUrlCheckUrls];
    } else {
      config.config.monitor.url_check_urls = [];
    }
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
  <div class="settings-panel-grid">
    <!-- 检测与重试 -->
    <section class="card settings-panel">
      <div class="settings-card-header">
        <IconApp name="eye" class="settings-card-icon" />
        <h2>检测与重试</h2>
      </div>
      <div class="card-body">
        <div class="form-row">
          <div class="form-group">
            <div class="field-label-row">
              <label for="settings-interval">检测间隔（秒）</label>
              <FieldHelp text="每隔多久检测一次网络连通性。建议 180~600 秒。" />
            </div>
            <input id="settings-interval" v-model.number="config.config.monitor.check_interval_seconds" type="number" min="10" max="86400" />
          </div>
          <div class="form-group">
            <div class="field-label-row">
              <label for="settings-network-check-timeout">检测超时（秒）</label>
              <FieldHelp text="网络检测的超时时间。默认 2 秒，范围 1~30 秒。" />
            </div>
            <input id="settings-network-check-timeout" v-model.number="config.config.monitor.network_check_timeout" type="number" min="1" max="30" />
          </div>
        </div>
        <div class="form-row settings-toggle-spacer">
          <div class="form-group">
            <div class="field-label-row">
              <label for="settings-post-login-delay">登录后延迟（秒）</label>
              <FieldHelp text="登录步骤完成后等待认证生效的延迟，再进行网络检测确认。默认 5 秒，范围 0~60 秒。" />
            </div>
            <input id="settings-post-login-delay" v-model.number="config.config.monitor.post_login_delay" type="number" min="0" max="60" />
          </div>
          <div class="form-group">
            <div class="field-label-row">
              <label for="settings-login-timeout">登录请求超时（秒）</label>
              <FieldHelp text="点击「手动登录」后前端等待的最长时间。" />
            </div>
            <input id="settings-login-timeout" v-model.number="config.config.browser.login_timeout" type="number" min="10" max="600" />
          </div>
        </div>
        <div class="form-row">
          <div class="form-group">
            <div class="field-label-row">
              <label for="settings-max-retries">最大登录重试次数</label>
              <FieldHelp text="登录失败后最多重试几次。" />
            </div>
            <input id="settings-max-retries" v-model.number="config.config.retry.max_retries" type="number" min="1" max="5" />
          </div>
          <div class="form-group">
            <div class="field-label-row">
              <label for="settings-retry-interval">重试间隔（秒）</label>
              <FieldHelp text="每次重试之间的固定间隔秒数。" />
            </div>
            <input id="settings-retry-interval" v-model.number="config.config.retry.retry_interval" type="number" min="1" max="300" />
          </div>
        </div>
      </div>
    </section>

    <!-- 网络检测方式 -->
    <section class="card settings-panel settings-panel--wide">
      <div class="settings-card-header">
        <IconApp name="wifi" class="settings-card-icon" />
        <h2>网络检测方式</h2>
      </div>
      <div class="card-body">
        <div class="settings-grid-2col">
          <div class="settings-detect-col">
            <h4 class="settings-detect-heading">网络状态检测</h4>
            <div class="toggle-group">
              <div class="toggle-with-help">
                <label class="toggle toggle-help-inline">
                  <input type="checkbox" v-model="config.config.monitor.enable_tcp_check" />
                  <span class="toggle-slider"></span>
                  <span class="toggle-label">TCP 检测</span>
                  <FieldHelp text="通过 TCP 连接外部站点检测网络连通性。" />
                </label>
              </div>
            </div>
            <div v-if="config.config.monitor.enable_tcp_check" class="form-group settings-toggle-compact">
              <div class="field-label-row">
                <label for="settings-network-targets">TCP 检测目标</label>
                <FieldHelp text="支持 host 或 host:port 格式，逗号分隔。" />
              </div>
              <input id="settings-network-targets"
                :value="config.config.monitor.ping_targets.join(',')"
                @input="config.config.monitor.ping_targets = ($event.target as HTMLInputElement).value.split(',').map(s => s.trim()).filter(Boolean)"
                type="text" placeholder="8.8.8.8:53,114.114.114.114:53" />
            </div>
            <div class="toggle-group settings-toggle-spacer">
              <div class="toggle-with-help">
                <label class="toggle toggle-help-inline">
                  <input type="checkbox" v-model="config.config.monitor.enable_http_check" />
                  <span class="toggle-slider"></span>
                  <span class="toggle-label">HTTP 检测</span>
                  <FieldHelp text="通过 HTTP 请求外部站点检测网络连通性。" />
                </label>
              </div>
            </div>
            <div v-if="config.config.monitor.enable_http_check" class="form-group settings-toggle-compact">
              <div class="field-label-row">
                <label for="settings-http-targets">HTTP 检测目标</label>
              </div>
              <input id="settings-http-targets"
                :value="config.config.monitor.test_urls.join(',')"
                @input="config.config.monitor.test_urls = ($event.target as HTMLInputElement).value.split(',').map(s => s.trim()).filter(Boolean)"
                type="text" placeholder="https://www.baidu.com,https://www.qq.com" />
            </div>
            <div class="toggle-group settings-toggle-spacer">
              <div class="toggle-with-help">
                <label class="toggle toggle-help-inline">
                  <input type="checkbox" v-model="urlCheckEnabled" />
                  <span class="toggle-slider"></span>
                  <span class="toggle-label">URL 标题检测</span>
                  <FieldHelp text="通过请求 URL 并检查页面标题/内容判断是否被劫持。每行一个，格式：url|期望响应。" />
                </label>
              </div>
            </div>
            <div v-if="urlCheckEnabled" class="form-group settings-toggle-compact">
              <div class="field-label-row">
                <label for="settings-url-check">URL 检测目标（每行一个，格式：url|期望响应）</label>
              </div>
              <textarea id="settings-url-check" v-model="urlCheckText" rows="4"
                placeholder="https://captive.apple.com|Success&#10;https://detectportal.firefox.com|success&#10;https://msftconnecttest.com|Microsoft Connect Test"></textarea>
            </div>
            <div class="toggle-group settings-toggle-spacer">
              <div class="toggle-with-help">
                <label class="toggle toggle-help-inline">
                  <input type="checkbox" v-model="config.config.monitor.enable_local_check" />
                  <span class="toggle-slider"></span>
                  <span class="toggle-label">物理网络连接检查</span>
                  <FieldHelp text="启用后检测物理网卡是否存在在线连接；若网卡全失联则直接判定为离线，跳过后续探测。" />
                </label>
              </div>
            </div>
          </div>
          <div class="settings-detect-col">
            <h4 class="settings-detect-heading">登录前检测</h4>
            <div class="toggle-group">
              <div class="toggle-with-help">
                <label class="toggle toggle-help-inline">
                  <input type="checkbox" v-model="config.config.monitor.check_auth_url" />
                  <span class="toggle-slider"></span>
                  <span class="toggle-label">登录网址可达性检测</span>
                </label>
              </div>
            </div>
          </div>
        </div>
      </div>
    </section>

    <!-- 暂停时段 -->
    <section class="card settings-panel">
      <div class="settings-card-header">
        <IconApp name="clock" class="settings-card-icon" />
        <h2>暂停时段</h2>
      </div>
      <div class="card-body">
        <div class="toggle-group">
          <div class="toggle-with-help">
            <label class="toggle toggle-help-inline">
              <input type="checkbox" v-model="config.config.pause.enabled" />
              <span class="toggle-slider"></span>
              <span class="toggle-label">启用暂停时段</span>
            </label>
          </div>
        </div>
        <div class="form-row">
          <div class="form-group">
            <label for="settings-pause-start">暂停开始（小时）</label>
            <input id="settings-pause-start" v-model.number="config.config.pause.start_hour" type="number" min="0" max="23" />
          </div>
          <div class="form-group">
            <label for="settings-pause-end">暂停结束（小时）</label>
            <input id="settings-pause-end" v-model.number="config.config.pause.end_hour" type="number" min="0" max="23" />
          </div>
        </div>
      </div>
    </section>
  </div>
</template>
