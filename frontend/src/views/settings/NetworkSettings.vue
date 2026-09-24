<script setup lang="ts">
/** 设置 · 网络页：监听端口与代理、自动更新通道及数据维护操作 */
import IconApp from "@/components/common/IconApp.vue";
import CustomSelect from "@/components/common/CustomSelect.vue";
import FieldHelp from "@/components/common/FieldHelp.vue";
import { computed, onMounted, ref } from "vue";
import { useConfig } from "@/composables/useConfig";
import { useConfirm } from "@/composables/useConfirm";
import { useStatus } from "@/composables/useStatus";
import { useToast } from "@/composables/useToast";
import { useUpdateDialog } from "@/composables/useUpdateDialog";
import { pickFile } from "@/utils/file";
import { systemApi, configApi } from "@/api";
import type { UpdateState, UpdateInfo } from "@/api/types";
import type { SelectOption } from "@/components/common/CustomSelect.vue";

const config = useConfig();
const { confirm } = useConfirm();
const { status } = useStatus();
const { toastOnly } = useToast();
/** 更新弹窗：本页的「立即检查」命中与「查看更新日志」入口都指向它 */
const update = useUpdateDialog();

// 显式标注 SelectOption[]：as const 的只读元组无法绑定 CustomSelect 的可变 options prop
const checkFrequencyOptions: SelectOption[] = [
  { value: "0", label: "每次启动" },
  { value: "24", label: "每天一次" },
  { value: "168", label: "每周一次" },
];
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
/** 下载进度（WS 状态快照的 update_progress，下载期间有值） */
const updateProgress = computed(() => status.update_progress ?? null);
const updateState = ref<UpdateState | null>(null);
const updateInfo = ref<UpdateInfo | null>(null);

/**
 * 本地安装包命中提示。
 *
 * 后端在检查阶段比对 `update/` 目录下文件与远程清单声明的 SHA256，命中即回报；
 * 内容不匹配（放的是旧版本）时不回报，此处自然为空。
 */
const localPackageHint = computed(() => {
  const pkg = updateInfo.value?.local_package;
  if (!pkg) return "";
  return `已在 update/ 目录找到匹配的安装包 ${pkg.file_name}，更新时将跳过下载`;
});

/** 数据目录（后端 base_path）；未取到时不展示绝对路径，避免给出错误位置 */
const basePath = ref("");
const updateDirLabel = computed(() =>
  basePath.value ? `${basePath.value}/update` : "程序数据目录下的 update/ 文件夹",
);

/** 复制数据目录路径：手输一长串路径容易错，给一键复制 */
async function copyUpdateDir() {
  if (!basePath.value) {
    toastOnly(false, "尚未取到数据目录，请稍后重试");
    return;
  }
  try {
    await navigator.clipboard.writeText(updateDirLabel.value);
    toastOnly(true, "已复制 update/ 目录路径");
  } catch {
    toastOnly(false, "复制失败，请手动选择路径文本");
  }
}

/**
 * 拉取数据目录（`GET /api/system/info` 的 `base_path`）。
 *
 * 失败时静默降级为相对描述（`updateDirLabel` 的兜底文案）——该说明条只是辅助信息，
 * 取不到绝对路径不应打断本页其他功能，故不弹报错提示。
 */
async function loadBasePath() {
  try {
    const info = await systemApi.info();
    basePath.value = info.base_path ?? "";
  } catch {
    basePath.value = "";
  }
}

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
  if (s.platform_unavailable) return `远程发布暂无当前平台的安装包（远程 v${s.latest_version}）`;
  return s.latest_version ? `当前已是最新（远程 v${s.latest_version}）` : "";
});
async function manualCheckUpdate() {
  updateChecking.value = true;
  updateInfo.value = null;
  try {
    const info = await systemApi.checkUpdate();
    updateInfo.value = info;
    // 命中新版本直接弹更新弹窗（完整 GitHub 发布说明在此展示）；
    //「已是最新 / 平台缺包」仍在本页就地提示，弹窗只用于真有更新可看可装时
    if (info?.has_update) update.openWith(info);
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
    await offerRestart();
  } catch (e: unknown) {
    updateInfo.value = { has_update: false, error: (e as Error).message || "更新失败" };
  } finally {
    updating.value = false;
  }
}

/**
 * 手动选择安装包：挑本地压缩包上传并暂存，成功后同样询问是否立即重启。
 *
 * 与「立即更新」的区别只在包的来源——服务端仍会校验版本并走同一套
 * staging → pending.json → helper 替换流程，故重启提示复用 offerRestart。
 */
async function selectUpdatePackage() {
  const file = await pickFile(".zip,.tar.gz,.tgz");
  if (!file) return;
  updating.value = true;
  updateInfo.value = null;
  try {
    const data = await systemApi.updateWithPackage(file);
    updateInfo.value = {
      has_update: false,
      message: (data.message as string) || "更新已就绪，重启后生效",
    };
    await offerRestart();
  } catch (e: unknown) {
    updateInfo.value = { has_update: false, error: (e as Error).message || "安装包处理失败" };
  } finally {
    updating.value = false;
  }
}

/** 更新暂存完成后的重启询问（两条更新路径共用） */
async function offerRestart() {
  const ok = await confirm({
    title: "更新已就绪",
    message: "更新已下载完成，是否立即重启应用以生效？",
    confirmText: "立即重启",
  });
  if (ok) {
    try {
      await systemApi.restart();
    } catch {
      if (updateInfo.value) updateInfo.value.message = "更新已就绪，但自动重启失败，请手动重启应用";
    }
  }
}

const autoRestartOptions = [
  { value: "0", label: "不启用" },
  { value: "6", label: "每 6 小时" },
  { value: "12", label: "每 12 小时" },
  { value: "24", label: "每 24 小时" },
  { value: "48", label: "每 48 小时" },
  { value: "168", label: "每 168 小时（每周）" },
];
const autoRestartHours = computed<string>({
  get: () => String(config.config.app_settings.auto_restart_hours ?? 0),
  set: (v) => { config.config.app_settings.auto_restart_hours = Number(v); },
});

const reloading = ref(false);
const reloadMsg = ref("");
async function reloadConfig() {
  if (config.dirty.value) {
    const ok = await confirm({
      title: "放弃未保存修改",
      message: "重新加载会用磁盘配置覆盖当前未保存的设置，是否继续？",
      confirmText: "放弃并重新加载",
      danger: true,
    });
    if (!ok) return;
  }
  reloading.value = true;
  reloadMsg.value = "";
  try {
    await configApi.reload();
    await config.fetchConfig();
    if (config.configLoadFailed.value) {
      reloadMsg.value = "后端已重新加载，但表单刷新失败，请重试";
      return;
    }
    reloadMsg.value = "配置已重新加载";
  } catch (e: unknown) {
    reloadMsg.value = "重新加载失败：" + ((e as Error).message || "未知错误");
  } finally { reloading.value = false; }
}

onMounted(() => {
  void refreshUpdateState();
  void loadBasePath();
});
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
          <input id="settings-app-port" v-model.number="config.config.app_settings.port" type="number" min="1" max="65535" />
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
      <div class="card-body">
        <!-- 手动放置安装包：跨两列的说明条（手动更新是低频操作，放折叠区避免占位） -->
        <details class="manual-update-tip">
          <summary>
            <IconApp name="info" width="15" height="15" />
            <span>下载慢？可把安装包放进 update/ 目录，更新时自动跳过下载</span>
            <IconApp name="chevron-down" class="manual-update-chevron" />
          </summary>
          <div class="manual-update-body">
            <p>
              从发布页下载对应平台的压缩包，放进下面的目录，再点「立即检查」——
              若包的内容与远程发布的一致，程序会复用它、不再联网下载。
            </p>
            <div class="manual-update-path">
              <code>{{ updateDirLabel }}</code>
              <button type="button" class="btn btn-ghost btn-sm" @click="copyUpdateDir">复制路径</button>
            </div>
            <ul class="manual-update-notes">
              <li>按 <strong>内容</strong>（SHA256）判断，改过文件名也能用；放着旧版本的包不会被误用。</li>
              <li>仍需能连上发布源：版本号与校验值来自远程，完全离线时请改用整包覆盖。</li>
              <li>该目录下的 <code>pending.json</code>、<code>staging/</code> 等是程序自己的文件，可与之共存。</li>
            </ul>
          </div>
        </details>
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
            <div class="segmented" role="group" aria-label="更新通道">
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
              <label>手动选择安装包</label>
              <FieldHelp text="从本地挑一个已下载好的发布包直接安装，不联网下载。适用于下载慢、或用自编译/镜像包的情况；版本须高于当前版本。" />
            </div>
            <div class="update-check-row">
              <button type="button" class="btn btn-secondary btn-sm" :disabled="updating" @click="selectUpdatePackage">
                <IconApp v-if="updating" name="refresh" class="spin" />
                {{ updating ? "处理中..." : "选择安装包" }}
              </button>
              <span class="hint">支持 .zip / .tar.gz / .tgz，上限 512 MB</span>
            </div>
          </div>
          <div class="form-group">
            <div class="field-label-row">
              <label>上次检查时间</label>
              <FieldHelp text="记录最近一次手动或自动检查的结果，重启后保留。" />
            </div>
            <div class="update-check-row">
              <button type="button" class="btn btn-secondary btn-sm" :disabled="updateChecking" @click="manualCheckUpdate">
                <IconApp v-if="updateChecking" name="refresh" class="spin" />
                {{ updateChecking ? "检查中..." : "立即检查" }}
              </button>
              <!-- 更新日志统一在弹窗里看（大区域 + 可滚动），本页只留紧凑状态行 -->
              <button type="button" class="btn btn-ghost btn-sm" @click="update.openDialog()">
                查看更新日志
              </button>
              <span v-if="lastCheckLabel" class="hint">{{ lastCheckLabel }}</span>
            </div>
            <span v-if="updateCheckHint && !updateInfo" class="hint update-check-hint" :class="{ 'update-check-error': !!updateState?.error }">{{ updateCheckHint }}</span>
            <!-- 检查结果：与关于页原逻辑一致，检查后原地展示更新/下载入口 -->
            <div v-if="updateInfo && !updateInfo.error && !updateInfo.message" class="update-result">
              <div v-if="updateInfo.has_update" class="update-available">
                <IconApp name="upload" width="16" height="16" />
                <span>发现新版本 <strong>v{{ updateInfo.latest }}</strong><template v-if="updateInfo.size">（约 {{ (updateInfo.size / 1048576).toFixed(1) }} MB）</template></span>
                <button type="button" class="btn btn-primary btn-sm" :disabled="updating" @click="applyUpdate">{{ updating ? "更新中..." : localPackageHint ? "使用本地包更新" : "立即更新" }}</button>
                <a v-if="updateInfo.url" :href="updateInfo.url" target="_blank" rel="noopener noreferrer" class="btn btn-ghost btn-sm">前往下载</a>
              </div>
              <div v-else class="update-latest">
                <IconApp name="check" width="16" height="16" />
                <span>{{ updateInfo.platform_unavailable ? "远程发布暂无当前平台的安装包" : "当前已是最新版本" }}</span>
              </div>
              <!-- 本地安装包命中：说明将跳过下载（应用阶段服务端会重新扫描校验） -->
              <p v-if="updateInfo.has_update && localPackageHint" class="hint update-local-package">
                <IconApp name="check" width="14" height="14" />
                {{ localPackageHint }}
              </p>
              <div v-if="updating && updateProgress" class="hint">下载更新 {{ updateProgress.percent }}%</div>
            </div>
            <div v-else-if="updateInfo && updateInfo.message" class="update-success">
              <IconApp name="check" width="16" height="16" />
              <span>{{ updateInfo.message }}，请重启程序生效</span>
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
          <button type="button" class="btn btn-secondary btn-sm" @click="reloadConfig" :disabled="reloading">
            {{ reloading ? "加载中..." : "重新加载配置" }}
          </button>
          <span v-if="reloadMsg" class="hint">{{ reloadMsg }}</span>
        </div>
      </div>
    </section>
  </div>
</template>
