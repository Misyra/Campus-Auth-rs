<script setup lang="ts">
/**
 * 登录方式选择器（渠道 + 该渠道用哪个任务）+ 直连测试。
 *
 * **请求参数不在这里编辑**：它们属于「直连任务」（任务页 · 直连任务 Tab，
 * `<base>/tasks/http/<id>.json`），方案只引用一个任务 id（`active_http_task`）。
 * 于是同一门户的多个账号共用一份配置，字段编辑也只有一处入口——此前同一份
 * 直连配置既能在方案编辑器改、又能在别处改，排查时说不清哪个生效。
 *
 * 草稿对象契约（与后端 ProfileData 同名字段）：
 *   active_task / login_channel / active_http_task
 *
 * 账号、密码、认证地址仍属方案：直连测试用宿主草稿里的账号密码，密码留空且为
 * 已保存方案时由后端回退该方案的本机凭据（见 POST /api/http-tasks/test）。
 *
 * 「去任务页配置」由宿主决定怎么走（emit `openGuide`），组件不直接导航——
 * 它同时被方案编辑器与向导宿主使用。
 */
import IconApp from "@/components/common/IconApp.vue";
import CustomSelect from "@/components/common/CustomSelect.vue";
import FieldHelp from "@/components/common/FieldHelp.vue";
import HttpTestResult from "@/components/common/HttpTestResult.vue";
import { computed } from "vue";
import { useTaskDirectory } from "@/composables/useTaskDirectory";
import { useHttpTaskTest } from "@/composables/useHttpTaskTest";
import { useToast } from "@/composables/useToast";
import { DEFAULT_TASK_ID } from "@/utils/constants";
import { browserTaskOptions, httpTaskOptions, taskBindingDisplay } from "@/utils/loginChannel";
import type { SelectOption } from "@/components/common/CustomSelect.vue";

/** 本组件读写的最小字段集（宿主草稿类型可含更多字段） */
export interface LoginChannelDraft {
  /** 浏览器渠道使用的任务 ID（空 = 未绑定，登录时回退内置默认任务） */
  active_task: string;
  login_channel: "browser" | "http";
  /** 直连渠道使用的直连任务 ID（空 = 未绑定；直连没有内置兜底任务） */
  active_http_task: string;
}

const props = withDefaults(
  defineProps<{
    /** 草稿对象（原地修改，宿主脏检测基于全量序列化比对，可直接感知） */
    modelValue: LoginChannelDraft;
    /** 已保存方案 ID；无（新建/未保存）时不传，测试需手填密码 */
    profileId?: string;
    /** 草稿中的账号，用于测试请求；留空则测试按钮提示先填账号 */
    username?: string;
    /** 草稿中的密码；留空时后端回退已保存凭据 */
    password?: string;
    /** 草稿中的认证地址（直连任务的 auth_url 留空时由后端回退用它） */
    authUrl?: string;
    /** 是否展示「发送测试请求」与其结果面板 */
    showTest?: boolean;
    /** 区段标题；传 null 表示由宿主自行渲染标题（无标题的紧凑场景） */
    title?: string | null;
    /** 是否展示「配置直连任务」入口（跳任务页；紧凑场景可不显示） */
    showGuide?: boolean;
  }>(),
  {
    profileId: undefined,
    username: "",
    password: "",
    authUrl: "",
    showTest: true,
    title: "登录方式",
    showGuide: false,
  },
);

const emit = defineEmits<{ openGuide: [] }>();

const { browserTasks, httpTasks } = useTaskDirectory();
const { toastOnly } = useToast();
const { running: httpTestRunning, result: testResult, runHttpTaskTest, clearTestResult } =
  useHttpTaskTest();

/**
 * 说明文案集中在此（`\n` 分段，气泡按 pre-line 渲染）。
 *
 * 直连的字段说明在任务页（`HttpTaskFields`）；这里只讲"渠道怎么选、任务是什么"，
 * 两处不重复。
 */
const HELP = {
  channel:
    "浏览器自动化兼容验证码、动态表单等复杂门户，但需要 Python 与浏览器。\n\n" +
    "直连请求在程序内直接向门户发登录请求，免 Python 与浏览器、更快；" +
    "失败后仍按方案的重试策略重发，不会自动切回浏览器。",
  browserTask:
    "本方案自动登录时执行的任务。任务内容在「任务 · 浏览器任务」里编辑；每个方案可各绑定一个，" +
    "切换方案即切换任务。选「通用登录（内置默认）」即使用程序自带的任务。",
  httpTask:
    "本方案直连登录时使用哪份直连任务（请求地址、请求头、判定关键字、凭据变换脚本都在任务里）。\n\n" +
    "同一门户的多个账号共用一份任务，字段在「任务 · 直连任务」里编辑。" +
    "直连没有内置兜底任务——门户地址没法内置，所以未绑定时直连登录会直接失败。",
  test: "测试只发这一次请求，不会保存任何配置，也不会改变自动登录状态。",
} as const;

const isHttp = computed(() => props.modelValue.login_channel === "http");

/**
 * 浏览器任务绑定值的显示代理：未绑定（空 `active_task`）时显示为内置默认任务。
 *
 * 走 get/set 代理而非在载入时把 `default` 写进草稿：后者会让草稿与服务端立刻
 * 不一致，编辑器一打开就显示「未保存」。代理只在用户真的下拉选择时才写回，
 * 未触碰的方案保持原有空值（后端对空值与 `default` 解析结果相同，两者等价）。
 */
const taskBinding = computed<string>({
  get: () => taskBindingDisplay(props.modelValue.active_task, DEFAULT_TASK_ID),
  set: (value: string) => {
    props.modelValue.active_task = value;
  },
});

/** 任务下拉选项：直接用任务列表的真实条目（default 一条标注「内置默认」） */
const browserTaskSelectOptions = computed<SelectOption[]>(() =>
  browserTaskOptions(browserTasks.value, DEFAULT_TASK_ID),
);

/** 直连任务下拉选项：首项是"未绑定"，因为直连没有兜底任务可回退 */
const httpTaskSelectOptions = computed<SelectOption[]>(() =>
  httpTaskOptions(httpTasks.value),
);

/** 已绑定任务是否存在于当前任务清单（被删掉的任务要当场看得见） */
const boundHttpTaskMissing = computed(
  () =>
    props.modelValue.active_http_task.trim() !== "" &&
    !httpTasks.value.some((t) => t.id === props.modelValue.active_http_task),
);

/** 已保存方案可留空密码由后端回退；新建/未保存时必须手填 */
const hasSavedProfile = computed(() => Boolean(props.profileId));

/** 表单控件 id 前缀：同页可能同时挂载多个实例 */
const uid = `login-channel-${Math.random().toString(36).slice(2, 8)}`;

/** 切换渠道：原地写回草稿（宿主序列化比对即可感知为未保存改动） */
function setChannel(channel: "browser" | "http"): void {
  props.modelValue.login_channel = channel;
  clearTestResult();
}

/** 换绑任务后旧结论即失效（结果面板上写着上一个任务的地址） */
function onHttpTaskChange(value: string): void {
  props.modelValue.active_http_task = value;
  clearTestResult();
}

async function runTest(): Promise<void> {
  if (!props.modelValue.active_http_task.trim()) {
    toastOnly(false, "请先选择一个直连任务");
    return;
  }
  if (!(props.username ?? "").trim()) {
    toastOnly(false, "请填写账号后再测试");
    return;
  }
  if (!hasSavedProfile.value && !(props.password ?? "")) {
    toastOnly(false, "请填写密码后再测试");
    return;
  }
  testResult.value = null;
  await runHttpTaskTest({
    task_id: props.modelValue.active_http_task.trim(),
    profile_id: props.profileId,
    username: props.username ?? "",
    password: props.password ?? "",
    fetch_page: true,
  });
}
</script>

<template>
  <div>
    <!-- 标题行：标题与入口同处一行。标题可为 null（宿主自渲染标题的紧凑场景），
         但入口必须独立于标题渲染——否则 :title="null" 的宿主会连入口一起消失。 -->
    <div v-if="title !== null || showGuide" class="channel-section-head">
      <span v-if="title !== null" class="editor-section-label channel-section-label">
        {{ title }}
        <FieldHelp :text="HELP.channel" />
      </span>
      <a
        v-if="showGuide"
        href="#"
        class="channel-guide-link"
        title="直连请求参数在「任务 · 直连任务」里配置，同一门户的账号共用一份"
        @click.prevent="emit('openGuide')"
      >
        <IconApp name="sparkles" class="icon-sm" />
        配置直连任务
      </a>
    </div>

    <!-- 渠道卡片：两渠道各自说明「要不要环境、适合谁」，比纯文字分段控件更可判 -->
    <div class="channel-cards" role="radiogroup" aria-label="登录方式">
      <button
        type="button"
        class="channel-card"
        role="radio"
        :aria-checked="!isHttp"
        :class="{ active: !isHttp }"
        @click="setChannel('browser')"
      >
        <span class="channel-card-icon"><IconApp name="chrome" /></span>
        <span class="channel-card-copy">
          <strong>浏览器自动化</strong>
          <small>按任务步骤操作登录页，兼容验证码与动态表单</small>
        </span>
        <span class="channel-card-cost">需要 Python 与浏览器</span>
      </button>

      <button
        type="button"
        class="channel-card"
        role="radio"
        :aria-checked="isHttp"
        :class="{ active: isHttp }"
        @click="setChannel('http')"
      >
        <span class="channel-card-icon"><IconApp name="globe" /></span>
        <span class="channel-card-copy">
          <strong>直连请求</strong>
          <small>直接向校园网网关发登录请求，不开浏览器、更快更省资源</small>
        </span>
        <span class="channel-card-cost channel-card-cost--free">免 Python 与浏览器</span>
      </button>
    </div>

    <div v-if="!isHttp" class="browser-channel-panel">
      <div class="form-group">
        <div class="field-label-row">
          <label :for="`${uid}-task`">浏览器任务</label>
          <FieldHelp :text="HELP.browserTask" wide />
        </div>
        <CustomSelect :id="`${uid}-task`" v-model="taskBinding" :options="browserTaskSelectOptions" />
      </div>
    </div>

    <div v-else class="http-channel-panel">
      <div class="form-group">
        <div class="field-label-row">
          <label :for="`${uid}-http-task`">直连任务</label>
          <FieldHelp :text="HELP.httpTask" wide />
        </div>
        <CustomSelect
          :id="`${uid}-http-task`"
          :model-value="modelValue.active_http_task"
          :options="httpTaskSelectOptions"
          @update:model-value="onHttpTaskChange"
        />
      </div>

      <!-- 未绑定 / 绑定的任务已被删除：两种都会让直连登录直接失败，故当场提示并给出出口 -->
      <div v-if="boundHttpTaskMissing" class="note note--warn">
        <IconApp name="alert-triangle" class="icon-sm" />
        <span>绑定的直连任务已不存在，请重新选择；直连登录在选中任务前不可用。</span>
      </div>
      <div v-else-if="!modelValue.active_http_task.trim()" class="note">
        <IconApp name="info" class="icon-sm" />
        <span>尚未绑定直连任务，直连登录会直接失败。</span>
        <a href="#" class="http-task-link" @click.prevent="emit('openGuide')">去新建 →</a>
      </div>

      <template v-if="showTest">
        <div class="http-test-actions">
          <button type="button" class="btn btn-primary" :disabled="httpTestRunning" @click="runTest">
            <IconApp :name="httpTestRunning ? 'refresh' : 'play'"
              class="icon-sm" :class="{ spin: httpTestRunning }" />
            {{ httpTestRunning ? '正在发送…' : '发送测试请求' }}
          </button>
          <FieldHelp :text="HELP.test" />
          <a class="btn btn-ghost" href="https://campus-auth.misyra.com/docs/profiles/http-login" target="_blank" rel="noopener noreferrer">
            <IconApp name="file-text" class="icon-sm" />
            使用文档
          </a>
          <span v-if="!hasSavedProfile" class="http-test-hint">需先填好账号与密码</span>
        </div>

        <HttpTestResult v-if="testResult" :result="testResult" />
      </template>
    </div>
  </div>
</template>

<style scoped>
/* 登录方式：渠道卡 + 该渠道的任务选择器（字段编辑在任务页，此处只管"用哪个"）。
   与「任务 · 直连任务」的编辑器形成对照：这里一眼看清两渠道的取舍。 */

.channel-section-label {
  /* 区段标题样式由 profiles.css 提供；此处仅保证在 head 行内不被压扁 */
  flex-wrap: wrap;
}

.channel-section-head {
  /* 标题 + 入口一行：入口靠右，标题缺省时不占位 */
  display: flex;
  align-items: center;
  gap: var(--space-sm);
  flex-wrap: wrap;
  margin-bottom: 12px;
}

.channel-section-head .editor-section-label {
  margin-bottom: 0;
}

.channel-guide-link {
  display: inline-flex;
  align-items: center;
  gap: 4px;
  margin-left: auto;
  padding: 2px 8px;
  border: 1px solid var(--border-accent-strong);
  border-radius: var(--radius-full);
  background: rgba(var(--accent-rgb), 0.08);
  color: var(--accent);
  font-size: var(--text-sm);
  font-weight: 600;
  text-decoration: none;
  transition: background var(--dur-base) var(--ease-out);
}

.channel-guide-link:hover {
  background: rgba(var(--accent-rgb), 0.16);
}

/* ===== 渠道卡片 ===== */
.channel-cards {
  display: grid;
  grid-template-columns: 1fr 1fr;
  gap: var(--space-sm);
  margin-bottom: var(--space-sm);
}

.channel-card {
  display: grid;
  grid-template-columns: auto minmax(0, 1fr);
  grid-template-rows: auto auto;
  align-items: center;
  gap: 2px var(--space-sm);
  padding: 12px 14px;
  border: 1px solid var(--border);
  border-radius: var(--radius-lg);
  background: var(--bg-glass-light);
  color: var(--text-secondary);
  text-align: left;
  cursor: pointer;
  transition: border-color var(--dur-base) var(--ease-out), background var(--dur-base) var(--ease-out);
}

.channel-card:hover {
  border-color: var(--border-hover);
  background: var(--bg-hover);
}

.channel-card.active {
  border-color: var(--accent);
  background: rgba(var(--accent-rgb), 0.08);
}

.channel-card-icon {
  grid-row: 1 / span 2;
  display: flex;
  align-items: center;
  justify-content: center;
  width: 32px;
  height: 32px;
  border-radius: var(--radius-md);
  background: var(--bg-secondary);
  color: var(--text-muted);
}

.channel-card.active .channel-card-icon {
  background: rgba(var(--accent-rgb), 0.16);
  color: var(--accent);
}

.channel-card-icon svg {
  width: 18px;
  height: 18px;
}

.channel-card-copy {
  display: flex;
  flex-direction: column;
  gap: 2px;
  min-width: 0;
}

.channel-card-copy strong {
  color: var(--text-primary);
  font-size: var(--text-base);
  font-weight: 600;
}

.channel-card-copy small {
  color: var(--text-muted);
  font-size: var(--text-sm);
  line-height: 1.45;
}

.channel-card-cost {
  grid-column: 2;
  justify-self: start;
  margin-top: 4px;
  padding: 1px 8px;
  border-radius: var(--radius-full);
  background: var(--warning-bg);
  color: var(--warning-text);
  font-size: var(--text-xs);
  font-weight: 600;
}

.channel-card-cost--free {
  background: var(--success-bg);
  color: var(--success);
}

/* 浏览器渠道：任务选择面板（与直连面板同构，保持两渠道视觉对等） */
.browser-channel-panel {
  margin-top: var(--space-sm);
  padding: var(--space-md);
  border: 1px solid var(--border);
  border-radius: var(--radius-lg);
  background: var(--bg-glass-light);
}

.browser-channel-panel .form-group {
  margin-bottom: 0;
}

/* ===== 直连面板 ===== */
.http-channel-panel {
  margin-top: var(--space-sm);
  padding: var(--space-md);
  border: 1px solid var(--border-accent);
  border-radius: var(--radius-lg);
  background:
    linear-gradient(135deg, rgba(var(--accent-rgb), 0.06), transparent 42%),
    var(--bg-glass-light);
}

.http-channel-panel .form-group {
  margin-bottom: var(--space-sm);
}

/* 绑定缺失提示改用全局 `.note` / `.note--warn`（components/misc.css）：
   - 原 `--muted` 变体（border/中性底/次要色）与 `.note` 的默认态完全同值
   - 原 `--warn` 变体是同族的第三份副本（只差 padding 8px 10px 与 align-items:center） */

.http-task-link {
  color: var(--accent);
  font-weight: 600;
  text-decoration: none;
}

.http-task-link:hover {
  text-decoration: underline;
}

/* ===== 测试 ===== */
.http-test-actions {
  display: flex;
  align-items: center;
  gap: var(--space-md);
  flex-wrap: wrap;
  margin-top: var(--space-md);
}

.http-test-hint {
  color: var(--text-muted);
  font-size: var(--text-sm);
}

@media (max-width: 768px) {
  .channel-cards {
    grid-template-columns: 1fr;
  }
}
</style>
