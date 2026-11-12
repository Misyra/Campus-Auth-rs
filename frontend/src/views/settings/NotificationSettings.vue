<script setup lang="ts">
/**
 * 设置 · 推送通知：通道增删改与测试发送、触发事件类型、免打扰时段与重试参数。
 *
 * 通道模型为可辨识联合（type 判别）：下拉切换 type 即地重建该通道为对应变体
 * （保留 id/名称/启用/超时，其余字段落新类型默认值），字段表单随类型切换。
 * 「测试发送」就地显示三分支结果（发送中 / 成功 / 失败摘要，错误已脱敏）。
 * 所有改动只写本地 config 草稿，经页面统一「保存」落盘（后端校验兜底）。
 */
import CustomSelect from "@/components/common/CustomSelect.vue";
import FieldHelp from "@/components/common/FieldHelp.vue";
import SettingsRow from "@/components/common/SettingsRow.vue";
import { notificationApi } from "@/api";
import { extractApiError } from "@/api/client";
import type {
  NotificationChannelConfig,
  NotificationChannelType,
  NotificationEventType,
  NotificationHeader,
  WebhookChannelConfig,
} from "@/api/types";
import { computed, ref } from "vue";
import { useConfig } from "@/composables/useConfig";
import { useToast } from "@/composables/useToast";
import type { SelectOption } from "@/components/common/CustomSelect.vue";

const config = useConfig();
const { toastOnly } = useToast();

const notification = computed(() => config.config.notification);

// ===== 通道类型 =====

const CHANNEL_TYPE_OPTIONS: SelectOption[] = [
  { value: "webhook", label: "Webhook（自定义）" },
  { value: "bark", label: "Bark" },
  { value: "serverchan", label: "Server酱" },
  { value: "telegram", label: "Telegram" },
  { value: "os", label: "系统通知" },
];

const TYPE_LABELS: Record<NotificationChannelType, string> = {
  webhook: "Webhook",
  bark: "Bark",
  serverchan: "Server酱",
  telegram: "Telegram",
  os: "系统通知",
};

function typeLabel(type: NotificationChannelType): string {
  return TYPE_LABELS[type] ?? type;
}

const MAX_CHANNELS = 10;

/** 通道 id 生成：时间戳 + 随机后缀，仅用于前端配对与结果回填（后端唯一性校验兜底） */
function nextChannelId(): string {
  return `ch-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 6)}`;
}

/** 新通道默认值：按类型给出最小可用草稿（超时 10s 与后端默认一致） */
function makeChannel(type: NotificationChannelType): NotificationChannelConfig {
  const base = { id: nextChannelId(), name: "", enabled: true, timeout_seconds: 10 };
  switch (type) {
    case "webhook":
      return { type: "webhook", ...base, url: "", headers: [], body_template: "" };
    case "bark":
      return { type: "bark", ...base, server: "", device_key: "" };
    case "serverchan":
      return { type: "serverchan", ...base, send_key: "" };
    case "telegram":
      return { type: "telegram", ...base, bot_token: "", chat_id: "" };
    case "os":
      // 系统通知无凭据字段，公共字段即全部表单
      return { type: "os", ...base };
  }
}

const addType = ref<NotificationChannelType>("webhook");

/** 模板占位符说明（字面含 {{ }}，不能直接写进文本节点，交给插值变量） */
const PLACEHOLDER_HELP = "{{event_type}}、{{summary}}、{{profile}}、{{time}}、{{detail}}";

function addChannel(): void {
  if (notification.value.channels.length >= MAX_CHANNELS) {
    toastOnly(false, `推送通道最多 ${MAX_CHANNELS} 个`);
    return;
  }
  notification.value.channels.push(makeChannel(addType.value));
}

/** 类型下拉切换：就地重建为对应变体（保留公共字段，其余落新类型默认值） */
function changeChannelType(channel: NotificationChannelConfig, type: string): void {
  if (type === channel.type) return;
  const rebuilt = makeChannel(type as NotificationChannelType);
  rebuilt.id = channel.id;
  rebuilt.name = channel.name;
  rebuilt.enabled = channel.enabled;
  // os 通道的 timeout_seconds 可选（后端 serde default 缺省 10），缺省落 10
  rebuilt.timeout_seconds = channel.timeout_seconds ?? 10;
  const index = notification.value.channels.indexOf(channel);
  if (index >= 0) notification.value.channels.splice(index, 1, rebuilt);
  // 类型切换丢弃旧类型字段（如密钥），同步清掉旧的测试结果避免误导
  clearTestResult(rebuilt.id);
}

function removeChannel(id: string): void {
  const index = notification.value.channels.findIndex((c) => c.id === id);
  if (index >= 0) notification.value.channels.splice(index, 1);
  clearTestResult(id);
}

// ===== webhook 自定义请求头 =====

function addHeader(channel: WebhookChannelConfig): void {
  channel.headers.push({ name: "", value: "" } satisfies NotificationHeader);
}

function removeHeader(channel: WebhookChannelConfig, index: number): void {
  channel.headers.splice(index, 1);
}

// ===== 测试发送 =====

type TestState = { status: "testing" } | { status: "success" } | { status: "fail"; message: string };
const testStates = ref<Record<string, TestState>>({});

function testStateOf(id: string): TestState | undefined {
  return testStates.value[id];
}

/** 失败分支的错误摘要（模板表达式不写复杂类型断言，收进函数） */
function failMessageOf(id: string): string {
  const state = testStates.value[id];
  return state && state.status === "fail" ? state.message : "";
}

function clearTestResult(id: string): void {
  delete testStates.value[id];
}

async function testChannel(channel: NotificationChannelConfig): Promise<void> {
  if (testStates.value[channel.id]?.status === "testing") return;
  testStates.value[channel.id] = { status: "testing" };
  try {
    const data = await notificationApi.test({ channels: [channel] });
    const result = data.results[0];
    if (result?.success) {
      testStates.value[channel.id] = { status: "success" };
    } else {
      testStates.value[channel.id] = {
        status: "fail",
        message: result?.error || "发送失败，请检查通道配置",
      };
    }
  } catch (error) {
    testStates.value[channel.id] = { status: "fail", message: extractApiError(error, "测试发送失败") };
  }
}

// ===== 事件类型开关组 =====

const EVENT_OPTIONS: { type: NotificationEventType; label: string; hint: string }[] = [
  { type: "login_succeeded", label: "登录成功", hint: "每次自动/手动登录成功" },
  { type: "login_failed", label: "登录失败", hint: "推荐开启：登录尝试失败（含原因）" },
  { type: "network_down", label: "网络断开", hint: "推荐开启：探测由通转断" },
  { type: "network_up", label: "网络恢复", hint: "探测由断转通" },
  { type: "task_finished", label: "任务完成", hint: "自定义任务执行成功" },
  { type: "task_failed", label: "任务失败", hint: "自定义任务执行失败" },
  { type: "update_available", label: "发现更新", hint: "检查到新版本" },
  { type: "environment_ready", label: "环境就绪", hint: "Python/uv 环境引导完成" },
  { type: "profile_switched", label: "档案切换", hint: "自动/手动切换活跃方案" },
];

/** 复选框数组 v-model 的成员判定/增删（checkbox 多选语义） */
function hasEvent(type: NotificationEventType): boolean {
  return notification.value.events.includes(type);
}

function toggleEvent(type: NotificationEventType, checked: boolean): void {
  const events = notification.value.events;
  if (checked && !events.includes(type)) events.push(type);
  if (!checked) {
    const index = events.indexOf(type);
    if (index >= 0) events.splice(index, 1);
  }
}

// ===== 免打扰时段（type=time 与 后端 时/分 字段互转） =====

function pad2(value: number): string {
  return String(value).padStart(2, "0");
}

const quietStart = computed<string>({
  get: () => `${pad2(notification.value.quiet_start_hour)}:${pad2(notification.value.quiet_start_minute)}`,
  set: (value: string) => {
    const [h, m] = value.split(":").map(Number);
    if (Number.isFinite(h)) notification.value.quiet_start_hour = Math.min(23, Math.max(0, h));
    if (Number.isFinite(m)) notification.value.quiet_start_minute = Math.min(59, Math.max(0, m));
  },
});

const quietEnd = computed<string>({
  get: () => `${pad2(notification.value.quiet_end_hour)}:${pad2(notification.value.quiet_end_minute)}`,
  set: (value: string) => {
    const [h, m] = value.split(":").map(Number);
    if (Number.isFinite(h)) notification.value.quiet_end_hour = Math.min(23, Math.max(0, h));
    if (Number.isFinite(m)) notification.value.quiet_end_minute = Math.min(59, Math.max(0, m));
  },
});
</script>

<template>
  <div class="settings-list-page notification-settings-page">
    <section class="settings-panel">
      <div class="settings-card-header">
        <h2>推送设置</h2>
      </div>
      <div class="card-body">
        <SettingsRow description="关闭后不推送任何通知；「测试发送」不受此开关限制">
          <template #label>
            启用推送通知
            <FieldHelp text="总开关。开启前建议先添加通道并用「测试发送」验证。通知发送失败会自动重试并只记日志，不影响自动登录。" />
          </template>
          <label class="toggle setting-row-switch">
            <input
              type="checkbox"
              role="switch"
              :aria-checked="notification.enabled"
              v-model="notification.enabled"
            />
            <span class="toggle-slider"></span>
            <span class="sr-only">启用推送通知</span>
          </label>
        </SettingsRow>
        <SettingsRow description="失败后按基准间隔逐次翻倍重试（如 2 → 4 → 8 秒），耗尽后只记日志">
          <template #label>
            <label for="settings-notification-retry">失败重试次数</label>
            <FieldHelp text="单次推送失败后的重试次数（不含首次）。0 表示只发送一次；退避间隔随次数翻倍。" />
          </template>
          <input
            id="settings-notification-retry"
            v-model.number="notification.retry_max_attempts"
            type="number"
            min="0"
            max="10"
          />
        </SettingsRow>
        <SettingsRow description="首次重试前的等待秒数，之后逐次翻倍">
          <template #label>
            <label for="settings-notification-retry-base">重试基准间隔（秒）</label>
          </template>
          <input
            id="settings-notification-retry-base"
            v-model.number="notification.retry_base_interval_seconds"
            type="number"
            min="1"
            max="60"
          />
        </SettingsRow>
      </div>
    </section>

    <section class="settings-panel">
      <div class="settings-card-header">
        <h2>触发事件</h2>
      </div>
      <div class="card-body">
        <p class="hint">勾选需要推送的事件类型；全不勾选等于不推送任何通知。</p>
        <div class="notif-event-chips" role="group" aria-label="触发事件类型">
          <label
            v-for="event in EVENT_OPTIONS"
            :key="event.type"
            class="notif-event-chip"
            :class="{ 'notif-event-chip--on': hasEvent(event.type) }"
            :title="event.hint"
          >
            <input
              type="checkbox"
              :checked="hasEvent(event.type)"
              :aria-label="event.label"
              @change="toggleEvent(event.type, ($event.target as HTMLInputElement).checked)"
            />
            <span>{{ event.label }}</span>
          </label>
        </div>
      </div>
    </section>

    <section class="settings-panel">
      <div class="settings-card-header">
        <h2>免打扰时段</h2>
      </div>
      <div class="card-body">
        <SettingsRow description="时段内事件只记日志不推送，支持跨零点（如 22:00–08:00）">
          <template #label>
            启用免打扰时段
            <FieldHelp text="免打扰期间照常记录事件，只是不推送。开始与结束时间相同视为全天免打扰。" />
          </template>
          <label class="toggle setting-row-switch">
            <input
              type="checkbox"
              role="switch"
              :aria-checked="notification.quiet_enabled"
              v-model="notification.quiet_enabled"
            />
            <span class="toggle-slider"></span>
            <span class="sr-only">启用免打扰时段</span>
          </label>
          <template #details>
            <SettingsRow description="使用 24 小时制">
              <template #label>
                <label for="settings-notification-quiet-start">开始时间</label>
              </template>
              <input
                id="settings-notification-quiet-start"
                v-model="quietStart"
                type="time"
                :disabled="!notification.quiet_enabled"
              />
            </SettingsRow>
            <SettingsRow description="支持次日时间，如 22:00 开始、08:00 结束">
              <template #label>
                <label for="settings-notification-quiet-end">结束时间</label>
              </template>
              <input
                id="settings-notification-quiet-end"
                v-model="quietEnd"
                type="time"
                :disabled="!notification.quiet_enabled"
              />
            </SettingsRow>
          </template>
        </SettingsRow>
      </div>
    </section>

    <section class="settings-panel">
      <div class="settings-card-header">
        <h2>推送通道（{{ notification.channels.length }}/10）</h2>
      </div>
      <div class="card-body">
        <p class="hint">
          支持自定义 Webhook 与 Bark、Server酱、Telegram 预设，以及本机系统通知；改动需点击页面底部「保存」后生效。
          密钥类字段明文存储在本机配置中（与代理地址同级别保护）。
        </p>

        <div
          v-for="(channel, index) in notification.channels"
          :key="channel.id"
          class="notif-channel-card"
        >
          <div class="notif-channel-header">
            <CustomSelect
              :model-value="channel.type"
              :options="CHANNEL_TYPE_OPTIONS"
              compact
              :aria-label="`通道 ${index + 1} 类型`"
              @update:model-value="(value: string) => changeChannelType(channel, value)"
            />
            <input
              v-model="channel.name"
              type="text"
              class="notif-channel-name"
              :placeholder="typeLabel(channel.type)"
              :aria-label="`通道 ${index + 1} 名称`"
            />
            <label class="toggle setting-row-switch">
              <input
                type="checkbox"
                role="switch"
                :aria-checked="channel.enabled"
                v-model="channel.enabled"
              />
              <span class="toggle-slider"></span>
              <span class="sr-only">启用通道 {{ channel.name || typeLabel(channel.type) }}</span>
            </label>
            <button
              type="button"
              class="btn btn-sm btn-secondary"
              :disabled="testStateOf(channel.id)?.status === 'testing'"
              @click="testChannel(channel)"
            >
              测试发送
            </button>
            <button
              type="button"
              class="btn btn-sm btn-danger"
              :aria-label="`删除通道 ${channel.name || typeLabel(channel.type)}`"
              @click="removeChannel(channel.id)"
            >
              删除
            </button>
          </div>

          <div class="notif-channel-body">
            <!-- Webhook：地址 + 自定义头 + JSON 模板 -->
            <template v-if="channel.type === 'webhook'">
              <div class="notif-field-grid">
                <label :for="`notif-url-${channel.id}`">目标地址</label>
                <input
                  :id="`notif-url-${channel.id}`"
                  v-model="channel.url"
                  type="text"
                  placeholder="https://example.com/hook"
                  autocomplete="off"
                />
                <label :for="`notif-timeout-${channel.id}`">超时（秒）</label>
                <input
                  :id="`notif-timeout-${channel.id}`"
                  v-model.number="channel.timeout_seconds"
                  type="number"
                  min="1"
                  max="60"
                />
              </div>
              <div v-for="(header, headerIndex) in channel.headers" :key="headerIndex" class="notif-header-row">
                <input
                  v-model="header.name"
                  type="text"
                  placeholder="头名称（如 Authorization）"
                  :aria-label="`通道 ${index + 1} 请求头 ${headerIndex + 1} 名称`"
                  autocomplete="off"
                />
                <input
                  v-model="header.value"
                  type="text"
                  placeholder="头值（可为密钥）"
                  :aria-label="`通道 ${index + 1} 请求头 ${headerIndex + 1} 值`"
                  autocomplete="off"
                />
                <button
                  type="button"
                  class="btn btn-sm btn-danger"
                  :aria-label="`删除请求头 ${headerIndex + 1}`"
                  @click="removeHeader(channel, headerIndex)"
                >
                  删除
                </button>
              </div>
              <button
                type="button"
                class="btn btn-sm"
                :disabled="channel.headers.length >= 16"
                @click="addHeader(channel)"
              >
                添加请求头
              </button>
              <div class="notif-field-grid notif-field-grid--full">
                <label :for="`notif-template-${channel.id}`">JSON 模板</label>
                <textarea
                  :id="`notif-template-${channel.id}`"
                  v-model="channel.body_template"
                  class="textarea--mono"
                  rows="4"
                  placeholder='{"title": "{{summary}}", "body": "{{detail}}"}（留空使用默认模板）'
                ></textarea>
                <p class="hint">
                  占位符：{{ PLACEHOLDER_HELP }}。
                  保存时校验渲染结果必须是合法 JSON（占位符写在字符串值内）。
                </p>
              </div>
            </template>

            <!-- Bark -->
            <template v-else-if="channel.type === 'bark'">
              <div class="notif-field-grid">
                <label :for="`notif-server-${channel.id}`">服务器</label>
                <input
                  :id="`notif-server-${channel.id}`"
                  v-model="channel.server"
                  type="text"
                  placeholder="https://api.day.app（官方，可留空）"
                  autocomplete="off"
                />
                <label :for="`notif-key-${channel.id}`">Device Key</label>
                <input
                  :id="`notif-key-${channel.id}`"
                  v-model="channel.device_key"
                  type="text"
                  placeholder="Bark App 显示的设备 Key"
                  autocomplete="off"
                />
                <label :for="`notif-timeout-${channel.id}`">超时（秒）</label>
                <input
                  :id="`notif-timeout-${channel.id}`"
                  v-model.number="channel.timeout_seconds"
                  type="number"
                  min="1"
                  max="60"
                />
              </div>
            </template>

            <!-- Server酱 -->
            <template v-else-if="channel.type === 'serverchan'">
              <div class="notif-field-grid">
                <label :for="`notif-sendkey-${channel.id}`">SendKey</label>
                <input
                  :id="`notif-sendkey-${channel.id}`"
                  v-model="channel.send_key"
                  type="text"
                  placeholder="sct 开头的 SendKey（sct.ftqq.com 获取）"
                  autocomplete="off"
                />
                <label :for="`notif-timeout-${channel.id}`">超时（秒）</label>
                <input
                  :id="`notif-timeout-${channel.id}`"
                  v-model.number="channel.timeout_seconds"
                  type="number"
                  min="1"
                  max="60"
                />
              </div>
            </template>

            <!-- 系统通知：无凭据区，仅超时可调 -->
            <template v-else-if="channel.type === 'os'">
              <div class="notif-field-grid">
                <label :for="`notif-timeout-${channel.id}`">超时（秒）</label>
                <input
                  :id="`notif-timeout-${channel.id}`"
                  v-model.number="channel.timeout_seconds"
                  type="number"
                  min="1"
                  max="60"
                />
              </div>
            </template>

            <!-- Telegram -->
            <template v-else>
              <div class="notif-field-grid">
                <label :for="`notif-token-${channel.id}`">Bot Token</label>
                <input
                  :id="`notif-token-${channel.id}`"
                  v-model="channel.bot_token"
                  type="text"
                  placeholder="@BotFather 下发的 Token"
                  autocomplete="off"
                />
                <label :for="`notif-chat-${channel.id}`">Chat ID</label>
                <input
                  :id="`notif-chat-${channel.id}`"
                  v-model="channel.chat_id"
                  type="text"
                  placeholder="数字 ID 或 @频道名"
                  autocomplete="off"
                />
                <label :for="`notif-timeout-${channel.id}`">超时（秒）</label>
                <input
                  :id="`notif-timeout-${channel.id}`"
                  v-model.number="channel.timeout_seconds"
                  type="number"
                  min="1"
                  max="60"
                />
              </div>
            </template>

            <!-- 测试结果三分支：发送中 / 成功 / 失败摘要 -->
            <p
              v-if="testStateOf(channel.id)?.status === 'testing'"
              class="hint notif-test-result"
              role="status"
            >
              正在发送测试消息…
            </p>
            <p
              v-else-if="testStateOf(channel.id)?.status === 'success'"
              class="hint notif-test-result notif-test-result--ok"
              role="status"
            >
              测试消息已发送，请到接收端确认。
            </p>
            <p
              v-else-if="testStateOf(channel.id)?.status === 'fail'"
              class="hint notif-test-result notif-test-result--fail"
              role="alert"
            >
              发送失败：{{ failMessageOf(channel.id) }}
            </p>
          </div>
        </div>

        <div class="notif-add-row">
          <CustomSelect
            v-model="addType"
            :options="CHANNEL_TYPE_OPTIONS"
            aria-label="要添加的通道类型"
          />
          <button type="button" class="btn btn-sm btn-primary" @click="addChannel">添加通道</button>
        </div>
      </div>
    </section>
  </div>
</template>
