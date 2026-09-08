<script setup lang="ts">
/**
 * 仓库导入双弹窗（导入列表 + 免责声明）。
 * 状态存于 useRepoImport 单例。此前 Modal 仅挂在 TasksView，
 * 导致设置·任务页点「从仓库导入」无反应、切到任务列表页才弹出的错位 bug，
 * 故提取为共享组件，两处入口（TasksView / TasksSettings）各自挂载。
 *
 * 列表为左右分栏：左侧任务条目（含 64px 缩略图），点选后右侧展示
 * 截图大图与完整信息；无 screenshot 定义的任务显示「暂无截图」占位。
 * 详情区大图可点击放大（三层弹窗：导入列表 > 放大预览，免责声明互斥）。
 * 缩略图/大图均经 /api/repo/image 同源代理（免鉴权 `<img>` 引用口径，
 * 出站限死任务站 raw 域），加载失败回退占位而非破图。
 */
import { computed, ref, watch } from "vue";
import Modal from "./common/Modal.vue";
import { repoApi } from "@/api";
import { useRepoImport } from "@/composables/useRepoImport";

const repo = useRepoImport();

/** 图片加载失败的任务 id 集合：缩略图/大图统一回退占位 */
const brokenImages = ref(new Set<string>());

/** 当前点选任务的截图代理地址（无定义时为空，由模板渲染占位） */
const selectedScreenshotUrl = computed(() => {
  const shot = repo.repoImport.value.selected?.screenshot?.trim();
  return shot ? repoApi.screenshotUrl(shot) : "";
});

/** 列表缩略图地址（无定义返回空字符串，模板渲染迷你占位） */
function thumbUrl(taskId: string, screenshot?: string): string {
  const shot = screenshot?.trim();
  if (!shot || brokenImages.value.has(taskId)) return "";
  return repoApi.screenshotUrl(shot);
}

/** 是否官方任务：作者为项目维护者 Misyra（任务站默认索引的维护者署名） */
function isOfficialTask(author?: string): boolean {
  return author?.trim().toLowerCase() === "misyra";
}

/** 标记某任务截图加载失败，缩略图与大图同步回退占位 */
function markImageBroken(taskId: string): void {
  brokenImages.value = new Set(brokenImages.value).add(taskId);
}

/** 放大预览的截图地址（空即关闭）：详情区大图点选后全尺寸查看 */
const previewShot = ref("");
const previewTitle = ref("");

/** 打开截图放大预览 */
function openShotPreview(url: string, taskName: string): void {
  previewShot.value = url;
  previewTitle.value = `${taskName} · 登录页截图`;
}

/** 关闭截图放大预览 */
function closeShotPreview(): void {
  previewShot.value = "";
  previewTitle.value = "";
}

// 主弹窗关闭时放大预览一起关，避免预览窗单独悬空
watch(
  () => repo.repoImport.value.visible,
  (visible) => {
    if (!visible) closeShotPreview();
  },
);
watch(
  () => repo.repoImport.value.tasks,
  () => {
    // 切换索引/重新加载后旧失败标记失效，清空避免误占位
    brokenImages.value = new Set<string>();
    // 列表刷新后旧点选可能已不在结果中，清空详情避免展示过期任务
    const selected = repo.repoImport.value.selected;
    if (selected && !repo.repoImport.value.tasks.some((t) => t.id === selected.id)) {
      repo.repoImport.value.selected = null;
    }
  },
);
</script>

<template>
  <Modal :open="repo.repoImport.value.visible" title="从云端仓库导入任务" size="xl" @close="repo.closeRepoImport">
    <div class="repo-import-source">
      <span class="repo-source-label">源：</span>
      <button class="btn btn-sm" :class="{ active: repo.repoImport.value.source === 'github' }" @click="repo.selectRepoSource('github')">GitHub</button>
      <button class="btn btn-sm" :class="{ active: repo.repoImport.value.source === 'gitee' }" @click="repo.selectRepoSource('gitee')">Gitee</button>
      <button class="btn btn-sm" :class="{ active: repo.repoImport.value.source === 'custom' }" @click="repo.selectRepoSource('custom')">自定义</button>
      <div v-if="repo.repoImport.value.source === 'custom'" class="repo-custom-url">
        <div class="form-group form-group--flush"><input v-model="repo.repoImport.value.url" type="text" placeholder="输入远程索引 URL" /></div>
      </div>
    </div>
    <div class="repo-import-action">
      <button class="btn btn-primary btn-sm" @click="repo.fetchRepoIndex()" :disabled="repo.repoImport.value.loading">
        {{ repo.repoImport.value.loading ? '加载中...' : '加载索引' }}
      </button>
    </div>
    <div v-if="repo.repoImport.value.error" class="repo-import-error">{{ repo.repoImport.value.error }}</div>
    <div v-if="repo.repoImport.value.tasks.length > 0" class="repo-import-search">
      <div class="form-group form-group--flush"><input v-model="repo.repoImport.value.searchQuery" type="text" placeholder="搜索任务..." /></div>
    </div>
    <div v-if="repo.repoImport.value.tasks.length > 0" class="repo-import-body">
      <div class="repo-import-list">
        <div
          v-for="task in repo.filteredRepoTasks.value"
          :key="task.id || task.name"
          class="repo-import-item"
          :class="{ selected: repo.repoImport.value.selected?.id === task.id }"
          @click="repo.selectRepoTask(task)"
        >
          <div v-if="thumbUrl(task.id, task.screenshot)" class="repo-item-thumb">
            <img :src="thumbUrl(task.id, task.screenshot)" :alt="`${task.name} 登录页截图`" loading="lazy" @error="markImageBroken(task.id)" />
          </div>
          <div v-else class="repo-item-thumb repo-item-thumb-empty">暂无截图</div>
          <div class="repo-item-text">
            <div class="repo-item-name">
              {{ task.name }}
              <span v-if="isOfficialTask(task.author)" class="badge badge--sm badge--success repo-item-official">官方任务</span>
            </div>
            <div class="repo-item-desc">{{ task.description }}</div>
            <div class="repo-item-meta">
              <span v-if="task.author" class="repo-item-author">{{ task.author }}</span>
              <span v-if="task.tags" class="repo-item-tags">{{ task.tags.join(', ') }}</span>
            </div>
          </div>
        </div>
        <div v-if="repo.filteredRepoTasks.value.length === 0" class="repo-import-empty">无匹配</div>
      </div>
      <div class="repo-import-detail">
        <template v-if="repo.repoImport.value.selected">
          <h4 class="repo-detail-name">
            {{ repo.repoImport.value.selected.name }}
            <span v-if="isOfficialTask(repo.repoImport.value.selected.author)" class="badge badge--sm badge--success repo-item-official">官方任务</span>
          </h4>
          <p class="repo-detail-desc">{{ repo.repoImport.value.selected.description }}</p>
          <div v-if="selectedScreenshotUrl && !brokenImages.has(repo.repoImport.value.selected.id)" class="repo-detail-shot">
            <img
              :src="selectedScreenshotUrl"
              :alt="`${repo.repoImport.value.selected.name} 登录页截图`"
              loading="lazy"
              class="repo-detail-shot-img"
              title="点击放大查看"
              @click="openShotPreview(selectedScreenshotUrl, repo.repoImport.value.selected.name)"
              @error="markImageBroken(repo.repoImport.value.selected.id)"
            />
            <span class="repo-detail-zoom-hint">点击放大</span>
          </div>
          <div v-else class="repo-detail-empty">暂无截图</div>
        </template>
        <div v-else class="repo-detail-empty">点击左侧任务查看登录页截图</div>
      </div>
    </div>
    <div v-else-if="!repo.repoImport.value.loading" class="repo-import-hint">
      <p>点击「加载索引」从远程仓库获取任务列表。</p>
      <p>你也可以 <a :href="repo.repoImport.value.url" target="_blank" rel="noopener">直接查看仓库</a>。</p>
    </div>
    <template #footer>
      <button v-if="repo.repoImport.value.selected" class="btn btn-primary btn-sm" @click="repo.confirmRepoImport(repo.repoImport.value.selected!)">导入此任务</button>
      <span v-else class="repo-footer-hint">点击左侧任务查看详情后导入</span>
    </template>
  </Modal>

  <!-- 免责弹窗：必须显式确认/取消，禁用遮罩与 ESC 关闭 -->
  <Modal :open="!!repo.repoImport.value.disclaimer" title="免责声明" :close-on-overlay="false" :close-on-esc="false" @close="repo.cancelRepoDisclaimer">
    <p>从远程仓库导入的任务由社区成员提供，未经审核验证。</p>
    <p class="repo-disclaimer-warn"><strong>请仔细阅读并确认任务内容后再使用。</strong>任务中填入的账号密码将在执行时提交到第三方网站，请确认目标网站可靠。</p>
    <template #footer>
      <button class="btn btn-secondary" @click="repo.cancelRepoDisclaimer()">取消</button>
      <button class="btn btn-primary" @click="repo.acceptRepoDisclaimer()">确认导入</button>
    </template>
  </Modal>

  <!-- 截图放大预览：复用 Modal，大图可滚动查看原图（沉浸预览加深遮罩） -->
  <Modal :open="!!previewShot" :title="previewTitle" size="xl" preview @close="closeShotPreview">
    <div class="repo-shot-preview">
      <img :src="previewShot" :alt="previewTitle" />
    </div>
  </Modal>
</template>

<style scoped>
.repo-import-source { display: flex; gap: 8px; align-items: center; margin-bottom: 12px; flex-wrap: wrap; }
.repo-source-label { font-size: var(--text-md); color: var(--text-secondary); }
.repo-import-source .btn.active { background: var(--accent); color: var(--on-accent); }
.repo-custom-url { width: 100%; margin-top: 8px; }
.repo-import-action { margin-bottom: 12px; }
.repo-import-error { color: var(--error); font-size: var(--text-md); margin-bottom: 8px; }
.repo-import-search { margin-bottom: 12px; }
.repo-import-body { display: flex; gap: 12px; min-height: 0; }
.repo-import-list { flex: 1; min-width: 0; overflow-y: auto; display: flex; flex-direction: column; gap: 8px; max-height: 56vh; }
.repo-import-item { display: flex; gap: 10px; align-items: flex-start; padding: 10px 12px; border: 1px solid var(--border); border-radius: var(--radius-md); cursor: pointer; transition: background var(--dur-fast) var(--ease-out); }
.repo-import-item:hover { background: var(--bg-hover); }
/* 选中态对齐 .task-item.active（淡底 + 边框），不用实底 accent */
.repo-import-item.selected { border-color: var(--accent); background: rgba(var(--accent-rgb), 0.06); }
.repo-item-thumb { flex: none; width: 64px; height: 64px; border-radius: var(--radius-sm); overflow: hidden; background: var(--bg-hover); }
.repo-item-thumb img { width: 100%; height: 100%; object-fit: cover; display: block; }
.repo-item-thumb-empty { display: flex; align-items: center; justify-content: center; font-size: var(--text-xs); color: var(--text-tertiary); text-align: center; padding: 4px; }
.repo-item-text { flex: 1; min-width: 0; }
.repo-item-name { font-weight: 600; }
/* 官方任务徽标：语义走通用 .badge--success，此处只留列表内的左侧间距 */
.repo-item-official { margin-left: 6px; vertical-align: middle; }
.repo-item-desc { font-size: var(--text-sm); color: var(--text-secondary); margin-top: 2px; }
.repo-item-meta { display: flex; gap: 12px; margin-top: 6px; font-size: var(--text-xs); color: var(--text-tertiary); }
.repo-import-detail { flex: 1; min-width: 0; border: 1px solid var(--border); border-radius: var(--radius-md); padding: 12px; display: flex; flex-direction: column; gap: 8px; max-height: 56vh; overflow-y: auto; }
.repo-detail-name { font-size: var(--text-md); font-weight: 600; }
.repo-detail-desc { font-size: var(--text-sm); color: var(--text-secondary); }
.repo-detail-shot { position: relative; border-radius: var(--radius-sm); overflow: hidden; border: 1px solid var(--border); }
.repo-detail-shot img { width: 100%; display: block; }
.repo-detail-shot-img { cursor: zoom-in; }
.repo-detail-zoom-hint { position: absolute; right: 8px; bottom: 8px; padding: 2px 8px; border-radius: var(--radius-xs); font-size: var(--text-xs); color: var(--text-secondary); background: var(--bg-modal); border: 1px solid var(--border); pointer-events: none; }
.repo-shot-preview { max-height: 70vh; overflow: auto; border-radius: var(--radius-sm); border: 1px solid var(--border); }
.repo-shot-preview img { width: 100%; display: block; }
.repo-detail-empty { flex: 1; display: flex; align-items: center; justify-content: center; min-height: 120px; color: var(--text-tertiary); font-size: var(--text-sm); background: var(--bg-hover); border-radius: var(--radius-sm); }
/* 列表弹窗 footer 为选中任务的操作区：有选中=导入按钮，无选中=引导文案 */
.repo-footer-hint { color: var(--text-tertiary); font-size: var(--text-sm); }
.repo-import-empty { text-align: center; color: var(--text-tertiary); padding: 24px; }
.repo-import-hint { color: var(--text-secondary); font-size: var(--text-md); }
.repo-import-hint a { color: var(--accent); }
.repo-disclaimer-warn { color: var(--error); }
</style>
