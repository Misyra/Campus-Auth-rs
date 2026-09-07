<script setup lang="ts">
import IconApp from "@/components/common/IconApp.vue";
import CustomSelect from "@/components/common/CustomSelect.vue";
import FieldHelp from "@/components/common/FieldHelp.vue";
import { computed, onMounted, ref } from "vue";
import { useConfig } from "@/composables/useConfig";
import { useConfirm } from "@/composables/useConfirm";
import { systemApi, configApi } from "@/api";
import type { UpdateState, UpdateInfo } from "@/api/types";

const config = useConfig();
const { confirm } = useConfirm();

const checkFrequencyOptions = [
  { value: "0", label: "每次启动" },
  { value: "24", label: "每天一次" },
  { value: "168", label: "每周一次" },
] as const;
const checkFrequency = computed<string>({
  get: () => {
    const h = config.config.updater.check_interval_hours ?? 24;
    if (h <= 0) return "0";
    return h < 168 ? "24" : "168";
  },
  set: (v) => { config.config.updater.check_interval_hours = Number(v); },
});

const channelOptions = [
  { value: "stable", label: "正式版" },
  { value: "prerelease", label: "测试版" },
  { value: "all", label: "全通道最新版" },
] as const;

const updateChecking = ref(false);
const updating = ref(false);
const updateState = ref<UpdateState | null>(null);
const updateInfo = ref<UpdateInfo | null>(null);

async function refreshUpdateState() {
  try { updateState.value = await systemApi.updateState(); } catch { updateState.value = null; }
}
const lastCheckLabel = computed(() => {
  const s = updateState.value;
  if (!s?.last_check_at) return "从未检查";
  const d = new Date(s.last_check_at);
  return Number.isNaN(d.getTime()) ? "" : d.toLocaleString();
});
const updateCheckHint = computed(() => {
  const s = updateState.value;
  if (!s) return "";
  if (s.error) return `上次检查失败：${s.error}`;
  if (s.has_update) return `发现新版本 v${s.latest_version}`;
  return s.latest_version ? `当前已是最新（远程 v${s.latest_version}）` : "";
});
async function manualCheckUpdate() {
  updateChecking.value = true;
  updateInfo.value = null;
  try {
    const info = await systemApi.checkUpdate();
    updateInfo.value = info as unknown as UpdateInfo;
  } catch (e: unknown) {
    updateInfo.value = { has_update: false, error: (e as Error).message || "检查失败" };
  } finally {
    updateChecking.value = false;
    await refreshUpdateState();
  }
}
async function applyUpdate() {
  updating.value = true;
  try {
    const info = updateInfo.value;
    const pin =
      info?.latest && info.url && typeof info.sha256 === "string"
        ? { version: info.latest, url: info.url, sha256: info.sha256 }
        : undefined;
    const data = await systemApi.update(pin);
    updateInfo.value = {
      has_update: false,
      message: (data.message as string) || "更新已就绪，重启后生效",
    };
    const ok = await confirm({
      title: "更新已就绪",
      message: "更新已下载完成，是否立即重启应用以生效？",
      confirmText: "立即重启",
    });
    if (ok) {
      try {
        await systemApi.shutdown();
      } catch {
        if (updateInfo.value) updateInfo.value.message = "更新已就绪，但自动重启失败，请手动重启应用";
      }
    }
  } catch (e: unknown) {
    updateInfo.value = { has_update: false, error: (e as Error).message || "更新失败" };
  } finally {
    updating.value = false;
  }
}

const autoRestartOptions = [
  { value: "0", label: "不启用" },
  { value: "6", label: "每 6 小时" },
  { value: "12", label: "每 12 小时" },
  { value: "24", label: "每 24 小时" },
  { value: "48", label: "每 48 小时" },
  { value: "168", label: "每 168 小时（每周）" },
] as const;
const autoRestartHours = computed<string>({
  get: () => String(config.config.app_settings.auto_restart_hours ?? 0),
  set: (v) => { config.config.app_settings.auto_restart_hours = Number(v); },
});

const reloading = ref(false);
const reloadMsg = ref("");
async function reloadConfig() {
  reloading.value = true;
  reloadMsg.value = "";
  try {
    await configApi.reload();
    reloadMsg.value = "配置已重新加载";
  } catch (e: unknown) {
    reloadMsg.value = "重新加载失败：" + ((e as Error).message || "未知错误");
  } finally { reloading.value = false; }
}

onMounted(() => { void refreshUpdateState(); });
</script>

<template>
  <div class="settings-panel-grid settings-panel-grid--cols2 network-page">
    <!-- 网络、端口与代理 -->
    <section class="card settings-panel settings-panel--wide">
      <div class="settings-card-header">
        <IconApp name="globe" class="settings-card-icon" />
        <h2>网络、端口与代理</h2>
      </div>
      <div class="card-body">
        <div class="form-group">
          <div class="field-label-row">
            <label for="settings-app-port">控制台端口</label>
            <FieldHelp text="Web 控制台的监听端口。修改后重启生效，默认 50721。" />
          </div>
          <input id="settings-app-port" v-model.number="config.config.app_settings.port" type="number" min="1024" max="65535" />
          <span class="hint">本机访问地址一般为 http://127.0.0.1:端口</span>
        </div>
        <div class="toggle-group">
          <div class="toggle-with-help">
            <label class="toggle toggle-help-inline">
              <input type="checkbox" v-model="config.config.updater.use_proxy" />
              <span class="toggle-slider"></span>
              <span class="toggle-label">使用代理下载更新</span>
            </label>
            <FieldHelp text="仅影响版本更新检查、下载与任务仓库。检测流量的代理设置见“检测”页。" />
          </div>
        </div>
        <div class="form-group">
          <div class="field-label-row">
            <label for="settings-proxy-url">代理地址</label>
            <FieldHelp text="完整的 HTTP 代理地址，如 http://127.0.0.1:7890。仅在启用后生效。" />
          </div>
          <input
            id="settings-proxy-url"
            v-model="config.config.updater.proxy_url"
            type="text"
            placeholder="http://127.0.0.1:7890"
            spellcheck="false"
            :disabled="!config.config.updater.use_proxy"
          />
        </div>
      </div>
    </section>

    <!-- 自动更新 -->
    <section class="card settings-panel settings-panel--wide">
      <div class="settings-card-header">
        <IconApp name="download" class="settings-card-icon" />
        <h2>自动更新</h2>
      </div>
      <div class="card-body settings-grid-2col">
        <div>
          <div class="toggle-group">
            <div class="toggle-with-help">
              <label class="toggle toggle-help-inline">
                <input type="checkbox" v-model="config.config.updater.auto_check_enabled" />
                <span class="toggle-slider"></span>
                <span class="toggle-label">自动检查更新</span>
              </label>
              <FieldHelp text="关闭后不再自动检查更新，仅保留手动“立即检查”。保存后即时生效。" />
            </div>
          </div>
          <div class="form-group">
            <div class="field-label-row">
              <label>检查频率</label>
              <FieldHelp text="每次启动：仅启动时检查一次；每天/每周：启动时先检查一次，之后按周期自动检查。" />
            </div>
            <CustomSelect
              v-model="checkFrequency"
              :options="checkFrequencyOptions"
              :disabled="!config.config.updater.auto_check_enabled"
            />
          </div>
        </div>
        <div>
          <div class="form-group">
            <div class="field-label-row">
              <label>更新通道</label>
              <FieldHelp text="正式版仅跟随稳定发布；测试版跟随预发布（alpha/beta）；全通道最新版取两者中更高者。" />
            </div>
            <div class="update-channel-segmented" role="group" aria-label="更新通道">
              <button
                v-for="opt in channelOptions"
                :key="opt.value"
                type="button"
                :class="{ active: config.config.updater.channel === opt.value }"
                @click="config.config.updater.channel = opt.value"
              >
                {{ opt.label }}
              </button>
            </div>
          </div>
          <div class="form-group">
            <div class="field-label-row">
              <label>上次检查时间</label>
              <FieldHelp text="记录最近一次手动或自动检查的结果，重启后保留。" />
            </div>
            <div class="update-check-row">
              <button class="btn btn-secondary btn-sm" :disabled="updateChecking" @click="manualCheckUpdate">
                <IconApp v-if="updateChecking" name="refresh" class="spin" />
                {{ updateChecking ? "检查中..." : "立即检查" }}
              </button>
              <span v-if="lastCheckLabel" class="hint">{{ lastCheckLabel }}</span>
            </div>
            <span v-if="updateCheckHint && !updateInfo" class="hint update-check-hint" :class="{ 'update-check-error': !!updateState?.error }">{{ updateCheckHint }}</span>
            <!-- 检查结果：与关于页原逻辑一致，检查后原地展示更新/下载入口 -->
            <div v-if="updateInfo && !updateInfo.error && !(updateInfo as unknown as { message?: string }).message" class="update-result">
              <div v-if="updateInfo.has_update" class="update-available">
                <IconApp name="upload" width="16" height="16" />
                <span>发现新版本 <strong>v{{ updateInfo.latest }}</strong></span>
                <button class="btn btn-primary btn-sm" :disabled="updating" @click="applyUpdate">{{ updating ? "更新中..." : "立即更新" }}</button>
                <a v-if="updateInfo.url" :href="updateInfo.url" target="_blank" rel="noopener noreferrer" class="btn btn-ghost btn-sm">前往下载</a>
              </div>
              <div v-else class="update-latest">
                <IconApp name="check" width="16" height="16" />
                <span>当前已是最新版本</span>
              </div>
            </div>
            <div v-else-if="updateInfo && (updateInfo as unknown as { message?: string }).message" class="update-success">
              <IconApp name="check" width="16" height="16" />
              <span>{{ (updateInfo as unknown as { message: string }).message }}，请重启程序生效</span>
            </div>
            <div v-else-if="updateInfo && updateInfo.error" class="update-error">{{ updateInfo.error }}</div>
          </div>
        </div>
      </div>
    </section>

    <!-- 维护操作 -->
    <section class="card settings-panel settings-panel--wide">
      <div class="settings-card-header">
        <IconApp name="sliders" class="settings-card-icon" />
        <h2>维护操作</h2>
      </div>
      <div class="card-body settings-grid-2col">
        <div class="form-group">
          <div class="field-label-row">
            <label for="settings-auto-restart">定时自重启</label>
            <FieldHelp text="按运行时长周期性重启本程序，以回收内存。先启动新进程再退出旧进程，修改即时生效。" />
          </div>
          <CustomSelect v-model="autoRestartHours" :options="autoRestartOptions" />
        </div>
        <div class="form-group">
          <div class="field-label-row">
            <label>配置热重载</label>
            <FieldHelp text="从磁盘重新读取配置文件并应用，无需重启。日常修改请使用下方的保存按钮。" />
          </div>
          <button class="btn btn-secondary btn-sm" @click="reloadConfig" :disabled="reloading">
            {{ reloading ? "加载中..." : "重新加载配置" }}
          </button>
          <span v-if="reloadMsg" class="hint">{{ reloadMsg }}</span>
        </div>
      </div>
    </section>
  </div>
</template>
