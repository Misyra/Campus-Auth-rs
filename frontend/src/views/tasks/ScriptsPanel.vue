<script setup lang="ts">
/** 脚本面板：**列表页 + 二级编辑页**（方案 G 终稿，自动保存模式）。
 *
 * 与浏览器任务 / 直连任务面板同构：列表态整页脚本表格（拖拽柄 / 名称 / ID /
 * 描述 / 执行程序 / 最近修改 / 操作），点行或「新建脚本」进入编辑态，编辑器独占
 * 全宽、面包屑返回，两态由 `?task=<id>` 表达（刷新与深链直达编辑态）。
 *
 * 编辑模型与另两个面板一致：字段变更 debounce 静默 PUT，没有保存按钮、没有
 * 「放弃未保存的修改？」确认。差异只在新建——脚本 ID 是文件名，得由用户命名，
 * 故新建先给一份空 ID 草稿，ID 合法后第一次自动保存才创建文件（见 useScripts）。
 *
 * 版式类名走 `styles/pages/tasks.css` 的 `tsk-*` 共享组：三个面板此前各复制一份
 * 约 350 行的 scoped 样式，改一处漏一处就漂移。
 */
import IconApp from "@/components/common/IconApp.vue";
import CustomSelect from "@/components/common/CustomSelect.vue";
import type { SelectOption } from "@/components/common/CustomSelect.vue";
import { computed, onBeforeUnmount, onMounted, ref, watch } from "vue";
import { useScripts } from "@/composables/useScripts";
import { useTasks } from "@/composables/useTasks";
import { useHttpTasks } from "@/composables/useHttpTasks";
import { useTaskEditorQuery } from "@/composables/useTaskEditorQuery";
import { useDragSort } from "@/utils/drag";
import { autosaveLabel } from "@/utils/autosave";
import { formatMtime } from "@/utils/formatters";

const {
  scripts,
  availableBinaries,
  editingTask,
  isNewDraft,
  runningIds,
  exportingIds,
  lastRunResult,
  autosaveState,
  draftGapsNow,
  commitScriptId,
  getBinaryName,
  fetchScripts,
  fetchAvailableBinaries,
  showScriptEditor,
  createScriptDraft,
  closeScriptEditor,
  deleteScript,
  runScript,
  exportScript,
  importScript,
  loadScriptTemplate,
  onBinarySelectChange,
} = useScripts();

// B1：拖拽排序复用 useDragSort——本列表（脚本）重排，浏览器任务 / 脚本 / 直连任务
// 三组顺序均随请求全量持久化（后端 order 接口整体替换，漏传的一组会被清空）
const { tasks: browserTasks } = useTasks();
const { httpTasks } = useHttpTasks();
const drag = useDragSort(scripts, { tasks: browserTasks, scripts, http: httpTasks });

/**
 * 行下标 → 拖拽用的**全量**下标。
 *
 * 模板里 `v-for` 走的是搜索过滤后的 `visibleScripts`，而 `useDragSort` 按传入
 * 列表（全量 `scripts`）的 id 做 splice 与持久化：直接把过滤后的下标喂给它，
 * 搜索状态下拖一行会挪动另一个脚本，并把错的顺序写进后端（刷新才暴露）。
 */
function dragIndex(scriptId: string): number {
  return scripts.value.findIndex((s) => s.id === scriptId);
}

// ===== 列表态 / 编辑态：两态由「草稿是否为空」表达，`?task=<id>` 只是意图 =====
const currentId = computed(() => editingTask.value?.id ?? "");

const { resolve, openIfNeeded, syncQuery, clearQuery } = useTaskEditorQuery({
  exists: (id) => scripts.value.some((s) => s.id === id),
  open: (id) => showScriptEditor(id),
  currentId: () => currentId.value,
});

const isEditing = computed(() => !!editingTask.value);

function openEditor(scriptId: string): void {
  // 地址栏与编辑器一起走：query 让刷新/返回保持编辑态
  syncQuery(scriptId);
  openIfNeeded(scriptId);
}

/** 返回列表：先撤 query（浏览器返回即列表），再关编辑器（在途改动补一发保存） */
async function closeEditor(): Promise<void> {
  clearQuery();
  await closeScriptEditor();
}

/** 新建：ID 由用户命名，故先给一份草稿（不落盘），补上 ID 后自动保存创建 */
function onNewScript(): void {
  clearQuery();
  createScriptDraft();
}

/**
 * 新建草稿第一次落盘后补上 `?task=<id>`：落盘前地址栏不能指向它（磁盘上没有，
 * 刷新会得到「找不到任务」），落盘后它就该和"点行进入"一样可刷新可分享。
 */
watch(isNewDraft, (isNew) => {
  const id = currentId.value;
  if (!isNew && id) syncQuery(id);
});

// ===== 搜索：纯前端过滤（名称 / 脚本 ID / 描述） =====
const searchQuery = ref("");

const visibleScripts = computed(() => {
  const q = searchQuery.value.trim().toLowerCase();
  if (!q) return scripts.value;
  return scripts.value.filter(
    (script) =>
      (script.name ?? "").toLowerCase().includes(q) ||
      script.id.toLowerCase().includes(q) ||
      (script.description ?? "").toLowerCase().includes(q),
  );
});

// ===== 行尾 ⋯ 菜单（对齐另两个面板）：同一时刻只开一个 =====
const openMenuId = ref("");

function toggleRowMenu(scriptId: string): void {
  openMenuId.value = openMenuId.value === scriptId ? "" : scriptId;
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

/** 全局监听只在菜单打开期间挂载 */
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

/**
 * 导入脚本文件：内容进编辑器交给自动保存落盘，并把 `?task=` 写上。
 *
 * 覆盖既有脚本时 `_isNew` 为 false，面板那个"落盘后补 query"的 watcher 不会触发
 * ——不在这里补一次，编辑器里改到一半刷新就会掉回列表态。
 */
async function onImportScript(): Promise<void> {
  const id = await importScript();
  if (id) syncQuery(id);
}

// ===== 编辑态派生 =====
/** 自动保存状态字（缺口优先，见 utils/autosave；新建未落盘时也不能说"改动自动保存"） */
const autosaveText = computed(() =>
  autosaveLabel(autosaveState.value, draftGapsNow.value, isNewDraft.value),
);

/** 未落盘的草稿不可运行（磁盘上还没有这个脚本） */
const canRun = computed(() => !editingTask.value?._isNew && !!currentId.value);

// ===== 二进制选项 =====
const binaryOptions = computed<SelectOption[]>(() => {
  const opts: SelectOption[] = [{ value: "", label: "Python (项目内解释器)" }];
  for (const b of availableBinaries.value) {
    opts.push({ value: b.path, label: `${b.name} (${b.path})` });
  }
  opts.push({ value: "__custom__", label: "自定义可执行文件 (.exe)" });
  return opts;
});

/** 列表「执行程序」列：空 = 项目内 Python，其余取文件名 */
function binaryLabel(path: string | undefined): { text: string; isPython: boolean } {
  const trimmed = (path ?? "").trim();
  return trimmed ? { text: getBinaryName(trimmed), isPython: false } : { text: "Python", isPython: true };
}

onMounted(async () => {
  await fetchScripts();
  void fetchAvailableBinaries();
  // 目录就绪后再消费 ?task=<id>：未就绪时不判定"任务不存在"（见 useTaskEditorQuery）
  resolve();
});
</script>

<template>
  <!-- ==================== 列表态 ==================== -->
  <div v-if="!isEditing" class="tsk-list-page">
    <div class="tsk-toolbar">
      <h2 class="tsk-title">自定义脚本</h2>
      <input
        v-model="searchQuery"
        class="tsk-search"
        type="text"
        placeholder="搜索名称 / 脚本 ID / 描述"
        aria-label="搜索脚本"
      />
      <button type="button" class="btn btn-sm" title="从文件导入脚本（.py / .sh / .bat / .cmd / .txt）" @click="onImportScript">
        <IconApp name="upload" class="icon-sm" />
        导入
      </button>
      <button type="button" class="btn btn-sm btn-primary" @click="onNewScript">
        <IconApp name="plus" class="icon-sm" />
        新建脚本
      </button>
    </div>

    <div class="card tsk-table-card">
      <table class="tsk-table">
        <colgroup>
          <col class="tsk-col-drag" />
          <col class="tsk-col-name" />
          <col class="tsk-col-id" />
          <col class="tsk-col-binary" />
          <col class="tsk-col-mtime" />
          <col class="tsk-col-actions" />
        </colgroup>
        <thead>
          <tr>
            <th class="tsk-drag-cell"></th>
            <th>名称</th>
            <th class="tsk-cell-id">脚本 ID</th>
            <th class="tsk-cell-binary">执行程序</th>
            <th class="tsk-cell-mtime">最近修改</th>
            <th class="tsk-actions">操作</th>
          </tr>
        </thead>
        <tbody>
          <tr v-if="!scripts.length">
            <td colspan="6">
              <div class="empty-state">
                <IconApp name="code" :stroke-width="1.5" />
                <span>暂无自定义脚本</span>
                <span class="empty-desc">
                  脚本用于定时执行的辅助动作（打卡、签到等），也可以作为方案「登录方式 · 自定义脚本」
                  的登录逻辑；支持 Python、Shell 或任意可执行程序
                </span>
                <div class="empty-actions">
                  <button type="button" class="btn btn-sm btn-primary" @click="onNewScript">
                    <IconApp name="plus" />新建脚本
                  </button>
                  <button type="button" class="btn btn-sm" @click="onImportScript">
                    <IconApp name="upload" class="icon-sm" />导入
                  </button>
                </div>
              </div>
            </td>
          </tr>
          <tr v-else-if="!visibleScripts.length">
            <td colspan="6">
              <div class="empty-state">
                <IconApp name="search" :stroke-width="1.5" />
                <span>没有匹配「{{ searchQuery }}」的脚本</span>
              </div>
            </td>
          </tr>
          <tr
            v-for="script in visibleScripts"
            :key="script.id"
            data-draggable-list
            @dragstart="drag.handleDragStart($event, dragIndex(script.id))"
            @dragover="drag.onDragOver($event, dragIndex(script.id))"
            @drop="drag.onDrop($event, dragIndex(script.id))"
            @dragend="drag.onDragEnd($event)"
            @click="openEditor(script.id)"
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
              <span class="tsk-name">{{ script.name || script.id }}</span>
              <span v-if="script.description" class="tsk-name-sub">{{ script.description }}</span>
            </td>
            <td class="tsk-cell-id tsk-cell-ellipsis"><span class="tsk-mono">{{ script.id }}</span></td>
            <td class="tsk-cell-binary">
              <span class="chip chip--dense" :class="{ 'chip--muted': binaryLabel(script.binary_path).isPython }">
                {{ binaryLabel(script.binary_path).text }}
              </span>
            </td>
            <td class="tsk-cell-mtime"><span class="tsk-mtime">{{ formatMtime(script.modified_at) }}</span></td>
            <td class="tsk-actions" @click.stop>
              <button type="button" class="btn btn-sm btn-icon-only" :title="`编辑：${script.name || script.id}`" @click="openEditor(script.id)">
                <IconApp name="pencil" class="icon-sm" />
              </button>
              <button
                type="button"
                class="btn btn-sm btn-icon-only"
                :title="`更多操作：${script.name || script.id}`"
                aria-haspopup="menu"
                :aria-expanded="openMenuId === script.id"
                @pointerdown.stop
                @click.stop="toggleRowMenu(script.id)"
              >
                <IconApp name="more-vertical" class="icon-sm" />
              </button>
              <div v-if="openMenuId === script.id" class="tsk-menu" role="menu" @pointerdown.stop>
                <button
                  type="button"
                  role="menuitem"
                  :disabled="runningIds.has(script.id)"
                  @click="runRowAction(() => runScript(script.id))"
                >{{ runningIds.has(script.id) ? '运行中…' : '立即运行' }}</button>
                <button type="button" role="menuitem" :disabled="exportingIds.has(script.id)" @click="runRowAction(() => exportScript(script.id))">导出脚本文件</button>
                <button type="button" role="menuitem" class="tsk-menu-danger" @click="runRowAction(() => deleteScript(script.id))">删除</button>
              </div>
            </td>
          </tr>
        </tbody>
      </table>
      <div v-if="scripts.length" class="tsk-foot">
        <span>共 {{ scripts.length }} 个脚本</span>
        <span v-if="searchQuery.trim()">· {{ visibleScripts.length }} 个匹配</span>
        <span class="tsk-spacer"></span>
        <span>拖拽行首调整顺序 · 点行进入编辑 · 改动自动保存</span>
      </div>
    </div>
  </div>

  <!-- ==================== 编辑态（二级页） ==================== -->
  <div v-else-if="editingTask" class="tsk-editor-page">
    <div class="tsk-crumb">
      <button type="button" class="tsk-crumb-back" @click="closeEditor">
        <IconApp name="chevron-down" class="icon-sm tsk-crumb-icon" />
        返回脚本
      </button>
      <span class="tsk-crumb-sep">/</span>
      <span class="tsk-crumb-here">{{ editingTask.name || editingTask.id || '新建脚本' }}</span>
    </div>

    <div class="tsk-editor-head">
      <h2 class="tsk-editor-title">{{ editingTask.name || editingTask.id || '新建脚本' }}</h2>
      <span :class="autosaveText.cls">{{ autosaveText.text }}</span>
      <span class="tsk-spacer"></span>
      <button
        v-if="canRun"
        type="button"
        class="btn btn-sm"
        :disabled="runningIds.has(currentId)"
        title="立即运行一次这个脚本"
        @click="runScript(currentId)"
      >
        <IconApp :name="runningIds.has(currentId) ? 'refresh' : 'play'" class="icon-sm" :class="{ spin: runningIds.has(currentId) }" />
        {{ runningIds.has(currentId) ? '运行中…' : '运行' }}
      </button>
      <button v-if="canRun" type="button" class="btn btn-sm" :disabled="exportingIds.has(currentId)" title="导出脚本文件" @click="exportScript(currentId)">
        <IconApp name="download" class="icon-sm" />
        导出
      </button>
      <button v-if="canRun" type="button" class="btn btn-sm btn-danger" title="删除脚本" @click="deleteScript(currentId)">
        <IconApp name="trash" class="icon-sm" />
        删除
      </button>
      <!-- 新建草稿还没落盘：此时没有"运行/删除"可言，只能先导出草稿或直接放弃它 -->
      <button v-if="!canRun" type="button" class="btn btn-sm" :disabled="exportingIds.has(currentId)" title="导出当前草稿（磁盘上还没有这个脚本）" @click="exportScript(currentId)">
        <IconApp name="download" class="icon-sm" />
        导出
      </button>
      <button v-if="!canRun" type="button" class="btn btn-sm btn-danger" title="放弃这个还没保存的新建脚本" @click="deleteScript(currentId)">
        <IconApp name="trash" class="icon-sm" />
        放弃
      </button>
    </div>

    <!-- 缺口提示：自动保存被缺口拦住时状态字已经改口，这里把"缺什么"说全 -->
    <div v-if="draftGapsNow.length" class="tsk-gaps">
      <IconApp name="alert-triangle" class="icon-sm" />
      <span>
        还缺 {{ draftGapsNow.join('、') }}，补齐前改动不会保存
        <template v-if="editingTask._isNew">（脚本 ID 就是文件名，也是「定时任务」引用它的值）</template>
      </span>
    </div>

    <div class="tsk-grid">
      <!-- 主列 -->
      <div class="tsk-main">
        <div class="card">
          <div class="card-header"><h3>基本信息</h3></div>
          <div class="card-body">
            <div class="form-row">
              <div class="form-group">
                <label for="script-id">脚本 ID</label>
                <input
                  id="script-id"
                  v-model.trim="editingTask.id"
                  type="text"
                  placeholder="my_script"
                  :disabled="!editingTask._isNew"
                  @blur="commitScriptId()"
                  @keyup.enter="commitScriptId()"
                />
                <span class="hint">
                  <!-- 允许字符写全：与 SCRIPT_ID_PATTERN / 后端 is_valid_task_id 同口径
                       （历史上这里写着"字母开头、只能用下划线"，比后端严，导致一类 ID 存不上） -->
                  <template v-if="editingTask._isNew">
                    1~64 位字母、数字、下划线或连字符；它同时是文件名，创建后不可修改。
                    打完名字按回车（或点输入框外）即创建，输入途中不会落盘
                  </template>
                  <template v-else>脚本文件名，创建后不可修改</template>
                </span>
              </div>
              <div class="form-group">
                <label for="script-name">名称</label>
                <input id="script-name" v-model="editingTask.name" type="text" placeholder="我的打卡脚本" />
              </div>
            </div>
            <div class="form-group">
              <label for="script-desc">描述</label>
              <input id="script-desc" v-model="editingTask.description" type="text" placeholder="脚本描述（可选）" />
              <span class="hint">名称与描述留空时按脚本 ID 显示</span>
            </div>
            <div class="form-group">
              <label for="script-binary">执行程序</label>
              <div class="binary-input-group">
                <CustomSelect v-model="editingTask.binary_path" :options="binaryOptions" @change="onBinarySelectChange" />
                <input
                  v-if="editingTask.binary_path === '__custom__'"
                  v-model="editingTask._customBinary"
                  type="text"
                  placeholder="输入可执行文件的完整路径（.exe）"
                  class="mt-2"
                />
              </div>
              <span class="hint" v-if="editingTask.binary_path && editingTask.binary_path !== '__custom__'">
                当前: {{ editingTask.binary_path }}
              </span>
              <span class="hint" v-else>
                仅支持 shell / bat / python / exe 四类；Python 使用项目内解释器，需要其他解释器请包一层 bat / shell
              </span>
            </div>
          </div>
        </div>

        <div class="card">
          <div class="card-header">
            <h3>脚本内容</h3>
            <div class="card-actions">
              <button type="button" class="btn btn-sm" title="用示例脚本覆盖当前内容" @click="loadScriptTemplate">
                加载示例模板
              </button>
            </div>
          </div>
          <div class="card-body">
            <div class="form-group">
              <textarea
                id="script-content"
                v-model="editingTask.content"
                rows="18"
                class="script-editor"
                placeholder="#!/usr/bin/env python3&#10;from urllib.request import urlopen&#10;&#10;..."
              ></textarea>
              <span class="hint">
                脚本可直接硬编码账号密码等参数；stdout 与 stderr 作为执行结果就地显示（不进日志页），按退出码判定成败（0 = 成功）
              </span>
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
              :disabled="!canRun || runningIds.has(currentId)"
              :title="canRun ? '立即运行一次' : '先补上脚本 ID，脚本落盘后才能运行'"
              @click="runScript(currentId)"
            >
              <IconApp :name="runningIds.has(currentId) ? 'refresh' : 'play'" class="icon-sm" :class="{ spin: runningIds.has(currentId) }" />
              {{ runningIds.has(currentId) ? '运行中…' : '立即运行' }}
            </button>
            <dl class="tsk-kv">
              <dt>调度</dt>
              <dd><span class="tsk-side-hint">在「定时任务」里引用本脚本才会自动执行</span></dd>
              <dt>输出</dt>
              <dd><span class="tsk-side-hint">stdout 与 stderr 在「立即运行」的结果里就地显示（不进日志页）</span></dd>
            </dl>
            <!-- 最近一次「立即运行」的结果：脚本输出不进日志页，不留下来就无处可查 -->
            <div
              v-if="lastRunResult && lastRunResult.id === currentId"
              class="history-item"
              :class="lastRunResult.result.success ? 'success' : 'failed'"
            >
              <div class="history-status">
                <IconApp :name="lastRunResult.result.success ? 'check-circle' : 'x-circle'" />
              </div>
              <div class="history-info">
                <div class="history-row">
                  <span class="history-time">退出码 {{ lastRunResult.result.exit_code }}</span>
                  <span class="history-duration">{{ lastRunResult.result.duration_ms }}ms</span>
                </div>
                <pre class="history-output">{{ lastRunResult.result.output || '（无输出）' }}</pre>
              </div>
            </div>
          </div>
        </div>

        <div class="card">
          <div class="card-header"><h3>快速上手</h3></div>
          <div class="card-body tsk-side-body">
            <p class="tsk-side-hint">
              脚本既能做辅助动作（打卡、签到等），也能当登录脚本：到「方案」把登录方式设为
              <strong>自定义脚本</strong>并选中本脚本，凭据以 <code>CAMPUS_*</code> 环境变量传入。
            </p>
            <p class="tsk-side-hint">
              登录脚本按<strong>退出码</strong>判定成败：0 = 本次尝试成功，程序随后仍做一次网络验证；
              非 0 = 按方案的重试策略重发（次数见「设置 · 检测」的「最大重试次数」）。
            </p>
            <p class="tsk-side-hint">
              从文件导入：点列表页的「导入」，内容直接进编辑器并自动保存；覆盖同名脚本前会先确认。
            </p>
            <p class="tsk-side-hint">默认超时 60 秒，退出码 0 视为成功；stderr 输出不影响判定。</p>
            <p class="tsk-side-hint">脚本内容上限 100 KB。</p>
          </div>
        </div>
      </div>
    </div>
  </div>
</template>

<style scoped>
/* 面板专属样式：共享版式（列表页 / 表格 / 菜单 / 二级页 / 状态字 / 缺口条）在
   styles/pages/tasks.css 的 `tsk-*` 组里。
   脚本正文编辑器（`.script-editor`）、执行程序分组（`.binary-input-group`）与
   `.mt-2` 由 styles/pages/scripts.css 全局提供——曾在本组件 scoped 里再抄一份，
   两份 min-height / 焦点色取值不同（380 vs 400、accent vs purple），实际以加载顺序
   决定谁生效，属"改一处不生效"的典型，故删掉这里的副本。 */

/* 解释器徽标的视觉已收敛到全局 `.chip--dense` / `.chip--muted`（components/chip.css）：
   Python 是默认项故弱化显示，其余（自定义程序）按默认主色提示。 */

/* 描述并入名称格后本表没有独立的弹性列，让名称列吃剩余宽度
   （fixed 布局下 auto 列分得剩余空间） */
.tsk-table col.tsk-col-name {
  width: auto;
}
</style>
