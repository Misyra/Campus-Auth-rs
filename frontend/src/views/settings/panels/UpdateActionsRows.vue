<script setup lang="ts">
/**
 * 设置 · 自动更新面板的检查/安装动作行（schema 槽位）：手动选包、立即检查与检查结果就地展示。
 *
 * 检查结果有发现更新 / 已最新 / 成功暂存 / 失败四类就地分支，超出标准行形态。
 */
import SettingsRow from "@/components/common/SettingsRow.vue";
import IconApp from "@/components/common/IconApp.vue";
import { computed, onMounted, ref } from "vue";
import { useStatus } from "@/composables/useStatus";
import { useConfirm } from "@/composables/useConfirm";
import { useUpdateDialog } from "@/composables/useUpdateDialog";
import { IS_OPENWRT } from "@/utils/platform";
import { pickFile } from "@/utils/file";
import { formatLocalDateTime } from "@/utils/formatters";
import { systemApi } from "@/api";
import type { UpdateState, UpdateInfo } from "@/api/types";

const { status } = useStatus();
const { confirm } = useConfirm();
/** 更新弹窗：本分区的「立即检查」命中与「查看更新日志」入口都指向它 */
const update = useUpdateDialog();

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

async function refreshUpdateState() {
  try { updateState.value = await systemApi.updateState(); } catch { updateState.value = null; }
}
const lastCheckLabel = computed(() => {
  const s = updateState.value;
  if (!s?.last_check_at) return "从未检查";
  return formatLocalDateTime(s.last_check_at);
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

onMounted(() => { void refreshUpdateState(); });
</script>

<template>
  <div>
    <SettingsRow
      v-if="!IS_OPENWRT"
      help="从本地挑一个已下载好的发布包直接安装，不联网下载。适用于下载慢、或用自编译/镜像包的情况；版本须高于当前版本。"
    >
      <template #label>
        <!-- 行标题：控件是自带完整文本的动作按钮「选择安装包」，名称与标题
             不同，强行 for 关联会改写按钮可访问名（误报豁免） -->
        <!-- eslint-disable-next-line vuejs-accessibility/label-has-for -->
        <label>手动选择安装包</label>
      </template>
      <div class="update-check-row">
        <button
          type="button"
          class="btn btn-secondary btn-sm"
          :disabled="updating"
          @click="selectUpdatePackage"
        >
          <IconApp v-if="updating" name="refresh" class="spin" />
          {{ updating ? "处理中..." : "选择安装包" }}
        </button>
        <span class="hint">支持 .zip / .tar.gz / .tgz，上限 512 MB</span>
      </div>
    </SettingsRow>
    <!-- 说明收进 ? 气泡：description 为最近一次检查的动态结果（随检查状态更新） -->
    <SettingsRow
      help="记录最近一次手动或自动检查的结果，重启后保留。"
      :description="updateCheckHint || undefined"
    >
      <template #label>
        <!-- 行标题：控件槽是检查时间/状态文本行，无可标记的表单控件（误报豁免） -->
        <!-- eslint-disable-next-line vuejs-accessibility/label-has-for -->
        <label>上次检查时间</label>
      </template>
      <div class="update-check-row">
        <button
          type="button"
          class="btn btn-secondary btn-sm"
          :disabled="updateChecking"
          @click="manualCheckUpdate"
        >
          <IconApp v-if="updateChecking" name="refresh" class="spin" />
          {{ updateChecking ? "检查中..." : "立即检查" }}
        </button>
        <!-- 更新日志统一在弹窗里看（大区域 + 可滚动），本页只留紧凑状态行 -->
        <button type="button" class="btn btn-ghost btn-sm" @click="update.openDialog()">查看更新日志</button>
        <span v-if="lastCheckLabel" class="hint">{{ lastCheckLabel }}</span>
      </div>
      <!-- 检查结果：与关于页原逻辑一致，检查后原地展示更新/下载入口 -->
      <p v-if="IS_OPENWRT" class="hint">OpenWrt 版请通过 SSH 停止 campus-auth 服务，替换程序后再启动；开机启动和进程重启由 procd 管理。</p>
      <div v-if="updateInfo && !updateInfo.error && !updateInfo.message" class="update-result">
        <div v-if="updateInfo.has_update" class="update-available">
          <IconApp name="upload" width="16" height="16" />
          <span>
            发现新版本
            <strong>v{{ updateInfo.latest }}</strong>
            <template v-if="updateInfo.size">（约 {{ (updateInfo.size / 1048576).toFixed(1) }} MB）</template>
          </span>
          <button v-if="!IS_OPENWRT" type="button" class="btn btn-primary btn-sm" :disabled="updating" @click="applyUpdate">
            {{ updating ? "更新中..." : localPackageHint ? "使用本地包更新" : "立即更新" }}
          </button>
          <a
            v-if="updateInfo.url"
            :href="updateInfo.url"
            target="_blank"
            rel="noopener noreferrer"
            class="btn btn-ghost btn-sm"
          >
            前往下载
          </a>
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
    </SettingsRow>
  </div>
</template>
