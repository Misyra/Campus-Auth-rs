<script setup lang="ts">
/**
 * 更新弹窗（全局挂载于 App.vue）。
 *
 * 取代设置页里那个 8em 高的更新日志小框：检查到新版本时直接弹出，正文按 GitHub
 * Release 的 Markdown 渲染在可滚动的大区域里。
 *
 * 关闭方式：右上角按钮 / ESC / 点击遮罩（弹窗以外的任意区域）——均为 Modal 默认行为。
 */
import { computed } from "vue";

import IconApp from "@/components/common/IconApp.vue";
import Modal from "@/components/common/Modal.vue";
import { useUpdateDialog } from "@/composables/useUpdateDialog";
import { releaseTagUrl } from "@/utils/constants";
import { renderReleaseNotes, stripTemplateTitle } from "@/utils/releaseNotes";

const update = useUpdateDialog();
/** 顶层 ref 绑定：模板里自动解包，直接写 `progress?.percent` */
const progress = update.progress;

const info = computed(() => update.state.info);

const title = computed(() => {
  if (!info.value) return "检查更新";
  if (info.value.message) return "更新已就绪";
  // 无更新时不拿"已是最新版本"当标题：这句话是状态说明，不是"有事发生"的通知，
  // 当成标题会读成"没更新还专门弹个窗"；这里只可能是用户自己点了「查看更新日志」
  return info.value.has_update ? "发现新版本" : "更新日志";
});

/** 版本行文案：有更新时是"最新 vs 当前"，无更新时只留"当前版本"，避免两处同值 */
const versionLabel = computed(() => (info.value?.has_update ? "最新版本" : "当前版本"));
const versionValue = computed(() => {
  if (!info.value) return "";
  return (info.value.has_update ? info.value.latest : info.value.current) ?? "";
});

/** 更新日志正文；渲染器已转义全部文本并限制链接协议，故可安全 v-html */
const notesHtml = computed(() => renderReleaseNotes(stripTemplateTitle(info.value?.notes)));
const releaseUrl = computed(() => releaseTagUrl(info.value?.latest));

/**
 * 发布日期。
 *
 * 按 UTC 显示：GitHub 的 `published_at` 是 UTC，发布说明标题里的日期也来自发布流程的
 * UTC 日期；用本地时区在 UTC+8 下会显示成次日（说明里写 09-20、这里写 9/21），
 * 两处对不上反而像是数据错。
 */
const releaseDateLabel = computed(() => {
  const raw = info.value?.release_date;
  if (!raw) return "";
  const date = new Date(raw);
  return Number.isNaN(date.getTime()) ? "" : date.toLocaleDateString("zh-CN", { timeZone: "UTC" });
});

const sizeLabel = computed(() => {
  const size = info.value?.size;
  return typeof size === "number" && size > 0 ? `约 ${(size / 1048576).toFixed(1)} MB` : "";
});

/** 本地安装包命中提示（后端在检查阶段比对 update/ 目录文件与远程声明的 SHA256） */
const localPackageHint = computed(() => {
  const pkg = info.value?.local_package;
  if (!pkg) return "";
  return `已在 update/ 目录找到匹配的安装包 ${pkg.file_name}，更新时将跳过下载`;
});

const applyLabel = computed(() => {
  if (update.state.applying) return "更新中…";
  return localPackageHint.value ? "使用本地包更新" : "立即更新";
});
</script>

<template>
  <Modal :open="update.state.open" :title="title" size="lg" @close="update.dismiss()">
    <div class="update-dialog">
      <div v-if="update.state.loading && !info" class="update-dialog-loading">
        <IconApp name="refresh" class="spin" width="16" height="16" />
        <span>正在检查更新…</span>
      </div>

      <template v-else-if="info">
        <div class="update-dialog-head">
          <div class="update-dialog-versions">
            <span class="update-dialog-version-label">{{ versionLabel }}</span>
            <span class="update-dialog-version-value">v{{ versionValue || "—" }}</span>
            <span v-if="info.has_update && info.current" class="update-dialog-version-current">
              当前 v{{ info.current }}
            </span>
          </div>
          <div v-if="releaseDateLabel || sizeLabel" class="update-dialog-meta">
            <span v-if="releaseDateLabel">发布于 {{ releaseDateLabel }}</span>
            <span v-if="sizeLabel">{{ sizeLabel }}</span>
          </div>
        </div>

        <p v-if="localPackageHint" class="update-dialog-hint">{{ localPackageHint }}</p>

        <div v-if="info.message" class="update-dialog-state update-dialog-state--ready">
          <IconApp name="check" width="16" height="16" />
          <span>{{ info.message }}，请重启程序生效</span>
        </div>
        <!-- 平台缺包是唯一需要在"无更新"时额外说明的情况：标题与版本行已表达"已是最新"，
             这里不再重复一句"当前已是最新版本" -->
        <div v-else-if="info.platform_unavailable" class="update-dialog-state">
          <IconApp name="info" width="16" height="16" />
          <span>远程发布暂无当前平台的安装包</span>
        </div>

        <!-- 更新日志：有更新时是"待装版本的说明"，已是最新时是当期发布的说明。
             后者让「查看更新日志」入口在平时也有意义，不必非得等到有更新才看得到 -->
        <template v-if="notesHtml">
          <div class="update-dialog-notes-head">
            <span class="update-dialog-notes-title">
              <IconApp name="list" width="14" height="14" />
              更新日志
            </span>
            <a :href="releaseUrl" target="_blank" rel="noopener noreferrer">
              在 GitHub 查看
              <span aria-hidden="true">→</span>
            </a>
          </div>
          <div class="update-dialog-notes" v-html="notesHtml"></div>
        </template>
        <p v-else-if="!info.message" class="update-dialog-empty">本次发布没有填写更新说明。</p>

        <div v-if="update.state.applying" class="update-dialog-progress">
          <div class="update-dialog-progress-bar">
            <span :style="{ width: `${progress?.percent ?? 0}%` }"></span>
          </div>
          <span class="hint">
            {{ progress ? `下载更新 ${progress.percent}%` : "正在准备下载…" }}
          </span>
        </div>
      </template>

      <p v-if="update.state.error" class="update-dialog-error">{{ update.state.error }}</p>
    </div>

    <template #footer>
      <button
        v-if="update.state.error && !info?.has_update"
        type="button"
        class="btn btn-secondary"
        @click="update.refresh()"
      >
        重试
      </button>
      <template v-if="info?.has_update">
        <button type="button" class="btn btn-ghost" @click="update.snooze()">稍后提醒</button>
        <a
          v-if="info.url"
          :href="info.url"
          target="_blank"
          rel="noopener noreferrer"
          class="btn btn-ghost"
        >
          前往下载
        </a>
        <button
          type="button"
          class="btn btn-primary"
          :disabled="update.state.applying"
          @click="update.applyUpdate()"
        >
          {{ applyLabel }}
        </button>
      </template>
      <button v-else type="button" class="btn btn-secondary" @click="update.dismiss()">关闭</button>
    </template>
  </Modal>
</template>

