<script setup lang="ts">
/** 直连任务面板：直连任务的增删改查、仓库导入与一次性的测试请求。
 *
 * 由「任务」页容器以子路由渲染（/tasks/http）。版式是**主从两栏**（方案 A，宽屏）：
 * 左列固定 296px 的列表卡（搜索 + 行 + 行内 ⋯ 菜单），右列是编辑器卡 / 帮助卡。
 * 这样「边看列表边改配置」不必上下翻一条长页，窄屏（≤1100px）才堆叠为单列。
 *
 * 与浏览器任务面板的差异：编辑器里没有 JSON 文本框（字段由 HttpTaskFields 平铺呈现），
 * 也没有「调试」按钮——直连任务不经 Python Worker，无法单步执行，验证路径是发一次
 * 测试请求（凭据由宿主传入，见 useHttpTaskTest）。
 *
 * 列表与脚本/浏览器任务同源（useTaskDirectory 单次拉取后按 task_type 过滤）。
 */
import IconApp from "@/components/common/IconApp.vue";
import FieldHelp from "@/components/common/FieldHelp.vue";
import HttpTaskFields from "@/components/common/HttpTaskFields.vue";
import HttpTestResult from "@/components/common/HttpTestResult.vue";
import HttpLoginWizard from "@/components/common/HttpLoginWizard.vue";
import { computed, onBeforeUnmount, onMounted, ref, watch } from "vue";
import { useRoute } from "vue-router";
import { useHttpTasks } from "@/composables/useHttpTasks";
import { useHttpTaskTest } from "@/composables/useHttpTaskTest";
import { useProfiles } from "@/composables/useProfiles";
import { useRepoImport } from "@/composables/useRepoImport";
import { useToast } from "@/composables/useToast";
import { HTTP_CRYPTO_BUILTINS, HTTP_TEMPLATE_PLACEHOLDERS } from "@/utils/loginChannel";
import { httpTaskPayload } from "@/utils/httpTask";
import {
  buildHttpTaskBindingIndex,
  buildHttpTaskRows,
  filterHttpTaskRows,
  isBindingIndexReady,
} from "@/utils/httpTaskList";
import { TASK_REPO_URL } from "@/utils/constants";

const {
  httpTasks,
  httpTaskDraft,
  httpTaskSaving,
  duplicatingIds,
  exportingIds,
  fetchHttpTasks,
  saveHttpTask,
  deleteHttpTask,
  showNewHttpTaskDraft,
  showHttpTaskEditor,
  closeHttpTaskEditor,
  duplicateHttpTask,
  exportHttpTask,
  importHttpTask,
} = useHttpTasks();

const repo = useRepoImport();
const { toastOnly } = useToast();
const { profiles } = useProfiles();
const { running, result: testResult, runHttpTaskTest, clearTestResult } = useHttpTaskTest();

/**
 * 测试凭据：任务里不含账号密码（凭据属于方案），而任务页没有方案上下文，
 * 故本页要测一次就得手填一次。
 *
 * 刻意用组件本地 ref 而非草稿字段：它不属于任务配置，写进草稿会被保存到
 * `<base>/tasks/http/<id>.json`（同一门户的不同账号共用一份任务，凭据落盘就白拆了），
 * 也会让 dirty 比对永远为真（"改了账号"被当成"改了任务"）。
 */
const testUsername = ref("");
const testPassword = ref("");

/** 表单控件 id 前缀：与 HttpTaskFields 同页共存，避免 label/for 撞车 */
const uid = `http-tasks-${Math.random().toString(36).slice(2, 8)}`;

const route = useRoute();

/** 直连配置向导的开关：向导编辑的是同一份草稿（不持有第二份状态），故只是显隐控制 */
const showWizard = ref(false);

/** 搜索关键字：纯前端过滤（见 visibleRows），不发任何请求 */
const searchQuery = ref("");

/**
 * 方案绑定索引：任务 id → 引用它的方案名（列表行上「被哪些方案绑定」的数据源）。
 *
 * 数据来自 `useProfiles().profiles`——`useUi.init` 已经拉过方案列表，这里只是换个
 * 方向聚合（方案存的是 `active_http_task`），**不新增请求**。
 */
const bindingIndex = computed(() => buildHttpTaskBindingIndex(profiles.value));

/**
 * 绑定索引是否可用。
 *
 * `profiles` 在方案列表拉取完成前是空表，此时**不能**把结果当成"没有方案引用它"：
 * 那会把一条正在被使用的任务标成灰「未绑定」，诱导用户删掉它。未就绪时整行不渲染
 * 绑定区（也不渲染"未绑定"），等列表到位后自然出现。
 */
const bindingsReady = computed(() => isBindingIndexReady(profiles.value));

/** 列表行：名称 / 方法 / 地址（均来自任务摘要自带字段）+ 绑定方案 */
const rows = computed(() => buildHttpTaskRows(httpTasks.value, bindingIndex.value));

/** 过滤后的行：匹配名称 / 任务 ID / 请求地址 */
const visibleRows = computed(() => filterHttpTaskRows(rows.value, searchQuery.value));

/**
 * 当前编辑器打开的是哪条**已保存**任务（用于行选中态）。
 * 新建草稿还没有对应行，故返回空串——此时列表里没有任何行是选中态。
 */
const activeRowId = computed(() => {
  const draft = httpTaskDraft.value;
  return draft && !draft._isNew ? draft.id : "";
});

// ===== 行尾 ⋯ 菜单 =====
// 复制 / 导出 / 删除收进菜单：列表行只留"这是什么"（名称、方法、地址、绑定），
// 操作不再把行挤成「名称 + 四个图标」。同一时刻只开一个菜单。
const openMenuId = ref("");

function toggleRowMenu(taskId: string): void {
  openMenuId.value = openMenuId.value === taskId ? "" : taskId;
}

function closeRowMenu(): void {
  openMenuId.value = "";
}

/** 执行菜单动作：先收起菜单（动作会弹确认框或发请求，菜单留着会挡住列表） */
function runRowAction(action: () => unknown): void {
  closeRowMenu();
  void action();
}

/**
 * 点行 → 打开该任务的编辑器。
 *
 * 点当前已打开的那一行直接返回：`showHttpTaskEditor` 会先做 dirty 确认，用户只是
 * 又点了一下同一行就被问「放弃未保存的修改？」，等于诱导他丢掉刚改的内容。
 */
async function openRow(taskId: string): Promise<void> {
  if (activeRowId.value === taskId) return;
  await showHttpTaskEditor(taskId);
}

function onDocumentPointerDown(): void {
  closeRowMenu();
}

function onDocumentKeydown(event: KeyboardEvent): void {
  if (event.key === "Escape") closeRowMenu();
}

/**
 * 全局监听只在菜单打开期间挂载：本页行数多，常驻监听白占开销（同 FieldHelp 的做法）。
 * 菜单本体上挂 `@pointerdown.stop`，否则点菜单项会先被这里关掉菜单、连点击都收不到。
 */
watch(openMenuId, (open) => {
  if (open) {
    document.addEventListener("pointerdown", onDocumentPointerDown);
    document.addEventListener("keydown", onDocumentKeydown);
  } else {
    document.removeEventListener("pointerdown", onDocumentPointerDown);
    document.removeEventListener("keydown", onDocumentKeydown);
  }
});

// 卸载时兜底摘除，避免路由离开后监听留在 document 上
onBeforeUnmount(() => {
  document.removeEventListener("pointerdown", onDocumentPointerDown);
  document.removeEventListener("keydown", onDocumentKeydown);
});

/**
 * 归属提示（原先标题下方那条整行蓝色横幅）。
 *
 * 只支持纯文本（FieldHelp 的气泡按 data-tip 纯文本渲染、不支持跳转链接），故这里
 * 不渲染「方案」链接——「方案」就在左侧主侧边栏，一句指路足够，也换掉了整屏横幅。
 */
const NOTICE_HELP =
  "直连请求要生效，需在侧边栏「方案」里把登录方式设为「直连请求」并选中一个任务。\n\n" +
  "本页只负责编辑与测试：任务本身不含账号密码，凭据与匹配规则留在方案里。";

/**
 * 进入本 Tab 时拉列表，并在带 `?task=<id>` 时自动打开该任务的编辑器。
 *
 * 参数来自方案编辑器的「配置直连任务」入口（方案只引用一个任务 id，字段编辑在
 * 这里）。消费时机有两个约束：必须在目录拉取之后（否则"有对应任务"无从判定，
 * 会被误判成不存在而白跑一趟跳转）；只在本函数里跑一次（参数不清理，重复进入
 * 同一参数不会再覆盖用户当前正在编辑的内容）。
 */
onMounted(async () => {
  await fetchHttpTasks();
  const requested = typeof route.query.task === "string" ? route.query.task.trim() : "";
  if (requested && httpTasks.value.some((t) => t.id === requested)) {
    await showHttpTaskEditor(requested);
  }
});

/**
 * 草稿任何变化（改字段 / 换任务 / 关编辑器）都清掉上一次测试结果。
 *
 * 结果卡上写着具体请求地址与判定结论，字段改过之后它就不再代表当前配置，
 * 留着会让人按过期结论排查。测试账号密码不在草稿里，改动它们不清结果。
 */
watch(httpTaskDraft, () => clearTestResult(), { deep: true });

/**
 * 草稿被换掉或关掉时收起向导。
 *
 * 向导编辑的是**某一份草稿**（props.draft 就是它），跟着新草稿走会让人以为还在改
 * 原来那个任务；保存/取消后草稿已消失，向导还开着则写的是无处可去的对象。
 * 只监听身份变化（非 deep）：字段编辑由上面的 deep watch 负责。
 */
watch(
  () => httpTaskDraft.value,
  () => {
    showWizard.value = false;
  },
);

/** 发送一次测试请求：用的是编辑器里的**当前草稿**（含未保存改动），不是已保存的任务 */
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
    // 后端没有请求地址只会返回 400，提前拦下省一次往返
    toastOnly(false, "请先填写请求地址");
    return;
  }
  await runHttpTaskTest({
    task: httpTaskPayload(draft),
    username: testUsername.value,
    password: testPassword.value,
    fetch_page: true,
  });
}

function closeEditor(): void {
  void closeHttpTaskEditor();
}
</script>

<template>
  <div class="http-tasks-panel">
    <!-- 标题行：标题 + 工具区（提示条已弱化为右侧 ? 气泡，内容保留） -->
    <div class="http-tasks-toolbar">
      <h2 class="http-tasks-title">直连任务</h2>
      <div class="http-tasks-tools">
        <FieldHelp :text="NOTICE_HELP" wide />
        <button class="btn btn-sm" @click="importHttpTask()" title="从文件导入直连任务">
          <IconApp name="upload" class="icon-sm" />
          导入
        </button>
        <button class="btn btn-sm" @click="repo.showRepoImport('http')" title="从云端仓库导入直连任务">
          <IconApp name="globe-grid" class="icon-sm" />
          仓库导入
        </button>
        <!-- 指向**任务**仓库：这一入口的用途是把适配好的登录任务分享给社区 -->
        <a
          :href="TASK_REPO_URL"
          target="_blank"
          rel="noopener"
          class="btn btn-sm btn-ghost"
          title="把你的直连任务分享到任务仓库，供他人一键导入"
        >
          <IconApp name="share-2" class="icon-sm" />
          分享适配
        </a>
        <button class="btn btn-sm btn-primary" @click="showNewHttpTaskDraft()">
          <IconApp name="plus" class="icon-sm" />
          新建直连任务
        </button>
      </div>
    </div>

    <!-- 空态：一条任务都没有时不再摆「296px 空列表列 + 1000px 散文说明卡」——
         主次颠倒（真功能缩在一角、说明书占满屏），且「新建 / 仓库导入」在页头、
         空列表格、提示条三处重复。改为整页一张居中引导卡：三步讲清怎么开始，
         两个入口收在卡里，字段词表折进卡尾（默认收起）。
         判据必须带 `!httpTaskDraft`：点「新建直连任务」时还没有任何已保存任务，
         若只看条数，引导卡会把刚打开的编辑器顶掉（新建第一步就卡死）。 -->
    <div v-if="!httpTasks.length && !httpTaskDraft" class="card http-guide-card">
      <div class="card-header"><h2>还没有直连任务</h2></div>
      <div class="card-body">
        <p class="http-guide-lead">
          直连任务直接向校园网网关发登录请求，<b>不开浏览器、不需要 Python 与 Playwright</b>，
          适合门户提供简单 GET/POST 接口的学校。
        </p>

        <ol class="http-guide-steps">
          <li>
            <strong>先拿到一条任务</strong>
            <span>任务仓库里有别人适配好的校园网任务，一键导入即可；也可以点「新建直连任务」从空白开始。</span>
          </li>
          <li>
            <strong>用配置向导填「请求的形状」</strong>
            <span>地址、请求头、请求内容、成败判定都在编辑器里；向导分步带你填，最后一步当场发一次测试请求。</span>
          </li>
          <li>
            <strong>回「方案」把它绑上去</strong>
            <span>方案编辑器里选「直连请求」并选中这条任务（<code>active_http_task</code>），登录时才真正用它。</span>
          </li>
        </ol>

        <div class="http-guide-actions">
          <button class="btn btn-primary" type="button" @click="showNewHttpTaskDraft()">
            <IconApp name="plus" class="icon-sm" />
            新建直连任务
          </button>
          <button class="btn" type="button" @click="repo.showRepoImport('http')">
            <IconApp name="globe-grid" class="icon-sm" />
            从仓库导入
          </button>
        </div>

        <details class="http-guide-more">
          <summary>字段与函数速查</summary>
          <div class="http-guide-more-body">
            <h4>占位符速查</h4>
            <div class="http-chip-row">
              <code v-for="ph in HTTP_TEMPLATE_PLACEHOLDERS" :key="ph" class="http-chip">{{ ph }}</code>
            </div>
            <h4>凭据变换脚本可用函数</h4>
            <div class="http-chip-row">
              <code v-for="fn in HTTP_CRYPTO_BUILTINS" :key="fn" class="http-chip http-chip--fn">{{ fn }}</code>
            </div>
            <p class="hint">每个字段怎么填、为什么这么填，见编辑器内该字段旁的 <code>?</code>。</p>
          </div>
        </details>
      </div>
    </div>

    <div v-else class="http-tasks-split">
      <!-- ===== 左列：列表卡 ===== -->
      <div class="card http-list-card">
        <div class="http-list-head">
          <div class="http-list-head-row">
            <!-- 页标题已经是「直连任务」，这里只报数量，不再重复同一个词 -->
            <h3 v-if="httpTasks.length">共 {{ httpTasks.length }} 个</h3>
            <!-- 一条都没有还能走到这里，只可能是"新建草稿已打开"（空态由引导卡接管，
                 对应列表里没有可搜索的东西，故搜索框也不渲染） -->
            <h3 v-else>暂无已保存任务</h3>
            <span v-if="searchQuery.trim()" class="http-list-count">
              {{ visibleRows.length }} 个匹配
            </span>
          </div>
          <input
            v-if="httpTasks.length"
            v-model="searchQuery"
            class="http-list-search"
            type="text"
            placeholder="搜索名称 / 任务 ID / 请求地址"
            aria-label="搜索直连任务"
          />
        </div>

        <div class="http-list-body">
          <!-- 新建草稿 + 空列表：说明"保存后才会出现在这里"，避免读成「没有匹配「」的任务」 -->
          <p v-if="!httpTasks.length" class="http-list-none">
            这条任务还没保存。保存后它会出现在这里，可以再点开继续编辑。
          </p>

          <!-- 有任务但没匹配上：与"一个任务都没有"区分开，否则像任务丢了 -->
          <p v-else-if="!visibleRows.length" class="http-list-none">
            没有匹配「{{ searchQuery }}」的任务
          </p>

          <ul v-else class="http-list">
            <li
              v-for="row in visibleRows"
              :key="row.id"
              class="http-row"
              :class="{ 'http-row--active': activeRowId === row.id }"
            >
              <button type="button" class="http-row-main" @click="openRow(row.id)">
                <span class="http-row-name">{{ row.name || row.id }}</span>
                <span class="http-row-sub">
                  <span v-if="row.method" class="http-method">{{ row.method }}</span>
                  <!-- 地址缺失（详情还没读到）时回退显示任务 ID：这一行永远是"这是什么"的抓手 -->
                  <span class="http-row-url">{{ row.url || row.id }}</span>
                </span>
                <span v-if="bindingsReady" class="http-row-bind">
                  <template v-if="row.boundProfiles.length">
                    <span
                      v-for="name in row.boundProfiles"
                      :key="name"
                      class="badge badge--sm badge--success"
                      :title="`方案「${name}」的直连登录指向本任务`"
                    >{{ name }}</span>
                  </template>
                  <span v-else class="badge badge--sm http-badge--muted" title="没有方案把直连登录指向本任务，删除它不会影响现有方案">
                    未绑定
                  </span>
                </span>
              </button>

              <button
                type="button"
                class="btn btn-sm btn-icon-only http-row-menu-btn"
                :title="`更多操作：${row.name || row.id}`"
                aria-haspopup="menu"
                :aria-expanded="openMenuId === row.id"
                @pointerdown.stop
                @click.stop="toggleRowMenu(row.id)"
              >
                <IconApp name="more-vertical" class="icon-sm" />
              </button>

              <!-- 菜单：pointerdown.stop 让点击不被"点外面关菜单"的全局监听吃掉 -->
              <div v-if="openMenuId === row.id" class="http-row-menu" role="menu" @pointerdown.stop>
                <button
                  type="button"
                  role="menuitem"
                  :disabled="duplicatingIds.has(row.id)"
                  @click="runRowAction(() => duplicateHttpTask(row.id))"
                >复制为新任务</button>
                <button
                  type="button"
                  role="menuitem"
                  :disabled="exportingIds.has(row.id)"
                  @click="runRowAction(() => exportHttpTask(row.id))"
                >导出 JSON</button>
                <button
                  type="button"
                  role="menuitem"
                  class="http-row-menu-danger"
                  @click="runRowAction(() => deleteHttpTask(row.id))"
                >删除</button>
              </div>
            </li>
          </ul>
        </div>
      </div>

      <!-- ===== 右列：编辑器卡 / 帮助卡 ===== -->
      <div class="http-editor-col">
        <div v-if="httpTaskDraft" class="card http-editor-card">
          <div class="card-header">
            <!-- 名称与状态徽标同组：卡头是 space-between 布局，若把徽标平铺成兄弟节点，
                 卡头换行时它会被推到首行右端、离开名称（实测编辑列 422px 时） -->
            <div class="http-editor-heading">
              <h2>{{ httpTaskDraft.name || httpTaskDraft.id || '新建直连任务' }}</h2>
              <!-- 状态徽标：新建（尚未落盘，可改 ID）与已保存（ID 锁定）是两种编辑语义 -->
              <span
                class="badge badge--sm"
                :class="httpTaskDraft._isNew ? 'badge--info' : 'badge--success'"
              >{{ httpTaskDraft._isNew ? '新建' : '已保存' }}</span>
            </div>
            <div class="card-actions">
              <!-- 向导与下方字段区编辑同一份草稿（不持有第二份状态），只是把因果链分步呈现，
                   首次配置者不至于只填了地址就去点测试 -->
              <button
                type="button"
                class="http-wizard-link"
                @click="showWizard = true"
                title="分步引导填写请求参数，并在最后一步当场发一次测试请求"
              >
                <IconApp name="sparkles" class="icon-sm" />
                配置向导
              </button>
              <button type="button" class="btn btn-sm" :disabled="running" @click="sendTestRequest">
                <IconApp :name="running ? 'refresh' : 'play'" class="icon-sm" :class="{ spin: running }" />
                {{ running ? '正在发送…' : '发送测试请求' }}
              </button>
              <button
                type="button"
                class="btn btn-sm btn-primary"
                :disabled="httpTaskSaving"
                @click="saveHttpTask()"
              >保存</button>
              <button
                type="button"
                class="btn btn-icon-only"
                @click="closeEditor"
                title="关闭编辑器（有未保存改动时会先确认）"
              >
                <IconApp name="close" />
              </button>
            </div>
          </div>

          <div class="card-body">
            <div class="form-row">
              <div class="form-group">
                <div class="field-label-row">
                  <label :for="`${uid}-id`">任务ID</label>
                  <FieldHelp text="落盘文件名（<base>/tasks/http/<id>.json），也是方案里 active_http_task 引用的值。已保存的任务不能改 ID——改 ID 等于换一个任务，请用「复制」。" />
                </div>
                <input :id="`${uid}-id`" v-model.trim="httpTaskDraft.id" type="text" placeholder="dorm_portal"
                  :disabled="!httpTaskDraft._isNew" />
                <span class="hint">1~64 位字母、数字、下划线或连字符</span>
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

            <!-- 请求形状（地址/请求头/成败判定/变换脚本）：与直连配置向导共用同一组件，
                 字段编辑只有这一处入口，避免两处改同一份配置 -->
            <HttpTaskFields :model="httpTaskDraft" />

            <!-- 测试区：任务里不含凭据（凭据属于方案），本页没有方案上下文，故手填一次。
                 发送按钮放在标题行（与保存同排），这里只留输入与结果——凭据填完不必再找按钮。 -->
            <section class="http-test-area">
              <div class="http-test-head">
                <strong>发送一次测试请求</strong>
                <FieldHelp
                  text="这里只用来验证「请求形状」对不对：任务本身不含账号密码（凭据属于方案），所以测试时要手填一次。填写的凭据仅用于本次请求，不会写进任务文件。\n\n填好后点标题行右侧的「发送测试请求」。"
                  wide
                />
              </div>
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
              <p class="hint">测试用的是编辑器里的当前内容（含未保存的改动），点标题行的「发送测试请求」发送。</p>
              <HttpTestResult v-if="testResult" :result="testResult" />
            </section>
          </div>
        </div>

        <!-- 未打开编辑器时的右列：只留「照抄用」的词表 + 一行指路。
             原先这里是一整屏散文说明书（5 段、单列 70 字/行），空态下还抢走了真功能的位置；
             散文里的信息已分别落到页头 ? 气泡（生效方式与归属）、引导卡三步、以及下面这行指路。 -->
        <div v-else class="card http-ref-card">
          <div class="card-header"><h2>字段速查</h2></div>
          <div class="card-body">
            <p class="http-ref-lead">
              点左列任一条目继续编辑；每个字段怎么填、为什么这么填，见编辑器内该字段旁的 <code>?</code>。
            </p>
            <h4>占位符速查</h4>
            <div class="http-chip-row">
              <code v-for="ph in HTTP_TEMPLATE_PLACEHOLDERS" :key="ph" class="http-chip">{{ ph }}</code>
            </div>
            <h4>凭据变换脚本可用函数</h4>
            <div class="http-chip-row">
              <code v-for="fn in HTTP_CRYPTO_BUILTINS" :key="fn" class="http-chip http-chip--fn">{{ fn }}</code>
            </div>
          </div>
        </div>
      </div>
    </div>

    <!-- 直连配置向导：内部走 Modal（挂到 body），故放在组件末尾不影响两栏布局；
         与测试区共用同一对凭据 ref（向导里没有凭据输入控件，必须由宿主传入） -->
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
/* ===== 标题行 ===== */
.http-tasks-panel {
  display: flex;
  flex-direction: column;
  gap: var(--space-md);
}

.http-tasks-toolbar {
  display: flex;
  align-items: center;
  flex-wrap: wrap;
  gap: var(--space-sm);
}

.http-tasks-title {
  /* 标题优先保持可读：宁可让工具区换行，也不把标题压成一列单字 */
  min-width: fit-content;
  margin-right: auto;
  font-size: var(--text-lg);
  font-weight: 600;
  color: var(--text-primary);
}

.http-tasks-tools {
  display: flex;
  align-items: center;
  flex-wrap: wrap;
  gap: var(--space-sm);
}

/* ===== 主从两栏 ===== */
.http-tasks-split {
  display: grid;
  /* 左列固定 296px：行内是「名称 + 方法 chip + 地址摘要 + 绑定 pills」，296px 刚好放下
     这三行且不挤压右列字段区；右列 minmax(0,1fr) 让长地址/代码行自己省略，
     不把列撑破视口（grid 子项默认 min-width:auto 会被最长行撑开） */
  grid-template-columns: 296px minmax(0, 1fr);
  gap: var(--space-md);
  /* 按内容高对齐：列表短、编辑器长时，列表卡不该被拉到与编辑器等高（留一大块空框） */
  align-items: start;
}

/* 窄屏堆叠：列表在上、编辑器在下（主从关系靠选中态与位置延续，不做抽屉） */
@media (max-width: 1100px) {
  .http-tasks-split {
    grid-template-columns: minmax(0, 1fr);
  }
}

/* ===== 左列：列表卡 ===== */
.http-list-card {
  display: flex;
  flex-direction: column;
  /* 不用 .card-header/.card-body：列表卡的头是"标题行 + 搜索框"两行，且体需要更紧的内边距
     （296px 宽的列里 20px 内边距会把行内容压到 250px 以下） */
}

.http-list-head {
  display: flex;
  flex-direction: column;
  gap: 10px;
  padding: 12px var(--space-sm) 10px;
  border-bottom: 1px solid var(--border);
}

.http-list-head-row {
  display: flex;
  align-items: baseline;
  gap: var(--space-sm);
}

.http-list-head-row h3 {
  font-size: var(--text-base);
  font-weight: 600;
  color: var(--text-primary);
}

.http-list-count {
  color: var(--text-muted);
  font-size: var(--text-sm);
}

.http-list-search {
  width: 100%;
  padding: 7px 10px;
  border: 1px solid var(--border);
  border-radius: var(--radius-sm);
  background: var(--bg-glass-light);
  color: var(--text-primary);
  font-size: var(--text-md);
  transition: border-color var(--dur-base) var(--ease-out), background var(--dur-base) var(--ease-out);
}

.http-list-search::placeholder {
  color: var(--text-muted);
}

.http-list-search:hover {
  background: var(--bg-glass);
}

.http-list-search:focus-visible {
  outline: none;
  border-color: var(--accent);
  box-shadow: 0 0 0 3px rgba(var(--accent-rgb), 0.12);
}

.http-list-body {
  padding: var(--space-sm);
}

/* ===== 空态：整页引导卡 ===== */
/* 居中限宽 900px：这是一张"怎么开始"的说明卡，铺满 1976px 会让每行中文拉到百余字。
   居中不是"右边空一块"——两侧对称，且页头操作行仍是满宽的。 */
.http-guide-card {
  /* width:100% 必需：panel 是 flex 列，margin:auto 会优先于 stretch —— 只写 max-width
     时卡片宽度由内容撑（折叠态 790px，展开词表后变 900px，开合时会跳一下） */
  width: 100%;
  max-width: 900px;
  margin: 0 auto;
}

.http-guide-lead {
  color: var(--text-secondary);
  font-size: var(--text-md);
  line-height: 1.7;
}

/* 三步：序号圆标 + 一行标题 + 一行说明（步骤间留白，不做成一坨） */
.http-guide-steps {
  display: flex;
  flex-direction: column;
  gap: var(--space-md);
  margin-top: var(--space-lg);
  padding-left: 0;
  list-style: none;
  counter-reset: http-guide;
}

.http-guide-steps li {
  position: relative;
  display: flex;
  flex-direction: column;
  gap: 4px;
  padding-left: 38px;
  color: var(--text-secondary);
  font-size: var(--text-md);
  line-height: 1.7;
}

.http-guide-steps li::before {
  counter-increment: http-guide;
  content: counter(http-guide);
  position: absolute;
  top: 0;
  left: 0;
  display: flex;
  align-items: center;
  justify-content: center;
  width: 24px;
  height: 24px;
  border-radius: var(--radius-full);
  background: rgba(var(--accent-rgb), 0.12);
  color: var(--accent);
  font-size: var(--text-sm);
  font-weight: 700;
}

.http-guide-steps strong {
  color: var(--text-primary);
  font-weight: 600;
}

.http-guide-actions {
  display: flex;
  flex-wrap: wrap;
  gap: var(--space-sm);
  margin-top: var(--space-lg);
}

/* 词表折进卡尾、默认收起：空态不该又是一屏字 */
.http-guide-more {
  margin-top: var(--space-lg);
  padding-top: var(--space-md);
  border-top: 1px solid var(--border);
}

.http-guide-more > summary {
  color: var(--text-secondary);
  font-size: var(--text-md);
  font-weight: 600;
  cursor: pointer;
}

.http-guide-more > summary:hover {
  color: var(--text-primary);
}

.http-guide-more-body {
  display: flex;
  flex-direction: column;
  gap: 6px;
  margin-top: var(--space-sm);
}

.http-guide-more-body h4 {
  margin-top: var(--space-sm);
}

/* 搜索无结果：与"一个都没有"用不同文案，避免看起来像任务丢了 */
.http-list-none {
  padding: 12px 6px;
  color: var(--text-muted);
  font-size: var(--text-md);
  line-height: 1.6;
}

.http-list {
  display: flex;
  flex-direction: column;
  gap: 2px;
  list-style: none;
}

.http-row {
  position: relative;
  display: flex;
  align-items: flex-start;
  border-radius: var(--radius-md);
  transition: background var(--dur-base) var(--ease-out), box-shadow var(--dur-base) var(--ease-out);
}

.http-row:hover {
  background: var(--bg-hover);
}

/* 选中行：浅底 + 描边（主从版式里"当前编辑的是哪条"必须一眼可见）。
   悬停时**不换底**——否则鼠标移上去"当前编辑的是哪条"就消失了；
   悬停反馈改由加深描边承担 */
.http-row--active,
.http-row--active:hover {
  background: rgba(var(--accent-rgb), 0.08);
  box-shadow: inset 0 0 0 1px rgba(var(--accent-rgb), 0.25);
}

.http-row--active:hover {
  box-shadow: inset 0 0 0 1px rgba(var(--accent-rgb), 0.45);
}

.http-row-main {
  flex: 1;
  min-width: 0;
  display: flex;
  flex-direction: column;
  gap: var(--space-xs);
  padding: 9px 10px;
  border: none;
  border-radius: var(--radius-md);
  background: transparent;
  color: inherit;
  font: inherit;
  text-align: left;
  cursor: pointer;
}

.http-row-name {
  font-size: var(--text-md);
  font-weight: 600;
  color: var(--text-primary);
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.http-row-sub {
  display: flex;
  align-items: center;
  gap: 6px;
  min-width: 0;
}

/* 方法 chip：等宽字，与地址摘要同属"请求形状"的标注 */
.http-method {
  flex-shrink: 0;
  padding: 1px 5px;
  border: 1px solid var(--border);
  border-radius: var(--radius-xs);
  background: var(--bg-glass);
  color: var(--text-secondary);
  font-family: var(--font-mono);
  font-size: var(--text-2xs);
  font-weight: 700;
}

.http-row-url {
  min-width: 0;
  color: var(--text-muted);
  font-family: var(--font-mono);
  font-size: var(--text-xs);
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.http-row-bind {
  display: flex;
  flex-wrap: wrap;
  gap: var(--space-xs);
}

/* 未绑定：中性灰。语义是"可以安全删掉"，不该用告警色吓人 */
.http-badge--muted {
  background: rgba(var(--slate-rgb), 0.12);
  color: var(--text-muted);
}

.http-row-menu-btn {
  flex-shrink: 0;
  margin: 4px 4px 0 0;
  color: var(--text-muted);
}

.http-row-menu-btn:hover:not(:disabled) {
  color: var(--text-primary);
}

.http-row-menu {
  position: absolute;
  top: 30px;
  right: 4px;
  z-index: var(--z-dropdown);
  display: flex;
  flex-direction: column;
  min-width: 128px;
  padding: var(--space-xs);
  border: 1px solid var(--border);
  border-radius: var(--radius-md);
  background: var(--bg-modal);
  box-shadow: var(--shadow-float);
}

.http-row-menu button {
  padding: 6px 10px;
  border: none;
  border-radius: var(--radius-sm);
  background: transparent;
  color: var(--text-primary);
  font-size: var(--text-md);
  text-align: left;
  cursor: pointer;
  transition: background var(--dur-base) var(--ease-out);
}

.http-row-menu button:hover:not(:disabled) {
  background: var(--bg-hover);
}

.http-row-menu button:disabled {
  color: var(--text-muted);
  cursor: default;
}

.http-row-menu .http-row-menu-danger {
  color: var(--error);
}

/* ===== 右列：编辑器 / 帮助 ===== */
.http-editor-col {
  /* min-width: 0：右列里的长地址、脚本编辑器不该把 grid 列撑破 */
  min-width: 0;
}

/* 名称 + 状态徽标同组：卡头换行时两者一起换，徽标不会离开名称 */
.http-editor-heading {
  display: flex;
  align-items: center;
  flex-wrap: wrap;
  gap: var(--space-sm);
  min-width: 0;
}

.http-editor-heading h2 {
  margin-right: 0;
}

/* ===== 配置向导入口 ===== */
/* 胶囊写法对齐 LoginChannelField 的「配置直连任务」入口：同一件事在两处的观感一致
   （都是"跳去分步配置"的次级入口，不该长得像主操作按钮） */
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

/* ===== 测试区 ===== */
/* 与上方字段区隔开：测试不落盘、不代表已保存，视觉上要能看出是"试一下" */
.http-test-area {
  margin-top: var(--space-lg);
  padding-top: var(--space-md);
  border-top: 1px dashed var(--border);
}

.http-test-head {
  display: flex;
  align-items: center;
  gap: 6px;
  margin-bottom: var(--space-sm);
}

.http-test-head strong {
  color: var(--text-primary);
  font-size: var(--text-base);
  font-weight: 600;
}

/* 未打开编辑器时的右列：一行指路（限宽，别在 984px 里铺成一条长行） */
.http-ref-lead {
  max-width: 620px;
  color: var(--text-secondary);
  font-size: var(--text-md);
  line-height: 1.7;
}

/* 词条样式与 HttpTaskFields 内的同名类一致：同一套视觉表达"照抄进输入框的值" */
.http-chip-row {
  display: flex;
  align-items: center;
  flex-wrap: wrap;
  gap: 6px;
}

.http-chip {
  padding: 2px 8px;
  border: 1px solid var(--border);
  border-radius: var(--radius-sm);
  background: var(--bg-secondary);
  color: var(--text-primary);
  font-family: var(--font-mono);
  font-size: var(--text-xs);
}

.http-chip--fn {
  color: var(--accent);
}
</style>
