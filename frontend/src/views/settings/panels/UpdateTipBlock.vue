<script setup lang="ts">
/** 设置 · 自动更新面板的本地包提示块（schema 槽位）：update/ 目录复用说明，默认折叠。 */
import IconApp from "@/components/common/IconApp.vue";
import { computed, onMounted, ref } from "vue";
import { useToast } from "@/composables/useToast";
import { systemApi } from "@/api";

const { toastOnly } = useToast();

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

onMounted(() => { void loadBasePath(); });
</script>

<template>
  <div>
    <!-- 手动放置安装包的说明默认折叠，按需查看。 -->
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
          <li>
            按
            <strong>内容</strong>
            （SHA256）判断，改过文件名也能用；放着旧版本的包不会被误用。
          </li>
          <li>仍需能连上发布源：版本号与校验值来自远程，完全离线时请改用整包覆盖。</li>
          <li>
            该目录下的
            <code>pending.json</code>
            、
            <code>staging/</code>
            等是程序自己的文件，可与之共存。
          </li>
        </ul>
      </div>
    </details>
  </div>
</template>
