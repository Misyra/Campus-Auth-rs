<script setup lang="ts">
/** 直连任务面板：**列表页 + 二级编辑页**（方案 G 终稿，自动保存模式）。
 *
 * 与浏览器任务面板同构：列表态整页任务表格，编辑态独占全宽、面包屑返回，
 * 深链 `?task=<id>` 直达编辑器（方案编辑器的「配置直连任务」入口沿用）。
 * 字段编辑由 `HttpTaskFields` 平铺呈现（无 JSON 文本框），变更 debounce
 * 静默 PUT；请求地址等缺口未补齐前自动保存不发请求（发了必 400），
 * 此时状态字改口「有 N 处待补全」并在编辑页顶部列出缺什么。
 *
 * 编辑器里没有「调试」——直连任务不经 Python Worker，无法单步执行，
 * 验证路径是发一次测试请求（凭据由宿主传入，见 useHttpTaskTest）。
 *
 * 版式类名走 `styles/pages/tasks.css` 的 `tsk-*` 共享组（三个面板同一份）。
 */
import IconApp from "@/components/common/IconApp.vue";
import FieldHelp from "@/components/common/FieldHelp.vue";
import HttpTaskFields from "@/components/common/HttpTaskFields.vue";
import HttpTestResult from "@/components/common/HttpTestResult.vue";
import HttpLoginWizard from "@/components/common/HttpLoginWizard.vue";
import { computed, onBeforeUnmount, onMounted, ref, watch } from "vue";
import { useHttpTasks, NEW_TASK_PLACEHOLDER_URL } from "@/composables/useHttpTasks";
import { useHttpTaskTest } from "@/composables/useHttpTaskTest";
import { useProfiles } from "@/composables/useProfiles";
import { useRepoImport } from "@/composables/useRepoImport";
import { useScripts } from "@/composables/useScripts";
import { useTasks } from "@/composables/useTasks";
import { useTaskEditorQuery } from "@/composables/useTaskEditorQuery";
import { useToast } from "@/composables/useToast";
import { autosaveLabel } from "@/utils/autosave";
import { useDragSort } from "@/utils/drag";
import { HTTP_CRYPTO_BUILTINS, HTTP_TEMPLATE_PLACEHOLDERS } from "@/utils/loginChannel";
import { httpTaskPayload } from "@/utils/httpTask";
import { buildHttpTaskBindingIndex, buildHttpTaskRows, filterHttpTaskRows } from "@/utils/httpTaskList";
import { formatMtime } from "@/utils/formatters";
import { TASK_REPO_URL } from "@/utils/constants";

const {
  httpTasks,
  httpTaskDraft,
  isNewDraft,
  autosaveState,
  draftGapsNow,
  duplicatingIds,
  exportingIds,
  fetchHttpTasks,
  deleteHttpTask,
  showHttpTaskEditor,
  createHttpTask,
  closeHttpTaskEditor,
  duplicateHttpTask,
  exportHttpTask,
  importHttpTask,
} = useHttpTasks();

const repo = useRepoImport();
const { toastOnly } = useToast();
const { profiles } = useProfiles();
const { running, result: testResult, runHttpTaskTest, clearTestResult } = useHttpTaskTest();

// B1：拖拽排序必须互传全量——后端 order 接口整体替换三组顺序，漏传的一组会被清空
const { tasks: browserTasks } = useTasks();
const { scripts } = useScripts();
const drag = useDragSort(httpTasks, { tasks: browserTasks, scripts, http: httpTasks });

/**
 * 行下标 → 拖拽用的**全量**下标。
 *
 * 模板里 `v-for` 走的是搜索过滤后的 `visibleRows`，而 `useDragSort` 按传入列表
 * （全量 `httpTasks`）的 id 做 splice 与持久化：把过滤后的下标直接喂给它，搜索状态
 * 下拖一行会挪动另一条任务（另两个面板踩过同一个坑）。
 */
function dragIndex(taskId: string): number {
  return httpTasks.value.findIndex((t) => t.id === taskId);
}

/**
 * 测试凭据：任务里不含账号密码（凭据属于方案），而任务页没有方案上下文，
 * 故本页要测一次就得手填一次。
 *
 * 刻意用组件本地 ref 而非草稿字段：它不属于任务配置，写进草稿会被保存到
 * `<base>/tasks/http/<id>.json`（同一门户的不同账号共用一份任务，凭据落盘就白拆了）。
 */
const testUsername = ref("");
const testPassword = ref("");

/** 表单控件 id 前缀：与 HttpTaskFields 同页共存，避免 label/for 撞车 */
const uid = `http-tasks-${Math.random().toString(36).slice(2, 8)}`;

/** 直连配置向导的开关：向导编辑的是同一份草稿（不持有第二份状态），故只是显隐控制 */
const showWizard = ref(false);

// ===== 列表态 / 编辑态（编辑态判据只有一个：草稿非空） =====
const isEditing = computed(() => !!httpTaskDraft.value);
const currentId = computed(() => httpTaskDraft.value?.id ?? "");

const { resolve, openIfNeeded, syncQuery, clearQuery } = useTaskEditorQuery({
  exists: (id) => httpTasks.value.some((t) => t.id === id),
  open: (id) => showHttpTaskEditor(id),
  currentId: () => currentId.value,
});

function openEditor(taskId: string): void {
  syncQuery(taskId);
  openIfNeeded(taskId);
}

/** 返回列表：先撤 query，再关编辑器（在途改动补一发保存） */
async function closeEditor(): Promise<void> {
  clearQuery();
  await closeHttpTaskEditor();
}

/** 新建：只在内存里起一份草稿（**不落盘**），首次真实改动才由自动保存创建文件 */
function onNewTask(): void {
  clearQuery();
  createHttpTask();
}

/**
 * 新建草稿第一次落盘后补上 `?task=<id>`：落盘前地址栏不能指向它（磁盘上没有，
 * 刷新会得到「找不到任务」），落盘后它就该和"点行进入"一样可刷新可分享。
 */
watch(isNewDraft, (isNew) => {
  const id = currentId.value;
  if (!isNew && id) syncQuery(id);
});

// ===== 列表态数据 =====
const searchQuery = ref("");

/** 方案绑定索引（active_http_task → 方案名），复用 httpTaskList 纯函数 */
const bindingIndex = computed(() => buildHttpTaskBindingIndex(profiles.value));

/** 绑定索引是否可用：profiles 未拉齐前不渲染"未绑定"（避免误导删除） */
const bindingsReady = computed(() => Object.keys(profiles.value).length > 0);

/** 列表行：名称 / 方法 / 地址 / 最近修改 / 绑定（后端 TaskSummary 自带这些字段） */
const rows = computed(() => buildHttpTaskRows(httpTasks.value, bindingIndex.value));

const visibleRows = computed(() => filterHttpTaskRows(rows.value, searchQuery.value));

// ===== 行尾 ⋯ 菜单 =====
const openMenuId = ref("");

function toggleRowMenu(taskId: string): void {
  openMenuId.value = openMenuId.value === taskId ? "" : taskId;
}

function closeRowMenu(): void {
  openMenuId.value = "";
}

function runRowAction(action: () => unknown): void {
  closeRowMenu();
  void action();
}

function onDocumentPointerDown(): void {
  closeRowMenu();
}

function onDocumentKeydown(event: KeyboardEvent): void {
  if (event.key === "Escape") closeRowMenu();
}

watch(openMenuId, (open) => {
  if (open) {
    document.addEventListener("pointerdown", onDocumentPointerDown);
    document.addEventListener("keydown", onDocumentKeydown);
  } else {
    document.removeEventListener("pointerdown", onDocumentPointerDown);
    document.removeEventListener("keydown", onDocumentKeydown);
  }
});

onBeforeUnmount(() => {
  document.removeEventListener("pointerdown", onDocumentPointerDown);
  document.removeEventListener("keydown", onDocumentKeydown);
});

// ===== 编辑态 =====

/**
 * 草稿任何变化（换任务 / 改字段）都清掉上一次测试结果：
 * 结果卡写着具体请求地址与判定结论，字段改过之后它就不再代表当前配置。
 */
watch(httpTaskDraft, () => clearTestResult(), { deep: true });

/** 草稿被换掉或关掉时收起向导（向导编辑的是某一份草稿） */
watch(
  () => httpTaskDraft.value?.id,
  () => {
    showWizard.value = false;
  },
);

/** 发送一次测试请求：用的是编辑器里的当前草稿（自动保存后的最新内容） */
async function sendTestRequest(): Promise<void> {
  const draft = httpTaskDraft.value;
  if (!draft) return;
  if (!testUsername.value.trim()) {
    toastOnly(false, "请先填写测试账号");
    return;
  }
  if (!testPassword.value) {
    toastOnly(false, "请先填写测试密码");
    return;
  }
  if (!draft.url.trim()) {
    toastOnly(false, "请先填写请求地址");
    return;
  }
  if (draft.url.trim() === NEW_TASK_PLACEHOLDER_URL) {
    toastOnly(false, "请求地址还是新建时的占位符，请先替换成你的门户地址");
    return;
  }
  await runHttpTaskTest({
    task: httpTaskPayload(draft),
    username: testUsername.value,
    password: testPassword.value,
    fetch_page: true,
  });
}

/** 归属提示（编辑器页头 ? 气泡） */
const NOTICE_HELP =
  "直连请求要生效，需在侧边栏「方案」里把登录方式设为「直连请求」并选中一个任务。\n\n" +
  "本页只负责编辑与测试：任务本身不含账号密码，凭据与匹配规则留在方案里。";

/** 自动保存状态字（缺口优先：缺口态说的是「有 N 处待补全，改动暂未保存」） */
/** 自动保存状态字（缺口优先；新建未落盘时也不能说"改动自动保存"） */
const autosaveText = computed(() =>
  autosaveLabel(autosaveState.value, draftGapsNow.value, isNewDraft.value),
);

onMounted(async () => {
  await fetchHttpTasks();
  // 目录就绪后再消费 ?task=<id>：未就绪时不判定"任务不存在"（见 useTaskEditorQuery）
  resolve();
});
</script>

<template>
  <!-- ==================== 列表态 ==================== -->
  <div v-if="!isEditing" class="tsk-list-page">
    <div class="tsk-toolbar">
      <h2 class="tsk-title">
        直连任务
        <FieldHelp :text="NOTICE_HELP" wide />
      </h2>
      <input
        v-model="searchQuery"
        class="tsk-search"
        type="text"
        placeholder="搜索名称 / ID / 描述 / 请求地址"
        aria-label="搜索直连任务"
      />
      <button type="button" class="btn btn-sm" title="从文件导入直连任务" @click="importHttpTask">
        <IconApp name="upload" class="icon-sm" />
        导入
      </button>
      <button type="button" class="btn btn-sm" title="从云端仓库导入直连任务" @click="repo.showRepoImport('http')">
        <IconApp name="globe-grid" class="icon-sm" />
        仓库导入
      </button>
      <!-- 与浏览器任务面板同一处修正：工具栏里的动作要用按钮外观，不用 `btn-ghost`
           （它同时抹掉底色与边框，会变成"夹在按钮中间的裸文字"） -->
      <a
        :href="TASK_REPO_URL"
        target="_blank"
        rel="noopener"
        class="btn btn-sm"
        title="把你的直连任务分享到任务仓库，供他人一键导入"
      >
        <IconApp name="share-2" class="icon-sm" />
        分享适配
      </a>
      <button type="button" class="btn btn-sm btn-primary" @click="onNewTask">
        <IconApp name="plus" class="icon-sm" />
        新建直连任务
      </button>
    </div>

    <div class="card tsk-table-card">
      <table class="tsk-table">
        <colgroup>
          <col class="tsk-col-drag" />
          <col class="tsk-col-name" />
          <col class="tsk-col-id" />
          <col class="tsk-col-flex" />
          <col class="tsk-col-bind" />
          <col class="tsk-col-mtime" />
          <col class="tsk-col-actions" />
        </colgroup>
        <thead>
          <tr>
            <th class="tsk-drag-cell"></th>
            <th>名称</th>
            <th class="tsk-cell-id">任务 ID</th>
            <th class="tsk-cell-flex">请求</th>
            <th class="tsk-cell-bind">绑定方案</th>
            <th class="tsk-cell-mtime">最近修改</th>
            <th class="tsk-actions">操作</th>
          </tr>
        </thead>
        <tbody>
          <tr v-if="!httpTasks.length">
            <td colspan="7">
              <div class="empty-state">
                <IconApp name="globe" :stroke-width="1.5" />
                <span>暂无直连任务</span>
                <span class="empty-desc">直连任务直接向网关发登录请求，不开浏览器、不需要 Python 与 Playwright</span>
                <div class="empty-actions">
                  <button type="button" class="btn btn-sm btn-primary" @click="onNewTask">
                    <IconApp name="plus" />新建直连任务
                  </button>
                  <button type="button" class="btn btn-sm" @click="repo.showRepoImport('http')">
                    <IconApp name="globe-grid" class="icon-sm" />仓库导入
                  </button>
                </div>
              </div>
            </td>
          </tr>
          <tr v-else-if="!visibleRows.length">
            <td colspan="7">
              <div class="empty-state">
                <IconApp name="search" :stroke-width="1.5" />
                <span>没有匹配「{{ searchQuery }}」的直连任务</span>
              </div>
            </td>
          </tr>
          <tr
            v-for="row in visibleRows"
            :key="row.id"
            data-draggable-list
            @dragstart="drag.handleDragStart($event, dragIndex(row.id))"
            @dragover="drag.onDragOver($event, dragIndex(row.id))"
            @drop="drag.onDrop($event, dragIndex(row.id))"
            @dragend="drag.onDragEnd($event)"
            @click="openEditor(row.id)"
          >
            <td class="tsk-drag-cell" @click.stop>
              <div
                class="task-drag-handle"
                title="拖拽排序"
                @mousedown="drag.onHandleMouseDown($event)"
                @mouseup="drag.onHandleMouseUp($event)"
              >
                <IconApp name="list" class="icon-sm" />
              </div>
            </td>
            <td>
              <span class="tsk-name">{{ row.name || row.id }}</span>
              <span v-if="row.description" class="tsk-name-sub">{{ row.description }}</span>
            </td>
            <td class="tsk-cell-id tsk-cell-ellipsis"><span class="tsk-mono">{{ row.id }}</span></td>
            <td class="tsk-cell-flex tsk-cell-ellipsis">
              <span class="tsk-chip-slot">
                <span v-if="row.method" class="chip chip--dense">{{ row.method }}</span>
                <span class="tsk-sub tsk-cell-ellipsis">{{ row.url || '—' }}</span>
              </span>
            </td>
            <td class="tsk-cell-bind tsk-cell-ellipsis">
              <span v-if="!bindingsReady" class="tsk-muted">—</span>
              <template v-else-if="row.boundProfiles.length">
                <span
                  v-for="name in row.boundProfiles"
                  :key="name"
                  class="badge badge--sm badge--success"
                  :title="`方案「${name}」的直连登录指向本任务`"
                >{{ name }}</span>
              </template>
              <span v-else class="tsk-muted">未绑定</span>
            </td>
            <td class="tsk-cell-mtime"><span class="tsk-mtime">{{ formatMtime(row.modifiedAt) }}</span></td>
            <td class="tsk-actions" @click.stop>
              <button type="button" class="btn btn-sm btn-icon-only" :title="`编辑：${row.name || row.id}`" @click="openEditor(row.id)">
                <IconApp name="pencil" class="icon-sm" />
              </button>
              <button
                type="button"
                class="btn btn-sm btn-icon-only"
                :title="`更多操作：${row.name || row.id}`"
                aria-haspopup="menu"
                :aria-expanded="openMenuId === row.id"
                @pointerdown.stop
                @click.stop="toggleRowMenu(row.id)"
              >
                <IconApp name="more-vertical" class="icon-sm" />
              </button>
              <div v-if="openMenuId === row.id" class="tsk-menu" role="menu" @pointerdown.stop>
                <button type="button" role="menuitem" :disabled="duplicatingIds.has(row.id)" @click="runRowAction(() => duplicateHttpTask(row.id))">复制为新任务</button>
                <button type="button" role="menuitem" :disabled="exportingIds.has(row.id)" @click="runRowAction(() => exportHttpTask(row.id))">导出 JSON</button>
                <button type="button" role="menuitem" class="tsk-menu-danger" @click="runRowAction(() => deleteHttpTask(row.id))">删除</button>
              </div>
            </td>
          </tr>
        </tbody>
      </table>
      <div v-if="httpTasks.length" class="tsk-foot">
        <span>共 {{ httpTasks.length }} 个任务</span>
        <span v-if="searchQuery.trim()">· {{ visibleRows.length }} 个匹配</span>
        <span class="tsk-spacer"></span>
        <span>拖拽行首调整顺序 · 点行进入编辑 · 改动自动保存</span>
      </div>
    </div>
  </div>

  <!-- ==================== 编辑态（二级页） ==================== -->
  <div v-else-if="httpTaskDraft" class="tsk-editor-page">
    <div class="tsk-crumb">
      <button type="button" class="tsk-crumb-back" @click="closeEditor">
        <IconApp name="chevron-down" class="icon-sm tsk-crumb-icon" />
        返回任务
      </button>
      <span class="tsk-crumb-sep">/</span>
      <span class="tsk-crumb-here">{{ httpTaskDraft.name || httpTaskDraft.id }}</span>
    </div>

    <div class="tsk-editor-head">
      <h2 class="tsk-editor-title">{{ httpTaskDraft.name || '未命名直连任务' }}</h2>
      <span :class="autosaveText.cls">{{ autosaveText.text }}</span>
      <span class="tsk-spacer"></span>
      <button type="button" class="btn btn-sm" title="导出任务 JSON" @click="exportHttpTask(httpTaskDraft.id)">
        <IconApp name="download" class="icon-sm" />
        导出
      </button>
      <button
        type="button"
        class="btn btn-sm btn-danger"
        :title="isNewDraft ? '放弃这个还没保存的新建任务' : '删除任务'"
        @click="deleteHttpTask(httpTaskDraft.id)"
      >
        <IconApp name="trash" class="icon-sm" />
        {{ isNewDraft ? '放弃' : '删除' }}
      </button>
    </div>

    <!-- 缺口提示：自动保存被缺口拦住时状态字已改口，这里把"缺什么"说全 -->
    <div v-if="draftGapsNow.length" class="tsk-gaps">
      <IconApp name="alert-triangle" class="icon-sm" />
      <span>还缺 {{ draftGapsNow.join('、') }}，补齐前改动不会保存</span>
    </div>

    <div class="tsk-grid">
      <!-- 主列 -->
      <div class="tsk-main">
        <div class="card">
          <div class="card-header">
            <h3>基本信息</h3>
            <div class="card-actions">
              <button
                type="button"
                class="http-wizard-link"
                title="分步引导填写请求参数，并在最后一步当场发一次测试请求"
                @click="showWizard = true"
              >
                <IconApp name="sparkles" class="icon-sm" />
                配置向导
              </button>
            </div>
          </div>
          <div class="card-body">
            <div class="form-row">
              <div class="form-group">
                <div class="field-label-row">
                  <label :for="`${uid}-id`">任务 ID</label>
                  <FieldHelp text="落盘文件名（<base>/tasks/http/<id>.json），也是方案里 active_http_task 引用的值。创建时生成，不可修改——改 ID 等于换一个任务，请用「复制」。" wide />
                </div>
                <input :id="`${uid}-id`" :value="httpTaskDraft.id" type="text" disabled />
              </div>
              <div class="form-group">
                <label :for="`${uid}-name`">任务名称</label>
                <input :id="`${uid}-name`" v-model="httpTaskDraft.name" type="text" placeholder="宿舍直连登录" />
              </div>
            </div>
            <div class="form-group">
              <label :for="`${uid}-desc`">描述</label>
              <input :id="`${uid}-desc`" v-model="httpTaskDraft.description" type="text" placeholder="任务描述（可选）" />
            </div>
          </div>
        </div>

        <!-- 请求形状（地址/请求头/成败判定/前置请求/退出登录/变换脚本）：与直连配置向导共用同一组件 -->
        <div class="card">
          <div class="card-header"><h3>请求配置</h3></div>
          <div class="card-body">
            <HttpTaskFields :model="httpTaskDraft" />
          </div>
        </div>

        <!-- 测试区：任务里不含凭据（凭据属于方案），本页没有方案上下文，故手填一次 -->
        <div class="card">
          <div class="card-header">
            <h3>发送一次测试请求</h3>
            <FieldHelp
              text="这里只用来验证「请求形状」对不对：任务本身不含账号密码（凭据属于方案），所以测试时要手填一次。填写的凭据仅用于本次请求，不会写进任务文件。"
              wide
            />
            <div class="card-actions">
              <button type="button" class="btn btn-sm btn-primary" :disabled="running" @click="sendTestRequest">
                <IconApp :name="running ? 'refresh' : 'play'" class="icon-sm" :class="{ spin: running }" />
                {{ running ? '正在发送…' : '发送测试请求' }}
              </button>
            </div>
          </div>
          <div class="card-body">
            <div class="form-row">
              <div class="form-group">
                <label :for="`${uid}-test-user`">测试账号</label>
                <input :id="`${uid}-test-user`" v-model="testUsername" type="text" placeholder="学号 / 手机号" />
              </div>
              <div class="form-group">
                <label :for="`${uid}-test-pass`">测试密码</label>
                <input :id="`${uid}-test-pass`" v-model="testPassword" type="password" placeholder="仅本次测试使用" />
              </div>
            </div>
            <p class="hint">测试用的是编辑器里的当前内容（已自动保存），点右上「发送测试请求」发送。</p>
            <HttpTestResult v-if="testResult" :result="testResult" />
          </div>
        </div>
      </div>

      <!-- 侧栏 -->
      <div class="tsk-side">
        <div class="card">
          <div class="card-header"><h3>快速上手</h3></div>
          <div class="card-body tsk-side-body">
            <p class="tsk-side-hint">直连请求要生效：到「方案」页把登录方式设为「直连请求」并选中本任务。</p>
            <p class="tsk-side-hint">不知道怎么填？<button type="button" class="btn btn-link" @click="showWizard = true">配置向导</button>分步带你填，最后一步当场测试。</p>
            <p class="tsk-side-hint">任务仓库里有别人适配好的门户任务，<button type="button" class="btn btn-link" @click="repo.showRepoImport('http')">从仓库导入</button>一键获取。</p>
          </div>
        </div>

        <div class="card">
          <div class="card-header"><h3>字段速查</h3></div>
          <div class="card-body tsk-side-body">
            <div class="chip-row">
              <code v-for="ph in HTTP_TEMPLATE_PLACEHOLDERS" :key="ph" class="chip">{{ ph }}</code>
            </div>
            <div class="chip-row">
              <code v-for="fn in HTTP_CRYPTO_BUILTINS" :key="fn" class="chip chip--fn">{{ fn }}</code>
            </div>
            <p class="tsk-side-hint">每个字段怎么填，见编辑器内该字段旁的 <code>?</code>。</p>
          </div>
        </div>
      </div>
    </div>

    <!-- 直连配置向导：内部走 Modal（挂到 body），与测试区共用同一对凭据 ref -->
    <HttpLoginWizard
      v-if="httpTaskDraft"
      :draft="httpTaskDraft"
      :open="showWizard"
      :test-username="testUsername"
      :test-password="testPassword"
      @close="showWizard = false"
    />
  </div>
</template>

<style scoped>
/* 面板专属样式：共享版式（列表页 / 表格 / 菜单 / 二级页 / 状态字 / 缺口条）在
   styles/pages/tasks.css 的 `tsk-*` 组里，这里只留本面板独有的两处。 */

/* 名称列略宽于共享值：本表的名称格是两行（名称 + 描述副行），20% 会把描述挤成半句；
   剩余宽度仍由「请求」列（`tsk-col-flex` = auto）吃。 */
.tsk-table col.tsk-col-name {
  width: 24%;
}

/* 配置向导入口（对齐 HttpTaskFields 的胶囊样式） */
.http-wizard-link {
  display: inline-flex;
  align-items: center;
  gap: 4px;
  padding: 2px 8px;
  border: 1px solid var(--border-accent-strong);
  border-radius: var(--radius-full);
  background: rgba(var(--accent-rgb), 0.08);
  color: var(--accent);
  font-size: var(--text-sm);
  font-weight: 600;
  cursor: pointer;
  transition: background var(--dur-base) var(--ease-out);
}

.http-wizard-link:hover {
  background: rgba(var(--accent-rgb), 0.16);
}

/* 字段速查词条改用全局 `.chip` / `.chip--fn` / `.chip-row`（components/chip.css）。
   此处原来那份与 HttpTaskFields 的 scoped 副本规则体逐字相同——scoped 样式无法
   跨组件复用，故只能各写一份；收敛到全局后副本删除。 */
</style>
