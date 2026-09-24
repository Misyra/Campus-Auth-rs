<script setup lang="ts">
/** 浏览器任务面板：**列表页 + 二级编辑页**（方案 G 终稿，自动保存模式）。
 *
 * 列表态：整页任务表格（拖拽柄 / 名称 / ID / 描述 / 绑定方案 / 最近修改 / 操作），
 * 无右列；点行或「新建任务」进入编辑态，列表整页切走，编辑器独占全宽、面包屑返回。
 * 两态由 `?task=<id>` 表达——刷新与深链均可直达编辑态，方案页等外部入口带
 * `?task=<id>` 跳进来即落编辑器。
 *
 * 自动保存：字段变更 debounce 静默 PUT（见 useTasks），无保存按钮、
 * 无「放弃未保存的修改」确认；JSON 语法非法时不落盘只标红（并在退出时明说一句）。
 *
 * 版式类名走 `styles/pages/tasks.css` 的 `tsk-*` 共享组（三个面板同一份）。
 */
import IconApp from "@/components/common/IconApp.vue";
import FieldHelp from "@/components/common/FieldHelp.vue";
import { computed, onBeforeUnmount, onMounted, ref, watch } from "vue";
import { useTasks } from "@/composables/useTasks";
import { useScripts } from "@/composables/useScripts";
import { useHttpTasks } from "@/composables/useHttpTasks";
import { useRepoImport } from "@/composables/useRepoImport";
import { useProfiles } from "@/composables/useProfiles";
import { useTaskEditorQuery } from "@/composables/useTaskEditorQuery";
import { useDragSort } from "@/utils/drag";
import { useDebug } from "@/composables/useDebug";
import { autosaveLabel } from "@/utils/autosave";
import { formatMtime } from "@/utils/formatters";
import { TASK_REPO_URL, TUTORIAL_VIDEO_URL } from "@/utils/constants";
import type { ProfileSummary } from "@/api/types";

const t = useTasks();
const s = useScripts();
const h = useHttpTasks();
const repo = useRepoImport();
const debug = useDebug();
const { profiles } = useProfiles();

// B1：拖拽排序必须互传全量——后端 order 接口会整体替换三组顺序，
// 漏传的一组会被清空，因此另两类任务的列表也要一并传入用于持久化
const drag = useDragSort(t.tasks, { tasks: t.tasks, scripts: s.scripts, http: h.httpTasks });

/** 实际列表（任务目录的浏览器任务视图） */
const browserTasks = computed(() => t.tasks.value);

/**
 * 行下标 → 拖拽用的**全量**下标。
 *
 * 模板里 `v-for` 走的是搜索过滤后的 `visibleTasks`，而 `useDragSort` 按传入列表
 * （全量 `t.tasks`）的 id 做 splice 与持久化：把过滤后的下标直接喂给它，搜索状态
 * 下拖一行会挪动另一条任务，并把错的顺序写进后端（刷新后才暴露）。
 */
function dragIndex(taskId: string): number {
  return t.tasks.value.findIndex((task) => task.id === taskId);
}

// ===== 二级编辑页路由语义：?task=<id> =====
/** 编辑态判据只有一个：草稿非空（query 只是"意图"，见 useTaskEditorQuery） */
const isEditing = computed(() => !!t.editingTask.value);
const currentId = computed(() => t.editingTask.value?.id ?? "");

const { resolve, openIfNeeded, syncQuery, clearQuery } = useTaskEditorQuery({
  exists: (id) => browserTasks.value.some((task) => task.id === id),
  open: (id) => t.showTaskEditor(id),
  currentId: () => currentId.value,
});

/** 进入编辑态：写 ?task=<id>（history push，返回即列表） */
function openEditor(taskId: string): void {
  syncQuery(taskId);
  openIfNeeded(taskId);
}

/** 返回列表态：先撤 query，再关编辑器（在途改动补一发保存，见 useTasks） */
async function closeEditor(): Promise<void> {
  clearQuery();
  await t.closeTaskEditor();
}

/** 新建：只在内存里起一份草稿（**不落盘**），首次真实改动才由自动保存创建文件 */
function onNewTask(): void {
  clearQuery();
  t.createTask();
}

/**
 * 新建草稿第一次落盘后补上 `?task=<id>`。
 *
 * 落盘之前地址栏不能指向它（磁盘上没有这个任务，刷新会得到「找不到任务」，分享出去的
 * 链接也是死的）；落盘之后它就该和"点行进入"完全一样——可刷新、可后退、可分享。
 */
watch(
  () => t.isNewDraft.value,
  (isNew) => {
    const id = currentId.value;
    if (!isNew && id) syncQuery(id);
  },
);

// ===== 搜索：纯前端过滤（名称 / 任务 ID / 描述） =====
const searchQuery = ref("");

/** 过滤后的列表（保持拖拽排序顺序） */
const visibleTasks = computed(() => {
  const q = searchQuery.value.trim().toLowerCase();
  if (!q) return browserTasks.value;
  return browserTasks.value.filter(
    (task) =>
      (task.name ?? "").toLowerCase().includes(q) ||
      task.id.toLowerCase().includes(q) ||
      (task.description ?? "").toLowerCase().includes(q),
  );
});

/**
 * 方案绑定索引：任务 id → 引用它的方案名（列表「绑定方案」列的数据源）。
 *
 * 浏览器任务的绑定关系存 `ProfileSummary.active_task`（直连任务才走
 * `active_http_task`，见 utils/httpTaskList 的同构实现）。按方案名排序保证
 * pill 顺序稳定。profiles 未就绪（空表）时返回空 Map，列显示「—」而不是
 * 误导性的"未绑定"。
 */
const bindingIndex = computed(() => {
  const index = new Map<string, string[]>();
  for (const profile of Object.values(profiles.value) as ProfileSummary[]) {
    const taskId = String(profile?.active_task ?? "").trim();
    if (!taskId) continue;
    const displayName = String(profile?.name ?? "").trim() || String(profile?.id ?? "").trim();
    index.set(taskId, [...(index.get(taskId) ?? []), displayName]);
  }
  for (const names of index.values()) names.sort();
  return index;
});

/** 绑定索引是否可用（profiles 已拉取）：未就绪时绑定列显示「—」 */
const bindingsReady = computed(() => Object.keys(profiles.value).length > 0);

// ===== 行尾 ⋯ 菜单：同一时刻只开一个 =====
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

// 切走时菜单还开着的话，那两个 document 监听会永久留着（Esc 会调到已卸载组件的作用域里）
onBeforeUnmount(() => {
  closeRowMenu();
});

// ===== 编辑态派生 =====
/**
 * 自动保存状态字。
 *
 * 落盘闸口有两条：JSON 为空与 JSON 语法非法——两者都会让 `persistDraft` 直接返回、
 * 一个字节都不写。缺口清单必须把这件事说出来，否则状态字依旧是「改动自动保存」，
 * 用户看到红字提示的视线不在编辑器上时会把没落盘的编辑当成已存（口径同直连 / 脚本
 * 面板的缺口优先，见 utils/autosave）。
 */
const jsonGate = computed<string[]>(() => {
  const json = t.editingTask.value?.json ?? "";
  if (!json.trim()) return ["JSON 配置"];
  return t.jsonError.value ? ["JSON 语法"] : [];
});

/** 自动保存状态字：缺口优先于 idle/saved，新建未落盘时改口（见 utils/autosave） */
const autosaveText = computed(() =>
  autosaveLabel(t.autosaveState.value, jsonGate.value, t.isNewDraft.value),
);

/** 调试动作的禁用条件：未落盘的新建草稿没有可调试的对象 */
const debugDisabled = computed(
  () => !currentId.value || t.isNewDraft.value || debug.loading.value,
);

/** JSON 合法且非空：只用于输入框描边 */
const jsonValid = computed(
  () => !!t.editingTask.value?.json.trim() && !t.jsonError.value,
);

/** 内置默认任务是浏览器渠道的兜底（未绑定方案时用它），不可删除 */
const isDefaultTask = computed(() => currentId.value === "default");

onMounted(async () => {
  await t.fetchTasks();
  // 目录就绪后再消费 ?task=<id>：未就绪时不判定"任务不存在"（见 useTaskEditorQuery）
  resolve();
});
</script>

<template>
  <!-- ==================== 列表态 ==================== -->
  <div v-if="!isEditing" class="tsk-list-page">
    <div class="tsk-toolbar">
      <h2 class="tsk-title">
        浏览器任务
        <FieldHelp
          text="任务本身不会自动运行：要到「方案」页选中这个任务并保存，自动登录才会用到它。&#10;&#10;未绑定时浏览器渠道会回退用内置的 default 任务。"
          wide
        />
      </h2>
      <input
        v-model="searchQuery"
        class="tsk-search"
        type="text"
        placeholder="搜索名称 / ID / 描述"
        aria-label="搜索浏览器任务"
      />
      <button type="button" class="btn btn-sm" title="从文件导入任务" @click="t.importTask()">
        <IconApp name="upload" class="icon-sm" />
        导入
      </button>
      <button type="button" class="btn btn-sm" title="从云端仓库导入" @click="repo.showRepoImport('browser')">
        <IconApp name="globe-grid" class="icon-sm" />
        仓库导入
      </button>
      <!-- 外链但仍是工具栏里的一个动作：这里不能用 `btn-ghost`——它同时抹掉底色与边框，
           于是这个 108px 的盒子变成"夹在两个按钮中间的裸文字"，看着不像能点。
           与旁边的「导入 / 仓库导入」同一套外观（外部去向由 title 说明）。 -->
      <a :href="TASK_REPO_URL" target="_blank" rel="noopener" class="btn btn-sm" title="把你的登录任务分享到任务仓库，供他人一键导入">
        <IconApp name="share-2" class="icon-sm" />
        分享适配
      </a>
      <button type="button" class="btn btn-sm btn-primary" @click="onNewTask">
        <IconApp name="plus" class="icon-sm" />
        新建任务
      </button>
    </div>

    <div class="card tsk-table-card">
      <table class="tsk-table">
        <colgroup>
          <col class="tsk-col-drag" />
          <col class="tsk-col-name" />
          <col class="tsk-col-id" />
          <col class="tsk-col-bind" />
          <col class="tsk-col-mtime" />
          <col class="tsk-col-actions" />
        </colgroup>
        <thead>
          <tr>
            <th class="tsk-drag-cell"></th>
            <th>名称</th>
            <th class="tsk-cell-id">任务 ID</th>
            <th class="tsk-cell-bind">绑定方案</th>
            <th class="tsk-cell-mtime">最近修改</th>
            <th class="tsk-actions">操作</th>
          </tr>
        </thead>
        <tbody>
          <tr v-if="!browserTasks.length">
            <td colspan="6">
              <div class="empty-state">
                <IconApp name="layout" :stroke-width="1.5" />
                <span>暂无任务配置</span>
                <span class="empty-desc">从任务仓库导入现成的登录任务，或点「新建任务」开始</span>
                <div class="empty-actions">
                  <button type="button" class="btn btn-sm btn-primary" @click="onNewTask">
                    <IconApp name="plus" />新建任务
                  </button>
                  <button type="button" class="btn btn-sm" @click="repo.showRepoImport('browser')">
                    <IconApp name="globe-grid" class="icon-sm" />仓库导入
                  </button>
                </div>
              </div>
            </td>
          </tr>
          <tr v-else-if="!visibleTasks.length">
            <td colspan="6">
              <div class="empty-state">
                <IconApp name="search" :stroke-width="1.5" />
                <span>没有匹配「{{ searchQuery }}」的任务</span>
              </div>
            </td>
          </tr>
          <tr
            v-for="task in visibleTasks"
            :key="task.id"
            data-draggable-list
            @dragstart="drag.handleDragStart($event, dragIndex(task.id))"
            @dragover="drag.onDragOver($event, dragIndex(task.id))"
            @drop="drag.onDrop($event, dragIndex(task.id))"
            @dragend="drag.onDragEnd($event)"
            @click="openEditor(task.id)"
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
              <span class="tsk-name">{{ task.name || task.id }}</span>
              <span v-if="task.description" class="tsk-name-sub">{{ task.description }}</span>
            </td>
            <td class="tsk-cell-id tsk-cell-ellipsis"><span class="tsk-mono">{{ task.id }}</span></td>
            <td class="tsk-cell-bind tsk-cell-ellipsis">
              <span v-if="!bindingsReady" class="tsk-muted">—</span>
              <template v-else-if="bindingIndex.get(task.id)?.length">
                <span v-for="name in bindingIndex.get(task.id)" :key="name" class="badge badge--sm badge--success">{{ name }}</span>
              </template>
              <span v-else class="tsk-muted">未绑定</span>
            </td>
            <td class="tsk-cell-mtime"><span class="tsk-mtime">{{ formatMtime(task.modified_at) }}</span></td>
            <td class="tsk-actions" @click.stop>
              <button type="button" class="btn btn-sm btn-icon-only" :title="`编辑：${task.name || task.id}`" @click="openEditor(task.id)">
                <IconApp name="pencil" class="icon-sm" />
              </button>
              <button
                type="button"
                class="btn btn-sm btn-icon-only"
                :title="`调试运行：${task.name || task.id}`"
                :disabled="debug.loading.value"
                @click="debug.startDebug(task.id)"
              >
                <IconApp :name="debug.loading.value ? 'refresh' : 'bug'" class="icon-sm" :class="{ spin: debug.loading.value }" />
              </button>
              <button
                type="button"
                class="btn btn-sm btn-icon-only"
                :title="`更多操作：${task.name || task.id}`"
                aria-haspopup="menu"
                :aria-expanded="openMenuId === task.id"
                @pointerdown.stop
                @click.stop="toggleRowMenu(task.id)"
              >
                <IconApp name="more-vertical" class="icon-sm" />
              </button>
              <div v-if="openMenuId === task.id" class="tsk-menu" role="menu" @pointerdown.stop>
                <button type="button" role="menuitem" :disabled="debug.loading.value" @click="runRowAction(() => debug.startDebug(task.id))">调试</button>
                <button type="button" role="menuitem" :disabled="t.duplicatingIds.has(task.id)" @click="runRowAction(() => t.duplicateTask(task.id))">复制为新任务</button>
                <button type="button" role="menuitem" :disabled="t.exportingIds.has(task.id)" @click="runRowAction(() => t.exportTask(task.id))">导出 JSON</button>
                <button
                  type="button"
                  role="menuitem"
                  class="tsk-menu-danger"
                  :disabled="task.id === 'default'"
                  :title="task.id === 'default' ? '内置默认任务是浏览器渠道的兜底，不可删除' : ''"
                  @click="runRowAction(() => t.deleteTask(task.id))"
                >删除</button>
              </div>
            </td>
          </tr>
        </tbody>
      </table>
      <div v-if="browserTasks.length" class="tsk-foot">
        <span>共 {{ browserTasks.length }} 个任务</span>
        <span v-if="searchQuery.trim()">· {{ visibleTasks.length }} 个匹配</span>
        <span class="tsk-spacer"></span>
        <span>拖拽行首调整顺序 · 点行进入编辑 · 改动自动保存</span>
      </div>
    </div>
  </div>

  <!-- ==================== 编辑态（二级页） ==================== -->
  <div v-else-if="t.editingTask.value" class="tsk-editor-page">
    <div class="tsk-crumb">
      <button type="button" class="tsk-crumb-back" @click="closeEditor">
        <IconApp name="chevron-down" class="icon-sm tsk-crumb-icon" />
        返回任务
      </button>
      <span class="tsk-crumb-sep">/</span>
      <span class="tsk-crumb-here">{{ t.editingTask.value.name || t.editingTask.value.id }}</span>
    </div>

    <div class="tsk-editor-head">
      <h2 class="tsk-editor-title">{{ t.editingTask.value.name || '未命名任务' }}</h2>
      <span :class="autosaveText.cls">{{ autosaveText.text }}</span>
      <span class="tsk-spacer"></span>
      <button type="button" class="btn btn-sm" title="导出任务 JSON" @click="t.exportTask(t.editingTask.value.id)">
        <IconApp name="download" class="icon-sm" />
        导出
      </button>
      <button
        v-if="!isDefaultTask"
        type="button"
        class="btn btn-sm btn-danger"
        :title="t.isNewDraft.value ? '放弃这个还没保存的新建任务' : '删除任务'"
        @click="t.deleteTask(t.editingTask.value.id)"
      >
        <IconApp name="trash" class="icon-sm" />
        {{ t.isNewDraft.value ? '放弃' : '删除' }}
      </button>
    </div>

    <div class="tsk-grid">
      <!-- 主列 -->
      <div class="tsk-main">
        <div class="card">
          <div class="card-header"><h3>基本信息</h3></div>
          <div class="card-body">
            <div class="form-row">
              <div class="form-group">
                <label for="task-id">任务 ID</label>
                <input id="task-id" :value="t.editingTask.value.id" type="text" disabled />
                <span class="hint">
                  <template v-if="isDefaultTask">内置默认任务：方案未选中任何任务时浏览器登录用它</template>
                  <template v-else-if="t.isNewDraft.value">改动后自动创建，ID 由创建时生成、之后不可修改</template>
                  <template v-else>创建时生成，不可修改；方案引用的就是它</template>
                </span>
              </div>
              <div class="form-group">
                <label for="task-name">任务名称</label>
                <input id="task-name" v-model="t.editingTask.value.name" type="text" placeholder="我的登录任务" @input="t.syncMetaToJson()" />
              </div>
            </div>
            <div class="form-group">
              <label for="task-desc">描述</label>
              <input id="task-desc" v-model="t.editingTask.value.description" type="text" placeholder="任务描述（可选）" @input="t.syncMetaToJson()" />
            </div>
            <div class="form-group">
              <label for="task-url">认证地址</label>
              <input id="task-url" v-model="t.editingTask.value.url" type="text" placeholder="不填则使用系统设置的认证地址" />
              <span class="hint">留空默认使用系统认证地址</span>
            </div>
          </div>
        </div>

        <div class="card">
          <div class="card-header">
            <h3>JSON 配置</h3>
            <div class="card-actions">
              <button type="button" class="btn btn-sm" title="用内置默认任务的配置覆盖当前内容" @click="t.loadTemplate('default')">加载默认模板</button>
              <button type="button" class="btn btn-sm" title="格式化 JSON（2 空格缩进）" @click="t.formatJson()">格式化</button>
              <button
                type="button"
                class="btn btn-sm"
                @click="debug.startDebug(t.editingTask.value.id)"
                :disabled="debugDisabled"
                :title="t.isNewDraft.value ? '新建任务还没有落盘，改动后才会创建它' : '单步调试当前任务'"
              >
                <IconApp :name="debug.loading.value ? 'refresh' : 'bug'" class="icon-sm" :class="{ spin: debug.loading.value }" />
                {{ debug.loading.value ? '启动中…' : '调试' }}
              </button>
            </div>
          </div>
          <div class="card-body">
            <div class="form-group">
              <textarea
                id="task-json"
                v-model="t.editingTask.value.json"
                class="task-json-editor"
                rows="18"
                placeholder="任务JSON配置"
                :class="{ 'json-invalid': t.jsonError.value, 'json-valid': jsonValid }"
                @input="t.validateJson(); t.syncJsonToMeta()"
              ></textarea>
              <div v-if="t.jsonError.value" class="json-error">JSON 语法错误，修正前不会保存：{{ t.jsonError.value }}</div>
              <!-- 危险步骤常驻提示：自动保存模式下没有"保存前"这个时机了（旧版在那里弹一次
                   确认，改一次字段就静默落盘），所以提示挪到字段旁边——用户得当场知道存的是什么 -->
              <div v-else-if="t.dangerousSteps.value.length" class="note note--warn">
                <IconApp name="alert-triangle" class="icon-sm" />
                <span>
                  这份配置含 {{ t.dangerousSteps.value.length }} 个会执行 JavaScript 的步骤（{{
                    t.dangerousSteps.value.map((d) => `第 ${d.stepIndex} 步 ${d.stepType}`).join("、")
                  }}）。它们能在页面上下文里执行任意 JS，改动停手半秒即自动保存——确认来源可信再继续。
                </span>
              </div>
              <span v-else class="hint">语法合法的改动会自动保存；语法错误时不落盘，修正后自动恢复</span>
            </div>
          </div>
        </div>
      </div>

      <!-- 侧栏 -->
      <div class="tsk-side">
        <div class="card">
          <div class="card-header"><h3>执行与调试</h3></div>
          <div class="card-body tsk-side-body">
            <button
              type="button"
              class="btn btn-primary tsk-side-btn"
              @click="debug.startDebug(t.editingTask.value.id)"
              :disabled="debugDisabled"
              :title="t.isNewDraft.value ? '新建任务还没有落盘，改动后才会创建它' : '单步调试当前任务'"
            >
              <IconApp :name="debug.loading.value ? 'refresh' : 'bug'" class="icon-sm" :class="{ spin: debug.loading.value }" />
              {{ debug.loading.value ? '启动中…' : '调试运行' }}
            </button>
            <dl class="tsk-kv">
              <dt>绑定方案</dt>
              <dd>
                <template v-if="bindingsReady && bindingIndex.get(currentId)?.length">
                  <span v-for="name in bindingIndex.get(currentId)" :key="name" class="badge badge--sm badge--success">{{ name }}</span>
                </template>
                <span v-else-if="!bindingsReady" class="tsk-muted">—</span>
                <span v-else class="tsk-muted">尚未被任何方案引用</span>
              </dd>
            </dl>
          </div>
        </div>

        <div class="card">
          <div class="card-header"><h3>快速上手</h3></div>
          <div class="card-body tsk-side-body">
            <p class="tsk-side-hint">任务本身不会自动运行：到「方案」页选中它并保存，自动登录才会使用。</p>
            <p class="tsk-side-hint">
              不想手写 JSON？<button type="button" class="btn btn-link" @click="repo.showRepoImport('browser')">从仓库导入</button>现成任务，或用<a :href="TUTORIAL_VIDEO_URL" target="_blank" rel="noopener noreferrer">任务录制器</a>生成。
            </p>
            <p class="tsk-side-hint">步骤类型与字段说明见<a href="/api/docs/task-writing-guide" target="_blank" rel="noopener noreferrer">编写指南</a>。</p>
          </div>
        </div>
      </div>
    </div>
  </div>
</template>

<style scoped>
/* 面板专属样式：共享版式（列表页 / 表格 / 菜单 / 二级页 / 状态字 / 键值块）在
   styles/pages/tasks.css 的 `tsk-*` 组里，这里只留 JSON 编辑器与本表的一处列宽 */

/* 描述并入名称格之后本表不再有独立的弹性列，故让**名称列**吃剩余宽度
   （fixed 布局下 auto 列分得剩余空间；不改的话剩下 5 列会被等比例放大，
   名称两行反而分不到最宽）。 */
.tsk-table col.tsk-col-name {
  width: auto;
}

/* JSON 正文编辑器：等宽 + 可纵向拉伸（中文描述与长步骤数组都要能读） */
.task-json-editor {
  width: 100%;
  min-height: 380px;
  padding: 12px;
  background: var(--bg-glass);
  border: 1px solid var(--border);
  border-radius: var(--radius-lg);
  color: var(--text-primary);
  font-family: var(--font-mono);
  font-size: var(--text-md);
  line-height: 1.6;
  resize: vertical;
  transition: border-color var(--dur-base) var(--ease-out), box-shadow var(--dur-base) var(--ease-out);
}

.task-json-editor:focus-visible {
  outline: none;
  border-color: var(--accent);
}
</style>
