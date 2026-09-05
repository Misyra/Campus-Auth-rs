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
  <div class="settings-panel-grid settings-panel-grid--cols2">
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
              <FieldHelp text="两次网络检测之间的间隔。过短增加资源消耗，过长延迟断线发现。建议 180~600 秒，默认 300 秒。" />
            </div>
            <input id="settings-interval" v-model.number="config.config.monitor.check_interval_seconds" type="number" min="10" max="86400" />
          </div>
          <div class="form-group">
            <div class="field-label-row">
              <label for="settings-network-check-timeout">检测超时（秒）</label>
              <FieldHelp text="单次检测的等待上限。默认 2 秒，弱网环境可放宽至 5 秒。" />
            </div>
            <input id="settings-network-check-timeout" v-model.number="config.config.monitor.network_check_timeout" type="number" min="1" max="30" />
          </div>
        </div>
        <div class="form-row settings-toggle-spacer">
          <div class="form-group">
            <div class="field-label-row">
              <label for="settings-max-retries">最大重试次数</label>
              <FieldHelp text="登录失败后的最大重试次数。默认 3 次。" />
            </div>
            <input id="settings-max-retries" v-model.number="config.config.retry.max_retries" type="number" min="1" max="5" />
          </div>
          <div class="form-group">
            <div class="field-label-row">
              <label for="settings-retry-interval">重试间隔（秒）</label>
              <FieldHelp text="相邻两次重试之间的间隔。过短可能触发登录页限流。默认 5 秒。" />
            </div>
            <input id="settings-retry-interval" v-model.number="config.config.retry.retry_interval" type="number" min="1" max="300" />
          </div>
        </div>
        <div class="form-row settings-toggle-spacer">
          <div class="form-group">
            <div class="field-label-row">
              <label for="settings-post-login-delay">登录后延迟（秒）</label>
              <FieldHelp text="登录完成后等待认证生效的时间，之后再复查网络。默认 5 秒。" />
            </div>
            <input id="settings-post-login-delay" v-model.number="config.config.monitor.post_login_delay" type="number" min="0" max="60" />
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
            <FieldHelp text="启用后在该时段内暂停检测与登录，适用于定时断网时段。" />
          </div>
        </div>
        <div class="form-row">
          <div class="form-group">
            <label for="settings-pause-start">开始时间（时）</label>
            <input id="settings-pause-start" v-model.number="config.config.pause.start_hour" type="number" min="0" max="23" />
          </div>
          <div class="form-group">
            <label for="settings-pause-end">结束时间（时）</label>
            <input id="settings-pause-end" v-model.number="config.config.pause.end_hour" type="number" min="0" max="23" />
          </div>
        </div>
        <span class="hint">支持跨天，例如开始 22、结束 6 表示每晚 22:00 至次日 6:00</span>
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
                </label>
                <FieldHelp text="通过 TCP 连接判断网络连通性，开销最小。" />
              </div>
            </div>
            <div v-if="config.config.monitor.enable_tcp_check" class="form-group settings-toggle-compact">
              <div class="field-label-row">
                <label for="settings-network-targets">TCP 检测目标</label>
                <FieldHelp text="多个目标以英文逗号分隔；省略端口时默认为 53。" />
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
                </label>
                <FieldHelp text="通过 HTTP 请求判断网络连通性，更接近真实上网行为。" />
              </div>
            </div>
            <div v-if="config.config.monitor.enable_http_check" class="form-group settings-toggle-compact">
              <div class="field-label-row">
                <label for="settings-http-targets">HTTP 检测目标</label>
                <FieldHelp text="建议使用返回 204 的轻量探针地址。" />
              </div>
              <input id="settings-http-targets"
                :value="config.config.monitor.test_urls.join(',')"
                @input="config.config.monitor.test_urls = ($event.target as HTMLInputElement).value.split(',').map(s => s.trim()).filter(Boolean)"
                type="text" placeholder="https://connect.rom.miui.com/generate_204" />
            </div>
            <div class="toggle-group settings-toggle-spacer">
              <div class="toggle-with-help">
                <label class="toggle toggle-help-inline">
                  <input type="checkbox" v-model="urlCheckEnabled" />
                  <span class="toggle-slider"></span>
                  <span class="toggle-label">URL 标题检测</span>
                </label>
                <FieldHelp text="请求页面并核对关键字，判断是否被劫持至登录页。每行一条：地址|关键字。" />
              </div>
            </div>
            <div v-if="urlCheckEnabled" class="form-group settings-toggle-compact">
              <div class="field-label-row">
                <label for="settings-url-check">URL 检测目标</label>
              </div>
              <textarea id="settings-url-check" v-model="urlCheckText" rows="4" class="settings-monospace-textarea"
                placeholder="https://captive.apple.com|Success&#10;https://detectportal.firefox.com|success&#10;https://msftconnecttest.com|Microsoft Connect Test"></textarea>
            </div>
          </div>
          <div class="settings-detect-col">
            <h4 class="settings-detect-heading">登录前检测</h4>
            <div class="toggle-group">
              <div class="toggle-with-help">
                <label class="toggle toggle-help-inline">
                  <input type="checkbox" v-model="config.config.monitor.enable_local_check" />
                  <span class="toggle-slider"></span>
                  <span class="toggle-label">网卡连接检查</span>
                </label>
                <FieldHelp text="物理网络全部断开时直接判定离线，跳过外网探测与登录。" />
              </div>
            </div>
            <div class="toggle-group settings-toggle-spacer">
              <div class="toggle-with-help">
                <label class="toggle toggle-help-inline">
                  <input type="checkbox" v-model="config.config.monitor.check_auth_url" />
                  <span class="toggle-slider"></span>
                  <span class="toggle-label">认证地址可达检查</span>
                </label>
                <FieldHelp text="登录前确认认证地址可达；不可达则跳过本次登录。" />
              </div>
            </div>
            <div class="toggle-group settings-toggle-spacer">
              <div class="toggle-with-help">
                <label class="toggle toggle-help-inline">
                  <input type="checkbox" v-model="config.config.monitor.disable_proxy" />
                  <span class="toggle-slider"></span>
                  <span class="toggle-label">监测不走代理</span>
                </label>
                <FieldHelp text="启用后监测流量直连；关闭则跟随系统代理。修改后重启生效。" />
              </div>
            </div>
          </div>
        </div>
      </div>
    </section>

  </div>
</template>
