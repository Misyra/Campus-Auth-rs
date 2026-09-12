<script setup lang="ts">
/** 设置 · 检测页：检测与重试、暂停时段及网络检测方式配置 */
import IconApp from "@/components/common/IconApp.vue";
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
              <FieldHelp text="两次网络检测之间的间隔。过短增加资源消耗，过长延迟断线发现。建议 60~300 秒，默认 120 秒（后端钳制 20~1200 秒）。" />
            </div>
            <input id="settings-interval" v-model.number="config.config.monitor.check_interval_seconds" type="number" min="20" max="1200" />
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
              <FieldHelp text="首次重试的等待间隔，之后每次重试翻倍（如 5 → 10 → 20 秒）。过短可能触发登录页限流。默认 5 秒。" />
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
    <section class="card settings-panel pause-card">
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
                  <input type="checkbox" v-model="config.config.monitor.enable_http_check" />
                  <span class="toggle-slider"></span>
                  <span class="toggle-label">204 门户检测 <span class="badge badge--sm badge--info">推荐开启</span></span>
                </label>
                <FieldHelp text="默认开启。请求 generate_204 端点：204 表示公网在线，200 或跳转表示被认证门户劫持。这是自动恢复的主要证据。" />
              </div>
            </div>
            <div v-if="config.config.monitor.enable_http_check" class="form-group settings-toggle-compact">
              <div class="field-label-row">
                <label for="settings-http-targets">204 门户检测目标</label>
                <FieldHelp text="必须填写返回 204 的轻量端点，普通网页返回 200 会被视为门户劫持证据、不能填在这里。每行一个地址，也兼容英文逗号分隔。" />
              </div>
              <textarea id="settings-http-targets" v-model="httpCheckText" rows="3" class="settings-monospace-textarea"
                placeholder="http://connect.rom.miui.com/generate_204&#10;http://www.gstatic.com/generate_204"></textarea>
            </div>
            <div class="toggle-group settings-toggle-spacer">
              <div class="toggle-with-help">
                <label class="toggle toggle-help-inline">
                  <input type="checkbox" v-model="config.config.monitor.enable_tcp_check" />
                  <span class="toggle-slider"></span>
                  <span class="toggle-label">TCP 检测（补充）</span>
                </label>
                <FieldHelp text="仅补充传输层证据。TCP 成功不代表网页可访问，因此它单独成功时不会判定公网在线，也不会直接触发登录。" />
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
                  <input type="checkbox" v-model="urlCheckEnabled" />
                  <span class="toggle-slider"></span>
                  <span class="toggle-label">URL 内容检测（补充）</span>
                </label>
                <FieldHelp text="用页面关键字补充确认公网或门户劫持。适合 204 端点在本网络不稳定时启用；每行一条：地址|关键字。" />
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
            <h4 class="settings-detect-heading">诊断与恢复辅助</h4>
            <div class="toggle-group">
              <div class="toggle-with-help">
                <label class="toggle toggle-help-inline">
                  <input type="checkbox" v-model="config.config.monitor.enable_local_check" />
                  <span class="toggle-slider"></span>
                  <span class="toggle-label">手动测试时检查网卡</span>
                </label>
                <FieldHelp text="仅手动「网络测试」时运行的诊断说明，与公网探测并行执行；自动监测与自动登录均不做网卡检查，开启与否也不影响登录能否进行。" />
              </div>
            </div>
            <div class="toggle-group settings-toggle-spacer">
              <div class="toggle-with-help">
                <label class="toggle toggle-help-inline">
                  <input type="checkbox" v-model="config.config.monitor.check_auth_url" />
                  <span class="toggle-slider"></span>
                  <span class="toggle-label">手动登录前检查认证地址</span>
                </label>
                <FieldHelp text="手动登录前先直连确认认证地址可达，不可达则直接失败、不启动浏览器。自动登录不做此项：触发前已有门户劫持的强证据。默认关闭：部分校园网限制直连，开启可能误拦本可成功的登录。" />
              </div>
            </div>
            <div class="toggle-group settings-toggle-spacer">
              <div class="toggle-with-help">
                <label class="toggle toggle-help-inline">
                  <input type="checkbox" v-model="config.config.monitor.disable_proxy" />
                  <span class="toggle-slider"></span>
                  <span class="toggle-label">检测不走代理</span>
                </label>
                <FieldHelp text="启用后公网检测流量直连，避免系统代理故障造成误判；关闭则跟随系统代理。保存后下一轮检测生效。" />
              </div>
            </div>
          </div>
        </div>
      </div>
    </section>

  </div>
</template>
