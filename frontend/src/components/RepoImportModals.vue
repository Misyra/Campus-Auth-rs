<script setup lang="ts">
/**
 * 仓库导入双弹窗（导入列表 + 免责声明）。
 * 状态存于 useRepoImport 单例。此前 Modal 仅挂在 TasksView，
 * 导致设置·任务页点「从仓库导入」无反应、切到任务列表页才弹出的错位 bug，
 * 故提取为共享组件，两处入口（TasksView / 任务与环境设置页）各自挂载。
 *
 * 预设索引按类别选取，仍按 `repoKind` 校验条目；脚本正文在确认前完整展示。
 *
 * 列表为左右分栏：有截图时显示缩略图，否则以登录类别图标识别条目；
 * 右侧优先展示类别、作者、标签与仓库来源，仅有截图时才展示大图。
 * 详情区大图可点击放大（三层弹窗：导入列表 > 放大预览，免责声明互斥）。
 * 缩略图/大图均经 /api/repo/image 同源代理（免鉴权 `<img>` 引用口径，
 * 出站限死任务站 raw 域），加载失败时回退类别图标或隐藏大图。
 *
 * 来源选择器与「刷新索引」同处一行工具条：此前操作按钮独占一行、
 * 与它作用的来源选择器被隔开，看不出点它会用哪个源。
 */
import { computed, nextTick, ref, watch } from "vue";
import Modal from "./common/Modal.vue";
import IconApp from "./common/IconApp.vue";
import { repoApi } from "@/api";
import type { RepoTask } from "@/api/types";
import { TASK_REPO_SOURCES } from "@/utils/constants";
import { repoSourceLabel, repoSourceUrl } from "@/utils/repoSource";
import { useRepoImport, repoKindLabel, type RepoKind } from "@/composables/useRepoImport";
import type { IconName } from "./common/IconApp.vue";

const repo = useRepoImport();

/** 源选项（模板直接遍历，避免在模板里硬编码按钮——增删源只改 constants） */
const sourceOptions = TASK_REPO_SOURCES;

/** 当前浏览的条目类型（决定筛选、标题与导入去向） */
const kind = computed(() => repo.repoImport.value.repoKind);

/** 当前类别的中文名（标题与空态共用，见 useRepoImport） */
const kindLabel = computed(() => repoKindLabel(kind.value));

/** 弹窗标题说清当前筛选的是哪一类 */
const modalTitle = computed(() => `从云端仓库导入${kindLabel.value}`);

/** 图片加载失败的任务 id 集合：缩略图回退类别图标，详情省去截图 */
const brokenImages = ref(new Set<string>());

/** 当前点选任务的截图代理地址（无定义时为空） */
const selectedScreenshotUrl = computed(() => {
  const shot = repo.repoImport.value.selected?.screenshot?.trim();
  return shot ? repoApi.screenshotUrl(shot) : "";
});

/** 列表缩略图地址（无定义返回空字符串，模板渲染类别图标） */
function thumbUrl(taskId: string, screenshot?: string): string {
  const shot = screenshot?.trim();
  if (!shot || brokenImages.value.has(taskId)) return "";
  return repoApi.screenshotUrl(shot);
}

/** 是否官方任务：作者为项目维护者 Misyra（任务站默认索引的维护者署名） */
function isOfficialTask(author?: string): boolean {
  return author?.trim().toLowerCase() === "misyra";
}

/** 来源仓库链接（改编自他人脚本时在索引里标注；只放行 http(s)，见 repoSource.ts） */
const sourceUrl = computed(() => repoSourceUrl(repo.repoImport.value.selected?.source));
/** 来源链接文字（去掉协议与 www.，否则半行都是前缀） */
const sourceLabel = computed(() => repoSourceLabel(sourceUrl.value));
/** 任务文件本身来自远端索引，链接也必须只放行 http(s) */
const taskFileUrl = computed(() => repoSourceUrl(repo.repoImport.value.selected?.url));

/** 列表和详情共用同一套类别名称、图标和说明 */
const kindDisplay: Record<RepoKind, { short: string; icon: IconName; description: string }> = {
  browser: { short: "浏览器登录", icon: "window-cursor", description: "通过浏览器操作登录页面" },
  http: { short: "HTTP 直连", icon: "send", description: "直接向认证接口发送请求" },
  script: { short: "脚本登录", icon: "terminal", description: "运行自定义脚本完成登录" },
};
const currentKindDisplay = computed(() => kindDisplay[kind.value]);
const detailRef = ref<HTMLElement | null>(null);

/** 手机端列表在详情上方，点选后将详情带入可视区 */
async function selectTask(task: RepoTask): Promise<void> {
  repo.selectRepoTask(task);
  if (!window.matchMedia("(max-width: 768px)").matches) return;
  await nextTick();
  detailRef.value?.scrollIntoView({
    behavior: window.matchMedia("(prefers-reduced-motion: reduce)").matches ? "auto" : "smooth",
    block: "start",
  });
}

/** 标记某任务截图加载失败，缩略图回退类别图标且详情省去截图 */
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
  <Modal :open="repo.repoImport.value.visible" :title="modalTitle" size="xl" @close="repo.closeRepoImport">
    <!-- 打开与切源自动加载；刷新按钮绕过两小时缓存 -->
    <div class="repo-import-toolbar">
      <div class="repo-import-field">
        <span class="repo-source-label">来源</span>
        <div class="segmented" role="group" aria-label="任务来源">
          <button
            v-for="opt in sourceOptions"
            :key="opt.id"
            type="button"
            :class="{ active: repo.repoImport.value.source === opt.id }"
            @click="repo.selectRepoSource(opt.id)"
          >
            {{ opt.label }}
          </button>
        </div>
      </div>
      <button class="btn btn-primary btn-sm" @click="repo.fetchRepoIndex()" :disabled="repo.repoImport.value.loading">
        <IconApp name="refresh" class="icon-sm" :class="{ spin: repo.repoImport.value.loading }" />
        {{ repo.repoImport.value.loading ? '刷新中...' : '刷新索引' }}
      </button>
    </div>
    <p v-if="repo.repoImport.value.loaded" class="repo-cache-hint">
      {{ repo.repoImport.value.loading ? '正在刷新，显示上次索引' : repo.repoImport.value.error ? '刷新失败，显示上次索引' : repo.repoImport.value.fromCache ? '已读取本地缓存' : '索引已更新' }} · 缓存有效期 2 小时
    </p>
    <!-- 来源说明：「国内用户建议用 Gitee」的提示由 TASK_REPO_SOURCES 的 hint 承载，
         自定义源无 hint 故整行不渲染 -->
    <p v-if="repo.currentSource.value.hint" class="repo-source-hint">{{ repo.currentSource.value.hint }}</p>
    <!-- 自定义索引可能混合类别；未知类型才需要提示，已知的其他类别按当前页筛掉。 -->
    <p v-if="repo.foreignRepoTaskCount.value > 0" class="repo-source-hint repo-foreign-hint">
      索引里有 {{ repo.foreignRepoTaskCount.value }} 个未知类型条目，已跳过。
    </p>

    <div v-if="repo.repoImport.value.source === 'custom'" class="repo-custom-url">
      <div class="form-group form-group--flush"><input v-model="repo.repoImport.value.url" type="url" placeholder="输入远程索引 URL，按回车加载" @keydown.enter="repo.fetchRepoIndex()" /></div>
    </div>

    <div v-if="repo.repoImport.value.error" class="repo-import-error">{{ repo.repoImport.value.error }}</div>
    <div v-if="repo.repoImport.value.tasks.length > 0" class="repo-import-search">
      <div class="form-group form-group--flush"><input v-model="repo.repoImport.value.searchQuery" type="text" placeholder="搜索任务..." /></div>
    </div>
    <div v-if="repo.repoImport.value.tasks.length > 0" class="repo-import-body">
      <div class="repo-import-list">
        <button
          v-for="task in repo.filteredRepoTasks.value"
          :key="task.id || task.name"
          type="button"
          class="repo-import-item"
          :class="{ selected: repo.repoImport.value.selected?.id === task.id }"
          :aria-pressed="repo.repoImport.value.selected?.id === task.id"
          @click="selectTask(task)"
        >
          <div v-if="thumbUrl(task.id, task.screenshot)" class="repo-item-thumb">
            <img :src="thumbUrl(task.id, task.screenshot)" :alt="`${task.name} 登录页截图`" loading="lazy" @error="markImageBroken(task.id)" />
          </div>
          <div v-else class="repo-item-thumb repo-item-kind-icon" aria-hidden="true">
            <IconApp :name="currentKindDisplay.icon" />
          </div>
          <div class="repo-item-text">
            <div class="repo-item-heading">
              <span class="repo-item-name">{{ task.name }}</span>
              <span v-if="isOfficialTask(task.author)" class="badge badge--sm badge--success">官方任务</span>
            </div>
            <div v-if="task.description" class="repo-item-desc">{{ task.description }}</div>
            <div class="repo-item-meta">
              <span class="repo-item-kind">{{ currentKindDisplay.short }}</span>
              <span v-if="task.author">{{ task.author }}</span>
              <span v-if="task.tags?.length" class="repo-item-tags">{{ task.tags.slice(0, 2).join(' / ') }}<template v-if="task.tags.length > 2"> +{{ task.tags.length - 2 }}</template></span>
            </div>
          </div>
        </button>
        <div v-if="repo.filteredRepoTasks.value.length === 0" class="repo-import-empty">
          {{ repo.repoImport.value.searchQuery.trim() ? "无匹配" : `该来源暂无${kindLabel}条目` }}
        </div>
      </div>
      <div ref="detailRef" class="repo-import-detail">
        <template v-if="repo.repoImport.value.selected">
          <h4 class="repo-detail-name">
            {{ repo.repoImport.value.selected.name }}
            <span v-if="isOfficialTask(repo.repoImport.value.selected.author)" class="badge badge--sm badge--success repo-item-official">官方任务</span>
          </h4>
          <p v-if="repo.repoImport.value.selected.description" class="repo-detail-desc">{{ repo.repoImport.value.selected.description }}</p>
          <div class="repo-detail-kind">
            <span class="repo-detail-kind-icon" aria-hidden="true"><IconApp :name="currentKindDisplay.icon" /></span>
            <span><strong>{{ currentKindDisplay.short }}</strong><small>{{ currentKindDisplay.description }}</small></span>
          </div>
          <dl v-if="repo.repoImport.value.selected.author || repo.repoImport.value.selected.version" class="repo-detail-facts">
            <div v-if="repo.repoImport.value.selected.author"><dt>作者</dt><dd>{{ repo.repoImport.value.selected.author }}</dd></div>
            <div v-if="repo.repoImport.value.selected.version"><dt>版本</dt><dd>{{ repo.repoImport.value.selected.version }}</dd></div>
          </dl>
          <div v-if="repo.repoImport.value.selected.tags?.length" class="repo-detail-tags">
            <span v-for="tag in repo.repoImport.value.selected.tags" :key="tag" class="repo-detail-tag">{{ tag }}</span>
          </div>
          <!-- 来源仓库：任务改编自他人脚本时标出处，可点开看原始实现。地址来自远端
               索引，故经 repoSourceUrl 只放行 http(s)（见 utils/repoSource.ts） -->
          <div v-if="sourceUrl || taskFileUrl" class="repo-detail-links">
            <a v-if="sourceUrl" class="repo-detail-source" :href="sourceUrl" target="_blank" rel="noopener" title="查看任务参考的原项目">
              <IconApp name="external-link" class="icon-sm" /><span>原项目：{{ sourceLabel }}</span>
            </a>
            <a v-if="taskFileUrl" class="repo-detail-source" :href="taskFileUrl" target="_blank" rel="noopener" title="查看仓库中的任务定义文件">
              <IconApp name="file-text" class="icon-sm" /><span>查看任务文件</span>
            </a>
          </div>
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
        </template>
        <div v-else class="repo-detail-empty">选择左侧任务，查看登录方式与来源</div>
      </div>
    </div>
    <div v-else-if="repo.repoImport.value.loading" class="empty-state empty-state--dashed repo-import-hint">
      <IconApp name="refresh" class="spin" :stroke-width="1.5" />
      <strong class="empty-title">正在获取任务索引</strong>
      <span class="empty-desc">读取当前来源的可导入任务…</span>
    </div>
    <div v-else class="empty-state empty-state--dashed repo-import-hint">
      <IconApp name="globe-grid" :stroke-width="1.5" />
      <!-- 「还没点加载」与「加载成功但这一类没有条目」是两件事。 -->
      <strong class="empty-title">{{ repo.repoImport.value.loaded ? `该来源暂无${kindLabel}条目` : repo.repoImport.value.source === 'custom' && !repo.repoImport.value.url.trim() ? '输入自定义索引地址' : '任务索引尚未加载' }}</strong>
      <span v-if="repo.repoImport.value.loaded" class="empty-desc">
        索引已读取，但里面没有{{ kindLabel }}。可到其他任务子页查看，或换个来源再试。
      </span>
      <span v-else class="empty-desc">{{ repo.repoImport.value.source === 'custom' && !repo.repoImport.value.url.trim() ? '填写地址后点击「刷新索引」获取任务。' : '可点击「刷新索引」重试，或切换来源。' }}</span>
      <!-- 指向仓库**主页**而非用户手填的索引地址：索引地址是给程序 GET 的 raw JSON，
           此前把它当作可读页面链接，点开是一屏 JSON 而不是仓库首页 -->
      <div class="empty-actions">
        <a :href="repo.sourceHomeUrl.value" target="_blank" rel="noopener" class="btn btn-ghost btn-sm">
          <IconApp name="globe" class="icon-sm" />
          直接查看仓库
        </a>
      </div>
    </div>
    <template #footer>
      <button v-if="repo.repoImport.value.selected" class="btn btn-primary btn-sm" :disabled="repo.repoImport.value.previewLoading" @click="repo.confirmRepoImport(repo.repoImport.value.selected!)">
        {{ repo.repoImport.value.previewLoading ? "下载脚本中…" : "导入此任务" }}
      </button>
      <!-- 列表为空时左侧根本没有可点的任务，这句引导会指向不存在的东西 -->
      <span v-else-if="repo.filteredRepoTasks.value.length" class="repo-footer-hint">点击左侧任务查看详情后导入</span>
      <span v-else class="repo-footer-hint">本页任务来自社区仓库，导入前请核对内容</span>
    </template>
  </Modal>

  <!-- 免责弹窗：必须显式确认/取消，禁用遮罩与 ESC 关闭 -->
  <Modal :open="!!repo.repoImport.value.disclaimer" title="免责声明" :close-on-overlay="false" :close-on-esc="false" @close="repo.cancelRepoDisclaimer">
    <p>从远程仓库导入的{{ kindLabel }}由社区成员提供，未经审核验证。</p>
    <p class="repo-disclaimer-warn">
      <strong>请仔细阅读并确认任务内容后再使用。</strong>
      <template v-if="kind === 'http'">
        HTTP 登录任务会把方案里的账号密码提交到任务中写明的地址，请确认该地址是你的校园网网关，而不是被改过的第三者接口。
      </template>
      <template v-else-if="kind === 'script'">
        登录脚本运行时会收到方案账号密码等 CAMPUS_* 环境变量。请逐行核对下方代码，确认没有将凭据发送到无关地址。
      </template>
      <template v-else>
        任务中填入的账号密码将在执行时提交到第三方网站，请确认目标网站可靠。
      </template>
    </p>
    <!-- 凭据变换脚本是要在登录时**执行**的 JavaScript：与浏览器任务的 eval/custom_js
         同级的风险，必须在导入前就说清（保存时另有一次确认，此处不能只剩"未经审核"） -->
    <p v-if="kind === 'http'" class="repo-disclaimer-warn">
      <strong>HTTP 登录任务可能包含凭据变换脚本。</strong>
      导入后登录时会执行其中的 JavaScript（沙箱内运行、无网络与文件访问），请确认来源可信。
    </p>
    <template v-if="kind === 'script'">
      <p>即将保存的脚本正文（导入后不会自动执行）：</p>
      <pre class="repo-script-preview">{{ repo.repoImport.value.scriptPreview }}</pre>
    </template>
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
/* 工具条：来源选择器与「刷新索引」同行，前者左、后者右 */
.repo-import-toolbar {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: var(--space-md);
  flex-wrap: wrap;
  margin-bottom: 8px;
}
.repo-import-field { display: flex; align-items: center; gap: 10px; flex-wrap: wrap; }
.repo-source-label { font-size: var(--text-md); color: var(--text-secondary); font-weight: 500; }
/* 来源补充说明（Gitee 更快 / GitHub 可能慢）：与工具条同样式的次级文字，不抢视线 */
.repo-source-hint { margin: 0 0 12px; font-size: var(--text-sm); color: var(--text-muted); line-height: 1.5; }
.repo-cache-hint { margin: -2px 0 8px; font-size: var(--text-xs); color: var(--text-tertiary); }
/* 未知类型条目提示：索引可继续使用，但贡献者需要修正 type。 */
.repo-foreign-hint { color: var(--warning-text); }
.repo-custom-url { margin-bottom: 12px; }
.repo-import-error { color: var(--error); font-size: var(--text-md); margin-bottom: 8px; }
.repo-import-search { margin-bottom: 12px; }
.repo-import-body { display: flex; gap: 12px; min-height: 0; }
.repo-import-list { flex: 1; min-width: 0; overflow-y: auto; display: flex; flex-direction: column; gap: 8px; max-height: 56vh; }
.repo-import-item { display: flex; width: 100%; gap: 12px; align-items: flex-start; padding: 12px; border: 1px solid var(--border); border-radius: var(--radius-md); background: transparent; color: inherit; font: inherit; text-align: left; cursor: pointer; transition: background var(--dur-fast) var(--ease-out), border-color var(--dur-fast) var(--ease-out); }
.repo-import-item:hover { background: var(--bg-hover); }
.repo-import-item:focus-visible { outline: 2px solid var(--accent); outline-offset: 2px; }
/* 选中态对齐 .task-item.active（淡底 + 边框），不用实底 accent */
.repo-import-item.selected { border-color: var(--accent); background: rgba(var(--accent-rgb), 0.06); }
.repo-item-thumb { flex: none; width: 44px; height: 44px; border-radius: var(--radius-sm); overflow: hidden; background: var(--bg-hover); }
.repo-item-thumb img { width: 100%; height: 100%; object-fit: cover; display: block; }
.repo-item-kind-icon { display: flex; align-items: center; justify-content: center; color: var(--text-secondary); }
.repo-item-kind-icon svg { width: 21px; height: 21px; }
.repo-item-text { flex: 1; min-width: 0; }
.repo-item-heading { display: flex; flex-wrap: wrap; align-items: center; gap: 6px; }
.repo-item-name { font-weight: 600; line-height: 1.4; }
/* 官方任务徽标：语义走通用 .badge--success，此处只留列表内的左侧间距 */
.repo-item-official { margin-left: 6px; vertical-align: middle; }
.repo-item-desc { display: -webkit-box; overflow: hidden; -webkit-box-orient: vertical; -webkit-line-clamp: 2; font-size: var(--text-sm); color: var(--text-secondary); line-height: 1.5; margin-top: 3px; }
.repo-item-meta { display: flex; flex-wrap: wrap; gap: 4px 10px; margin-top: 7px; font-size: var(--text-xs); color: var(--text-tertiary); }
.repo-item-kind { font-weight: 600; color: var(--text-secondary); }
.repo-item-tags { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; max-width: 100%; }
.repo-import-detail { flex: 1; min-width: 0; border: 1px solid var(--border); border-radius: var(--radius-md); padding: 12px; display: flex; flex-direction: column; gap: 8px; max-height: 56vh; overflow-y: auto; }
.repo-detail-name { margin: 0; font-size: var(--text-lg); font-weight: 600; line-height: 1.4; }
.repo-detail-desc { margin: 0; font-size: var(--text-sm); color: var(--text-secondary); line-height: 1.6; }
.repo-detail-kind { display: flex; align-items: center; gap: 10px; padding: 10px; border-radius: var(--radius-sm); background: var(--bg-hover); }
.repo-detail-kind-icon { display: flex; align-items: center; justify-content: center; flex: none; width: 32px; height: 32px; border-radius: var(--radius-xs); background: var(--bg-modal); color: var(--text-primary); }
.repo-detail-kind-icon svg { width: 18px; height: 18px; }
.repo-detail-kind strong, .repo-detail-kind small { display: block; }
.repo-detail-kind strong { font-size: var(--text-sm); }
.repo-detail-kind small { margin-top: 2px; font-size: var(--text-xs); color: var(--text-secondary); }
.repo-detail-facts { display: flex; flex-wrap: wrap; gap: 8px 24px; margin: 2px 0 0; font-size: var(--text-sm); }
.repo-detail-facts div { display: flex; gap: 7px; min-width: 0; }
.repo-detail-facts dt { color: var(--text-tertiary); }
.repo-detail-facts dd { margin: 0; overflow-wrap: anywhere; }
.repo-detail-tags { display: flex; flex-wrap: wrap; gap: 6px; }
.repo-detail-tag { padding: 2px 7px; border: 1px solid var(--border); border-radius: var(--radius-xs); font-size: var(--text-xs); color: var(--text-secondary); }
.repo-detail-links { display: flex; flex-direction: column; align-items: flex-start; gap: 6px; min-width: 0; padding-top: 6px; border-top: 1px solid var(--border); }
/* 来源仓库：与描述同层次的一行小链接，不抢主操作（导入按钮才是主操作） */
.repo-detail-source { display: inline-flex; align-items: center; gap: 5px; max-width: 100%; font-size: var(--text-xs); color: var(--text-muted); text-decoration: none; }
.repo-detail-source svg { flex: none; }
.repo-detail-source span { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.repo-detail-source:hover { color: var(--accent); text-decoration: underline; }
.repo-detail-shot { position: relative; border-radius: var(--radius-sm); overflow: hidden; border: 1px solid var(--border); }
.repo-detail-shot img { width: 100%; display: block; }
.repo-detail-shot-img { cursor: zoom-in; }
.repo-detail-zoom-hint { position: absolute; right: 8px; bottom: 8px; padding: 2px 8px; border-radius: var(--radius-xs); font-size: var(--text-xs); color: var(--text-secondary); background: var(--bg-modal); border: 1px solid var(--border); pointer-events: none; }
.repo-shot-preview { max-height: 70vh; overflow: auto; border-radius: var(--radius-sm); border: 1px solid var(--border); }
.repo-shot-preview img { width: 100%; display: block; }
.repo-detail-empty { flex: 1; display: flex; align-items: center; justify-content: center; min-height: 120px; padding: 16px; color: var(--text-tertiary); font-size: var(--text-sm); text-align: center; }
/* 列表弹窗 footer 为选中任务的操作区：有选中=导入按钮，无选中=引导文案 */
.repo-footer-hint { color: var(--text-tertiary); font-size: var(--text-sm); }
.repo-import-empty { text-align: center; color: var(--text-tertiary); padding: 24px; }
/* 空态复用全局 .empty-state--dashed（结构见 misc.css）：虚线框表达"待填充"，
   并为「直接查看仓库」留出可点区域。此处只复位整体透明度与收敛文字宽度，
   不改通用外观——.empty-state 默认 opacity:0.8 会把里面的按钮一起做旧，
   而这里有真按钮，故复位为 1，改由 .empty-desc 的色值承担弱化。
   （scoped 选择器编译后带 [data-v-*]，特异性高于全局单类，覆盖可靠且不受
   样式表注入顺序影响。） */
.repo-import-hint { text-align: center; opacity: 1; }
.repo-import-hint .empty-desc { max-width: 34em; line-height: 1.6; }

@media (max-width: 768px) {
  .repo-import-list { max-height: 40vh; }
  .repo-import-detail { max-height: none; }
}

.repo-disclaimer-warn { color: var(--error); }
.repo-script-preview { max-height: 46vh; overflow: auto; white-space: pre-wrap; overflow-wrap: anywhere; padding: 12px; border: 1px solid var(--border); border-radius: var(--radius-sm); background: var(--bg-hover); font-size: var(--text-xs); }
</style>
