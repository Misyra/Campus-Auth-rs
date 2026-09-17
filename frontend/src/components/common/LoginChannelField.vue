<script setup lang="ts">
/**
 * 登录方式选择器（渠道 + 浏览器任务）+ 直连请求参数面板（可复用）。
 *
 * 从 ProfilesView 编辑器抽出：登录方式此前只在该编辑器内可选，其他入口
 * （设置页、引导向导）无法切换。以 `v-model` 绑定一个含登录渠道与直连字段的
 * 草稿对象，宿主只负责提供草稿与保存，本组件只管编辑与测试。
 *
 * 草稿对象契约（与后端 ProfileData 同名字段）：
 *   active_task / login_channel / http_method / http_url / http_headers /
 *   http_body / http_success_pattern / http_failure_pattern / http_crypto_script
 *
 * 浏览器任务选择内聚在此处：它是「怎么登录」的一部分（浏览器渠道要指定用哪个
 * 任务操作网页），且**按方案绑定**——切方案即切任务。任务管理页只负责编辑，
 * 不再承担"启用哪个"的职责。
 *
 * `showTest` 供不落盘的场景使用；测试请求经 `useProfiles.testHttpLogin`
 * 发送，不会保存方案、也不会触发登录状态机。
 *
 * 「测试」需要账号与密码可用：账号取草稿的 username，密码留空时由后端按
 * profileId 回退该方案本机已保存凭据（见 POST /api/profiles/http-login-test）。
 *
 * 直连面板按「填什么 → 发什么 → 怎么判定 → 要不要变换凭据 → 验证」的因果顺序
 * 分组编号：这一块字段多（7 个输入）且互有依赖，此前平铺成一片，用户常只填了
 * 地址就点测试、拿到「未命中成功标识」后不知道还差什么。编号 + 每组的
 * 一句话说明把「先填哪个」显式化。
 */
import IconApp from "@/components/common/IconApp.vue";
import CustomSelect from "@/components/common/CustomSelect.vue";
import FieldHelp from "@/components/common/FieldHelp.vue";
import { computed, ref } from "vue";
import { useProfiles } from "@/composables/useProfiles";
import { useTaskDirectory } from "@/composables/useTaskDirectory";
import { DEFAULT_TASK_ID } from "@/utils/constants";
import {
  HTTP_BODY_EXAMPLE,
  HTTP_CRYPTO_BUILTINS,
  HTTP_CRYPTO_CTX_FIELDS,
  HTTP_CRYPTO_SCRIPT_SKELETON,
  HTTP_HEADERS_EXAMPLE,
  HTTP_MAC_FORMAT_NOTE,
  HTTP_METHOD_OPTIONS,
  HTTP_TEMPLATE_PLACEHOLDERS,
  browserTaskOptions,
  httpTestOutcomeHint,
  httpTestOutcomeLabel,
  isCredentialExposedViaGet,
  taskBindingDisplay,
} from "@/utils/loginChannel";
import type { HttpLoginTestResult } from "@/api/types";
import type { SelectOption } from "@/components/common/CustomSelect.vue";

/** 本组件读写的最小字段集（宿主草稿类型可含更多字段） */
export interface LoginChannelDraft {
  /** 浏览器渠道使用的任务 ID（空 = 未绑定，登录时回退内置默认任务） */
  active_task: string;
  login_channel: "browser" | "http";
  http_method: "GET" | "POST";
  http_url: string;
  http_headers: string;
  http_body: string;
  http_success_pattern: string;
  http_failure_pattern: string;
  http_crypto_script: string;
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
    /** 草稿中的认证地址，作为脚本 ctx.auth_url 与登录页抓取来源 */
    authUrl?: string;
    /** 是否展示「发送测试请求」与其结果面板 */
    showTest?: boolean;
    /** 区段标题；传 null 表示由宿主自行渲染标题（无标题的紧凑场景） */
    title?: string | null;
    /** 是否展示「直连配置向导」入口（配置方案编辑器等有足够空间的宿主传 true） */
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

const p = useProfiles();

// 浏览器任务清单：与任务页同源（useTaskDirectory 单次拉取），此处只读展示
const { browserTasks } = useTaskDirectory();

/** 任务下拉选项：直接用任务列表的真实条目（default 一条标注「内置默认」） */
const taskOptions = computed<SelectOption[]>(() =>
  browserTaskOptions(browserTasks.value, DEFAULT_TASK_ID),
);

/**
 * 绑定值的显示代理：未绑定（空 `active_task`）时显示为内置默认任务。
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

// 测试结果自持：多实例（不同宿主/多方案）各自展示，互不覆盖
const testResult = ref<HttpLoginTestResult | null>(null);
// 高级项（凭据变换脚本）默认收起：绝大多数门户不需要，展开后会占掉半屏，
// 把「地址 / 请求内容 / 判定」这三步挤到首屏之外
const scriptOpen = ref(false);

/** 切换渠道：原地写回草稿（宿主序列化比对即可感知为未保存改动） */
function setChannel(channel: "browser" | "http"): void {
  props.modelValue.login_channel = channel;
}

const isHttp = computed(() => props.modelValue.login_channel === "http");

/** GET + 地址含 {password}：凭据落在查询串里，需要专门提示 */
const passwordInUrl = computed(() =>
  isCredentialExposedViaGet(props.modelValue.http_method, props.modelValue.http_url),
);

/** 已保存方案可留空密码由后端回退；新建/未保存时必须手填 */
const hasSavedProfile = computed(() => Boolean(props.profileId));

/** 表单控件 id 前缀：同页可能同时挂载多个实例（方案编辑器 + 设置页） */
const uid = `http-login-${Math.random().toString(36).slice(2, 8)}`;

/** 已展开过脚本区（或草稿里本来就有脚本）时保持展开，避免收起遮蔽既有配置 */
const scriptHasContent = computed(() => props.modelValue.http_crypto_script.trim().length > 0);

function fillUrlExample(): void {
  props.modelValue.http_url =
    "http://10.0.0.1/login?username={username}&password={password}";
}

function fillHeadersExample(): void {
  props.modelValue.http_headers = HTTP_HEADERS_EXAMPLE;
}

function fillBodyExample(): void {
  props.modelValue.http_body = HTTP_BODY_EXAMPLE;
}

function fillScriptSkeleton(): void {
  props.modelValue.http_crypto_script = HTTP_CRYPTO_SCRIPT_SKELETON;
  scriptOpen.value = true;
}

async function runTest(): Promise<void> {
  const draft = props.modelValue;
  if (!draft.http_url.trim()) {
    p.toastHttpTestPrecondition("请填写直连请求地址");
    return;
  }
  if (!(props.username ?? "").trim()) {
    p.toastHttpTestPrecondition("请填写账号后再测试");
    return;
  }
  testResult.value = null;
  testResult.value = await p.testHttpLogin({
    profileId: props.profileId,
    username: props.username ?? "",
    password: props.password ?? "",
    http_method: draft.http_method,
    http_url: draft.http_url,
    http_headers: draft.http_headers,
    http_body: draft.http_body,
    http_success_pattern: draft.http_success_pattern,
    http_failure_pattern: draft.http_failure_pattern,
    http_crypto_script: draft.http_crypto_script,
    auth_url: props.authUrl ?? "",
  });
}
</script>

<template>
  <div class="channel-section">
    <!-- 标题行：标题与向导入口同处一行。标题可为 null（宿主自渲染标题的紧凑场景），
         但向导入口必须独立于标题渲染——否则 :title="null" 的宿主（设置·账号）
         会连入口一起消失。 -->
    <div v-if="title !== null || showGuide" class="channel-section-head">
      <span v-if="title !== null" class="editor-section-label channel-section-label">
        {{ title }}
        <FieldHelp text="浏览器自动化兼容验证码、动态表单等复杂门户；直连请求无需 Python 与浏览器，适合可直接调用登录接口的门户。" />
      </span>
      <a
        v-if="showGuide"
        href="#"
        class="channel-guide-link"
        title="分步引导：填入地址、请求内容与判定关键字，并当场验证"
        @click.prevent="emit('openGuide')"
      >
        <IconApp name="sparkles" class="icon-sm" />
        直连配置向导
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
          <FieldHelp text="本方案自动登录时执行的任务。任务内容在「任务」页编辑；每个方案可各绑定一个，切换方案即切换任务。选「通用登录（内置默认）」即使用程序自带的任务。" />
        </div>
        <CustomSelect :id="`${uid}-task`" v-model="taskBinding" :options="taskOptions" />
        <span class="hint">按方案绑定：切换方案会同时切换任务</span>
      </div>
      <p class="channel-note">按选定任务的步骤操作网页，适合验证码、动态表单和复杂交互。</p>
    </div>

    <div v-else class="http-channel-panel">
      <div class="http-channel-intro">
        <strong>直接向校园网网关发送登录请求</strong>
        <span>不启动浏览器；失败后按普通登录策略重试，不会自动切回浏览器。</span>
      </div>

      <!-- ① 请求地址 -->
      <section class="http-step">
        <div class="http-step-head">
          <span class="http-step-num">1</span>
          <div>
            <strong>填写门户的登录请求地址</strong>
            <small>浏览器开发者工具 → Network → 点登录 → 复制该请求的 URL 与参数</small>
          </div>
        </div>

        <div class="form-row http-url-row">
          <div class="form-group http-method-field">
            <label :for="`${uid}-method`">方法</label>
            <CustomSelect :id="`${uid}-method`" v-model="modelValue.http_method" :options="HTTP_METHOD_OPTIONS" />
          </div>
          <div class="form-group">
            <div class="field-label-row">
              <label :for="`${uid}-url`">请求地址</label>
              <button type="button" class="btn btn-link http-fill-btn" @click="fillUrlExample">填入示例</button>
            </div>
            <input :id="`${uid}-url`" v-model.trim="modelValue.http_url" type="text"
              placeholder="http://10.0.0.1/login?username={username}&password={password}" />
          </div>
        </div>

        <div v-if="passwordInUrl" class="http-risk-note">
          <IconApp name="alert-triangle" class="icon-sm" />
          <span>
            当前把密码放进了请求地址，网关、代理或系统网络日志仍可能记录完整地址。程序只保证自身日志脱敏；能用 POST 时优先用 POST。
          </span>
        </div>
      </section>

      <!-- ② 请求内容 -->
      <section class="http-step">
        <div class="http-step-head">
          <span class="http-step-num">2</span>
          <div>
            <strong>补齐请求头与请求内容</strong>
            <small>GET 通常只需地址；POST 还要照抄表单内容，部分门户校验 Referer</small>
          </div>
        </div>

        <div class="form-group">
          <div class="field-label-row">
            <label :for="`${uid}-headers`">请求头</label>
            <button type="button" class="btn btn-link http-fill-btn" @click="fillHeadersExample">填入示例</button>
          </div>
          <textarea :id="`${uid}-headers`" v-model="modelValue.http_headers" rows="3"
            :placeholder="HTTP_HEADERS_EXAMPLE"></textarea>
          <span class="hint">每行一项，格式为“名称: 值”。</span>
        </div>

        <div v-if="modelValue.http_method === 'POST'" class="form-group">
          <div class="field-label-row">
            <label :for="`${uid}-body`">请求内容</label>
            <button type="button" class="btn btn-link http-fill-btn" @click="fillBodyExample">填入示例</button>
          </div>
          <textarea :id="`${uid}-body`" v-model="modelValue.http_body" rows="4"
            :placeholder="HTTP_BODY_EXAMPLE"></textarea>
        </div>

        <div class="http-template-help">
          <div class="http-template-head">
            地址、请求头与请求内容都支持占位符，发送前会被替换成真实值：
          </div>
          <div class="http-chip-row">
            <code v-for="ph in HTTP_TEMPLATE_PLACEHOLDERS" :key="ph" class="http-chip">{{ ph }}</code>
          </div>
          <div class="http-template-note">
            值原样替换、不做转义，特殊字符请在脚本中用 <code>url_encode()</code> 处理；写错的占位符会原样发出去（便于在测试结果里发现）。
          </div>
          <div class="http-template-note">
            {{ HTTP_MAC_FORMAT_NOTE }}
          </div>
        </div>
      </section>

      <!-- ③ 成败判定 -->
      <section class="http-step">
        <div class="http-step-head">
          <span class="http-step-num">3</span>
          <div>
            <strong>告诉程序怎么判断登录成功</strong>
            <small>填响应里出现的文字即可；留空则只要返回 2xx 就算成功</small>
          </div>
        </div>

        <div class="form-row">
          <div class="form-group">
            <div class="field-label-row">
              <label :for="`${uid}-success`">成功关键字</label>
              <FieldHelp text="响应内容里出现这段文字即判定成功。留空时以 HTTP 2xx 判断，但部分门户（如 Dr.COM / eportal）即使密码错误也返回 200，此时必须填写。" />
            </div>
            <input :id="`${uid}-success`" v-model="modelValue.http_success_pattern" type="text"
              placeholder="登录成功（留空则以 HTTP 2xx 判断）" />
          </div>
          <div class="form-group">
            <div class="field-label-row">
              <label :for="`${uid}-failure`">失败关键字</label>
              <FieldHelp text="响应内容里出现这段文字即判定失败，并且不再重试（如密码错误这种重试无意义的结论）。留空则不据此提前判失败。" />
            </div>
            <input :id="`${uid}-failure`" v-model="modelValue.http_failure_pattern" type="text"
              placeholder="账号或密码错误" />
          </div>
        </div>

        <div class="http-judge-note">
          <IconApp name="info" class="icon-sm" />
          <span>
            判定顺序：先看失败关键字，再看成功关键字。门户响应恒为 <code>HTTP 200</code> 时（JSONP 接口很常见）两个关键字都要填，否则凭据错误也会被判成成功。
          </span>
        </div>
      </section>

      <!-- ④ 凭据变换（可选，默认收起） -->
      <section class="http-step http-step--advanced">
        <button
          type="button"
          class="http-advanced-toggle"
          :aria-expanded="scriptOpen || scriptHasContent"
          @click="scriptOpen = !scriptOpen"
        >
          <IconApp name="chevron-down" class="icon-sm http-advanced-arrow" :class="{ expanded: scriptOpen || scriptHasContent }" />
          <span class="http-step-num http-step-num--muted">4</span>
          <span class="http-advanced-copy">
            <strong>凭据变换脚本（可选）</strong>
            <small>门户要求密码加密、或需要按本机 IP 算签名时才用</small>
          </span>
          <span v-if="scriptHasContent" class="http-advanced-badge">已配置</span>
        </button>

        <div v-show="scriptOpen || scriptHasContent" class="http-advanced-body">
          <div class="form-group">
            <div class="field-label-row">
              <label :for="`${uid}-script`">脚本内容</label>
              <button type="button" class="btn btn-link http-fill-btn" @click="fillScriptSkeleton">填入骨架</button>
            </div>
            <textarea :id="`${uid}-script`" v-model="modelValue.http_crypto_script" class="http-script-editor" rows="8"
              placeholder="function transform(ctx) {
  return { password: md5(ctx.password) };
}"></textarea>
          </div>

          <div class="http-script-help">
            <div class="http-help-block">
              <span class="http-help-title">脚本要定义 <code>transform(ctx)</code>，返回对象的字段可直接用 <code>{字段名}</code> 引用</span>
              <div class="http-chip-row">
                <span class="http-chip-label">可读入参 ctx</span>
                <code v-for="f in HTTP_CRYPTO_CTX_FIELDS" :key="f" class="http-chip">{{ f }}</code>
              </div>
              <div class="http-chip-row">
                <span class="http-chip-label">可用函数</span>
                <code v-for="fn in HTTP_CRYPTO_BUILTINS" :key="fn" class="http-chip http-chip--fn">{{ fn }}</code>
              </div>
              <div class="http-template-note">
                ctx 不含网络与文件访问能力；脚本最长执行 500 毫秒，超时即判为配置错误。
                <code>page</code> 是认证地址的页面原文（抓取失败时为空串），<code>local_ip</code> / <code>local_mac</code> 取不到时为空串，脚本需要自行容忍。
              </div>
            </div>
          </div>
        </div>
      </section>

      <!-- ⑤ 验证 -->
      <template v-if="showTest">
        <section class="http-step http-step--test">
          <div class="http-test-actions">
            <button type="button" class="btn btn-primary" :disabled="p.httpTestRunning.value" @click="runTest">
              <IconApp :name="p.httpTestRunning.value ? 'refresh' : 'play'"
                class="icon-sm" :class="{ spin: p.httpTestRunning.value }" />
              {{ p.httpTestRunning.value ? '正在发送…' : '发送测试请求' }}
            </button>
            <a class="btn btn-ghost" href="/api/docs/http-login-guide">
              <IconApp name="file-text" class="icon-sm" />
              使用文档
            </a>
            <span class="http-test-hint">
              测试只发这一次请求，不会保存方案，也不会改变自动登录状态。
              <template v-if="!hasSavedProfile">需先填好账号与密码。</template>
            </span>
          </div>

          <div v-if="testResult" class="http-test-result"
            :class="testResult.outcome === 'success' ? 'success' : 'failed'">
            <div class="http-test-result-head">
              <span class="http-test-result-icon">
                <IconApp :name="testResult.outcome === 'success' ? 'check-circle' : 'alert-triangle'" />
              </span>
              <div class="http-test-result-title">
                <strong>{{ httpTestOutcomeLabel(testResult.outcome) }}</strong>
                <span v-if="testResult.status">HTTP {{ testResult.status }}</span>
                <span v-else>无响应</span>
                <span>{{ testResult.duration_ms }} ms</span>
              </div>
            </div>

            <p class="http-test-result-message">{{ testResult.message }}</p>
            <p class="http-test-result-hint">{{ httpTestOutcomeHint(testResult.outcome) }}</p>

            <div v-if="testResult.script_error" class="http-test-script-error">
              <strong>脚本错误</strong>
              <code>{{ testResult.script_error }}</code>
            </div>

            <details class="http-test-detail">
              <summary>查看实际发出的请求与响应</summary>
              <dl>
                <template v-if="testResult.rendered_url">
                  <dt>请求地址</dt><dd><code>{{ testResult.rendered_url }}</code></dd>
                </template>
                <template v-if="testResult.rendered_headers">
                  <dt>请求头</dt><dd><code>{{ testResult.rendered_headers }}</code></dd>
                </template>
                <template v-if="testResult.rendered_body">
                  <dt>请求内容</dt><dd><code>{{ testResult.rendered_body }}</code></dd>
                </template>
                <template v-if="testResult.response_snippet">
                  <dt>响应片段</dt><dd><code>{{ testResult.response_snippet }}</code></dd>
                </template>
              </dl>
              <p class="http-test-detail-note">其中的账号与密码已替换为 <code>***</code>。</p>
            </details>
          </div>
        </section>
      </template>
    </div>
  </div>
</template>

<style scoped>
/* 直连登录面板：字段多且互有依赖，用「编号步骤 + 因果顺序」组织，
   与浏览器渠道的单块面板形成明确对比（谁需要更多配置一眼可见）。 */

.channel-section-label {
  /* 区段标题样式由 profiles.css 提供；此处仅保证在 head 行内不被压扁 */
  flex-wrap: wrap;
}

.channel-section-head {
  /* 标题 + 向导入口一行：入口靠右，标题缺省时不占位 */
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

.channel-note {
  margin: 0;
  color: var(--text-muted);
  font-size: var(--text-sm);
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
  margin-bottom: var(--space-sm);
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

.http-channel-intro {
  display: flex;
  flex-direction: column;
  gap: 3px;
  margin-bottom: var(--space-md);
}

.http-channel-intro strong {
  color: var(--text-primary);
  font-size: var(--text-base);
}

.http-channel-intro span {
  color: var(--text-muted);
  font-size: var(--text-sm);
}

/* 步骤块：左侧竖线把「编号 + 说明 + 字段」绑成一个视觉单元 */
.http-step {
  margin-top: var(--space-md);
  padding-left: 12px;
  border-left: 2px solid rgba(var(--accent-rgb), 0.25);
}

.http-step:first-of-type {
  margin-top: var(--space-sm);
}

.http-step--test {
  border-left-color: transparent;
  padding-left: 0;
}

.http-step-head {
  display: flex;
  align-items: flex-start;
  gap: 10px;
  margin-bottom: var(--space-sm);
}

.http-step-head strong {
  display: block;
  color: var(--text-primary);
  font-size: var(--text-base);
  font-weight: 600;
}

.http-step-head small {
  display: block;
  margin-top: 2px;
  color: var(--text-muted);
  font-size: var(--text-sm);
  line-height: 1.45;
}

.http-step-num {
  flex-shrink: 0;
  display: inline-flex;
  align-items: center;
  justify-content: center;
  width: 20px;
  height: 20px;
  border-radius: var(--radius-full);
  background: rgba(var(--accent-rgb), 0.15);
  color: var(--accent);
  font-size: var(--text-sm);
  font-weight: 700;
}

.http-step-num--muted {
  background: rgba(var(--slate-rgb), 0.15);
  color: var(--text-muted);
}

.http-url-row {
  grid-template-columns: minmax(110px, 0.25fr) minmax(0, 1.75fr);
}

.http-method-field {
  min-width: 0;
}

/* 字段标签右侧的「填入示例」：行内小链接，不抢主操作注意力 */
.http-fill-btn {
  padding: 0;
  font-size: var(--text-xs);
  font-weight: 600;
}

.http-risk-note,
.http-judge-note {
  display: flex;
  align-items: flex-start;
  gap: var(--space-sm);
  padding: 10px 12px;
  border-radius: var(--radius-md);
  font-size: var(--text-sm);
  line-height: 1.6;
}

.http-risk-note {
  margin: 0 0 var(--space-md);
  color: var(--warning-text);
  border: 1px solid rgba(var(--warning-rgb), 0.25);
  background: var(--warning-bg);
}

.http-risk-note svg,
.http-judge-note svg {
  flex-shrink: 0;
  margin-top: 2px;
}

.http-judge-note {
  color: var(--text-secondary);
  border: 1px solid var(--border);
  background: rgba(var(--slate-rgb), 0.06);
}

.http-judge-note code,
.http-template-note code,
.http-help-title code {
  font-family: var(--font-mono);
  font-size: 0.95em;
}

/* 占位符 / 内置函数速查 */
.http-template-help {
  margin-bottom: var(--space-sm);
  padding: 10px 12px;
  border: 1px dashed var(--border-accent-strong);
  border-radius: var(--radius-md);
  background: rgba(var(--accent-rgb), 0.04);
}

.http-template-head {
  color: var(--text-secondary);
  font-size: var(--text-sm);
  line-height: 1.6;
}

.http-chip-row {
  display: flex;
  align-items: center;
  flex-wrap: wrap;
  gap: 6px;
  margin-top: 8px;
}

.http-chip-label {
  color: var(--text-muted);
  font-size: var(--text-xs);
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

.http-template-note {
  margin-top: 8px;
  color: var(--text-muted);
  font-size: var(--text-sm);
  line-height: 1.6;
}

/* ===== 高级项（凭据变换脚本） ===== */
.http-step--advanced {
  border-left-color: rgba(var(--slate-rgb), 0.25);
}

.http-advanced-toggle {
  display: flex;
  align-items: center;
  gap: 10px;
  width: 100%;
  padding: 0;
  border: none;
  background: transparent;
  color: inherit;
  text-align: left;
  cursor: pointer;
}

.http-advanced-arrow {
  flex-shrink: 0;
  color: var(--text-muted);
  transition: transform var(--dur-base) var(--ease-out);
}

.http-advanced-arrow.expanded {
  transform: rotate(180deg);
}

.http-advanced-copy {
  display: flex;
  flex-direction: column;
  gap: 2px;
  min-width: 0;
}

.http-advanced-copy strong {
  color: var(--text-primary);
  font-size: var(--text-base);
  font-weight: 600;
}

.http-advanced-copy small {
  color: var(--text-muted);
  font-size: var(--text-sm);
}

.http-advanced-badge {
  margin-left: auto;
  padding: 1px 8px;
  border-radius: var(--radius-full);
  background: var(--success-bg);
  color: var(--success);
  font-size: var(--text-xs);
  font-weight: 600;
}

.http-advanced-body {
  margin-top: var(--space-sm);
}

.http-script-editor {
  min-height: 170px;
}

.http-script-help {
  padding: 10px 12px;
  border: 1px solid var(--border);
  border-radius: var(--radius-md);
  background: rgba(var(--slate-rgb), 0.05);
}

.http-help-block .http-help-title {
  display: block;
  color: var(--text-secondary);
  font-size: var(--text-sm);
  line-height: 1.6;
}

/* ===== 测试与结果 ===== */
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

.http-test-result {
  margin-top: var(--space-md);
  padding: var(--space-md);
  border-radius: var(--radius-md);
  border: 1px solid var(--danger-border);
  background: var(--danger-bg);
}

.http-test-result.success {
  border-color: var(--success-border);
  background: var(--success-bg);
}

.http-test-result-head {
  display: flex;
  align-items: center;
  gap: 10px;
}

.http-test-result-icon {
  display: flex;
  color: var(--error);
}

.http-test-result.success .http-test-result-icon {
  color: var(--success);
}

.http-test-result-icon svg {
  width: 22px;
  height: 22px;
}

.http-test-result-title {
  display: flex;
  align-items: baseline;
  flex-wrap: wrap;
  gap: var(--space-sm);
  min-width: 0;
}

.http-test-result-title strong {
  color: var(--text-primary);
  font-size: var(--text-base);
}

.http-test-result-title span {
  color: var(--text-muted);
  font-family: var(--font-mono);
  font-size: var(--text-xs);
}

.http-test-result-message {
  margin: 8px 0 0;
  color: var(--text-secondary);
  font-size: var(--text-sm);
  overflow-wrap: anywhere;
}

/* 「下一步该怎么办」：与结论分开呈现，避免用户只看结论就反复重试同一配置 */
.http-test-result-hint {
  margin: 8px 0 0;
  padding: 8px 10px;
  border-radius: var(--radius-sm);
  background: rgba(var(--slate-rgb), 0.08);
  color: var(--text-secondary);
  font-size: var(--text-sm);
  line-height: 1.6;
}

.http-test-script-error {
  display: flex;
  flex-direction: column;
  gap: 4px;
  margin-top: var(--space-sm);
  padding: 8px 10px;
  border-radius: var(--radius-sm);
  border: 1px solid var(--danger-border);
  background: rgba(var(--error-rgb), 0.06);
}

.http-test-script-error strong {
  color: var(--error);
  font-size: var(--text-sm);
}

.http-test-script-error code {
  color: var(--text-secondary);
  font-family: var(--font-mono);
  font-size: var(--text-xs);
  overflow-wrap: anywhere;
}

.http-test-detail {
  margin-top: var(--space-sm);
}

.http-test-detail summary {
  color: var(--accent);
  font-size: var(--text-sm);
  cursor: pointer;
}

.http-test-detail dl {
  display: grid;
  grid-template-columns: 76px minmax(0, 1fr);
  gap: 6px 10px;
  margin: var(--space-sm) 0 0;
}

.http-test-detail dt {
  color: var(--text-muted);
  font-size: var(--text-xs);
}

.http-test-detail dd {
  min-width: 0;
  margin: 0;
}

.http-test-detail dd code {
  display: block;
  max-height: 120px;
  overflow: auto;
  white-space: pre-wrap;
  overflow-wrap: anywhere;
  color: var(--text-primary);
  font-family: var(--font-mono);
  font-size: var(--text-xs);
}

.http-test-detail-note {
  margin: var(--space-sm) 0 0;
  color: var(--text-muted);
  font-size: var(--text-xs);
}

.http-test-detail-note code {
  font-family: var(--font-mono);
}

@media (max-width: 768px) {
  .channel-cards {
    grid-template-columns: 1fr;
  }

  .http-url-row {
    grid-template-columns: 1fr;
  }

  .http-test-detail dl {
    grid-template-columns: 1fr;
  }
}
</style>
