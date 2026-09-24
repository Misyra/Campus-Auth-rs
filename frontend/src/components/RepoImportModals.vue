<script setup lang="ts">
/**
 * 仓库导入双弹窗（导入列表 + 免责声明）。
 * 状态存于 useRepoImport 单例。此前 Modal 仅挂在 TasksView，
 * 导致设置·任务页点「从仓库导入」无反应、切到任务列表页才弹出的错位 bug，
 * 故提取为共享组件，两处入口（TasksView / 任务与环境设置页）各自挂载。
 *
 * 两类任务各有一份索引（`index.json` 收浏览器任务、`index.http.json` 收直连任务），
 * 弹窗只读 `repoKind` 那一份：标题、空态与导入去向都随它变化，避免"看着是浏览器任务、
 * 导进去变成直连草稿"这种错位。索引文件里若出现另一类的条目，那是文件写错了，此时
 * 显式提示条数而非静默过滤。
 *
 * 列表为左右分栏：左侧任务条目（含 64px 缩略图），点选后右侧展示
 * 截图大图与完整信息；无 screenshot 定义的任务显示「暂无截图」占位。
 * 详情区大图可点击放大（三层弹窗：导入列表 > 放大预览，免责声明互斥）。
 * 缩略图/大图均经 /api/repo/image 同源代理（免鉴权 `<img>` 引用口径，
 * 出站限死任务站 raw 域），加载失败回退占位而非破图。
 *
 * 来源选择器与「加载索引」同处一行工具条：此前「加载索引」独占一行、
 * 与它作用的来源选择器被隔开，看不出点它会用哪个源。
 */
import { computed, ref, watch } from "vue";
import Modal from "./common/Modal.vue";
import IconApp from "./common/IconApp.vue";
import { repoApi } from "@/api";
import { TASK_REPO_SOURCES } from "@/utils/constants";
import { repoSourceLabel, repoSourceUrl } from "@/utils/repoSource";
import { useRepoImport, repoKindLabel } from "@/composables/useRepoImport";

const repo = useRepoImport();

/** 源选项（模板直接遍历，避免在模板里硬编码按钮——增删源只改 constants） */
const sourceOptions = TASK_REPO_SOURCES;

/** 当前浏览的条目类型（决定读哪份索引、标题与导入去向） */
const kind = computed(() => repo.repoImport.value.repoKind);

/** 当前类别的中文名（标题、空态与异类条目提示共用同一处措辞，见 useRepoImport） */
const kindLabel = computed(() => repoKindLabel(kind.value));

/** 弹窗标题：两类任务各有一份索引，标题必须说清在看哪一类 */
const modalTitle = computed(() => `从云端仓库导入${kindLabel.value}`);

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

/** 来源仓库链接（改编自他人脚本时在索引里标注；只放行 http(s)，见 repoSource.ts） */
const sourceUrl = computed(() => repoSourceUrl(repo.repoImport.value.selected?.source));
/** 来源链接文字（去掉协议与 www.，否则半行都是前缀） */
const sourceLabel = computed(() => repoSourceLabel(sourceUrl.value));

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
  <Modal :open="repo.repoImport.value.visible" :title="modalTitle" size="xl" @close="repo.closeRepoImport">
    <!-- 来源选择器与「加载索引」同行：点它会用哪个源，一看便知 -->
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
        <IconApp :name="repo.repoImport.value.loading ? 'refresh' : 'download'" class="icon-sm" :class="{ spin: repo.repoImport.value.loading }" />
        {{ repo.repoImport.value.loading ? '加载中...' : '加载索引' }}
      </button>
    </div>
    <!-- 来源说明：「国内用户建议用 Gitee」的提示由 TASK_REPO_SOURCES 的 hint 承载，
         自定义源无 hint 故整行不渲染 -->
    <p v-if="repo.currentSource.value.hint" class="repo-source-hint">{{ repo.currentSource.value.hint }}</p>
    <!-- 异类条目提示：索引文件只承载一类条目，出现不符即该文件写错了（或自定义地址指到了
         另一类的索引）。数量显式说出来，避免用户对着短列表猜"我的学校去哪了" -->
    <p v-if="repo.foreignRepoTaskCount.value > 0" class="repo-source-hint repo-foreign-hint">
      索引里有 {{ repo.foreignRepoTaskCount.value }} 个条目不属于{{ kindLabel }}（类型不符），已跳过。
    </p>

    <div v-if="repo.repoImport.value.source === 'custom'" class="repo-custom-url">
      <div class="form-group form-group--flush"><input v-model="repo.repoImport.value.url" type="text" placeholder="输入远程索引 URL" /></div>
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
            <!-- meta 行自带 flex + gap + 小字弱色（.repo-item-meta），
                 子项无需任何额外规则——原先挂的 .repo-item-author / .repo-item-tags
                 全仓没有对应规则，是空钩子，故去掉。 -->
            <div class="repo-item-meta">
              <span v-if="task.author">{{ task.author }}</span>
              <span v-if="task.tags">{{ task.tags.join(', ') }}</span>
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
          <!-- 来源仓库：任务改编自他人脚本时标出处，可点开看原始实现。地址来自远端
               索引，故经 repoSourceUrl 只放行 http(s)（见 utils/repoSource.ts） -->
          <a
            v-if="sourceUrl"
            class="repo-detail-source"
            :href="sourceUrl"
            target="_blank"
            rel="noopener"
            title="本任务改编自该仓库，点开查看原始实现"
          >
            <IconApp name="external-link" class="icon-sm" />
            <span>来源：{{ sourceLabel }}</span>
          </a>
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
    <div v-else-if="!repo.repoImport.value.loading" class="empty-state empty-state--dashed repo-import-hint">
      <IconApp name="globe-grid" :stroke-width="1.5" />
      <!-- 「还没点加载」与「加载成功但这一类没有条目」是两件事：另一类任务有各自的索引，
           空列表在拆分后是合法状态，不能说成"尚未加载"或"格式不正确" -->
      <strong class="empty-title">{{ repo.repoImport.value.loaded ? `该来源暂无${kindLabel}条目` : "尚未加载任务列表" }}</strong>
      <span v-if="repo.repoImport.value.loaded" class="empty-desc">
        索引已读取，但里面没有{{ kindLabel }}。{{ kindLabel === "浏览器任务" ? "直连任务" : "浏览器任务" }}有各自的索引，
        可到对应子页的「仓库导入」查看；也可以换个来源再试。
      </span>
      <span v-else class="empty-desc">点击上方「加载索引」，从任务仓库获取可导入的任务。</span>
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
      <button v-if="repo.repoImport.value.selected" class="btn btn-primary btn-sm" @click="repo.confirmRepoImport(repo.repoImport.value.selected!)">导入此任务</button>
      <!-- 列表为空时左侧根本没有可点的任务，这句引导会指向不存在的东西 -->
      <span v-else-if="repo.repoImport.value.tasks.length" class="repo-footer-hint">点击左侧任务查看详情后导入</span>
      <span v-else class="repo-footer-hint">本页任务来自社区仓库，导入前请核对内容</span>
    </template>
  </Modal>

  <!-- 免责弹窗：必须显式确认/取消，禁用遮罩与 ESC 关闭 -->
  <Modal :open="!!repo.repoImport.value.disclaimer" title="免责声明" :close-on-overlay="false" :close-on-esc="false" @close="repo.cancelRepoDisclaimer">
    <p>从远程仓库导入的{{ kind === 'http' ? '直连任务' : '任务' }}由社区成员提供，未经审核验证。</p>
    <p class="repo-disclaimer-warn">
      <strong>请仔细阅读并确认任务内容后再使用。</strong>
      <template v-if="kind === 'http'">
        直连任务会把方案里的账号密码提交到任务中写明的地址，请确认该地址是你的校园网网关，而不是被改过的第三者接口。
      </template>
      <template v-else>
        任务中填入的账号密码将在执行时提交到第三方网站，请确认目标网站可靠。
      </template>
    </p>
    <!-- 凭据变换脚本是要在登录时**执行**的 JavaScript：与浏览器任务的 eval/custom_js
         同级的风险，必须在导入前就说清（保存时另有一次确认，此处不能只剩"未经审核"） -->
    <p v-if="kind === 'http'" class="repo-disclaimer-warn">
      <strong>直连任务可能包含凭据变换脚本。</strong>
      导入后登录时会执行其中的 JavaScript（沙箱内运行、无网络与文件访问），请确认来源可信。
    </p>
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
/* 工具条：来源选择器与「加载索引」同行，前者左、后者右 */
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
/* 异类条目提示：诊断口径（索引文件被写混、或自定义地址指到了另一类的索引），
   用警告色区别于上面的来源说明，但不像 .repo-import-error 那样当失败处理 */
.repo-foreign-hint { color: var(--warning-text); }
.repo-custom-url { margin-bottom: 12px; }
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
/* 来源仓库：与描述同层次的一行小链接，不抢主操作（导入按钮才是主操作） */
.repo-detail-source { display: inline-flex; align-items: center; gap: 5px; max-width: 100%; font-size: var(--text-xs); color: var(--text-muted); text-decoration: none; }
.repo-detail-source span { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.repo-detail-source:hover { color: var(--accent); text-decoration: underline; }
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
/* 空态复用全局 .empty-state--dashed（结构见 misc.css）：虚线框表达"待填充"，
   并为「直接查看仓库」留出可点区域。此处只复位整体透明度与收敛文字宽度，
   不改通用外观——.empty-state 默认 opacity:0.8 会把里面的按钮一起做旧，
   而这里有真按钮，故复位为 1，改由 .empty-desc 的色值承担弱化。
   （scoped 选择器编译后带 [data-v-*]，特异性高于全局单类，覆盖可靠且不受
   样式表注入顺序影响。） */
.repo-import-hint { text-align: center; opacity: 1; }
.repo-import-hint .empty-desc { max-width: 34em; line-height: 1.6; }

.repo-disclaimer-warn { color: var(--error); }
</style>
