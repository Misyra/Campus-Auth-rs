<script setup lang="ts">
/**
 * 直连任务的请求参数表单（任务页编辑器与直连配置向导共用）。
 *
 * 只负责「请求形状」四组字段：请求地址 / 请求头与请求内容 / 成败判定 / 凭据变换脚本。
 * 账号、密码、认证地址**不在这里**——它们属于方案，故本组件对方案一无所知：
 * 方案编辑器只挑一个直连任务，字段编辑只有这一处入口（避免两处改同一份配置）。
 *
 * 说明文字与 LoginChannelField 同一口径：字段留在界面上，解释收进 `?` 气泡
 * （悬停或点击展开，见 `FieldHelp`）。
 */
import IconApp from "@/components/common/IconApp.vue";
import CustomSelect from "@/components/common/CustomSelect.vue";
import FieldHelp from "@/components/common/FieldHelp.vue";
import { computed, ref } from "vue";
import {
  HTTP_BODY_EXAMPLE,
  HTTP_CERT_POLICY_OPTIONS,
  HTTP_CRYPTO_BUILTINS,
  HTTP_CRYPTO_CTX_FIELDS,
  HTTP_CRYPTO_SCRIPT_SKELETON,
  HTTP_HEADERS_EXAMPLE,
  HTTP_MAC_FORMAT_NOTE,
  HTTP_METHOD_OPTIONS,
  HTTP_TEMPLATE_PLACEHOLDERS,
  certPolicyFromValue,
  certPolicyHint as certPolicyHintText,
  certPolicyToValue,
  isCredentialExposedViaGet,
} from "@/utils/loginChannel";
import type { HttpCertPolicy } from "@/utils/loginChannel";
import type { HttpTaskDraft } from "@/utils/httpTask";

const props = defineProps<{
  /** 直连任务草稿（原地修改，宿主按全量序列化比对即可感知未保存改动） */
  model: HttpTaskDraft;
}>();

/**
 * 字段说明文案集中在此（以 `\n` 分段，气泡按 pre-line 渲染换行）。
 *
 * 「为什么这么填」不进正文：面板字段多，解释铺开会把输入控件挤出首屏。
 */
const HELP = {
  url:
    "在浏览器里打开登录页 → F12 开发者工具 → Network / 网络 → 勾选「保留日志」→ " +
    "故意输错密码点一次登录 → 在请求列表里找到提交账号密码的那条请求，复制它的 URL 与参数。\n\n" +
    "地址中的账号、密码原样换成 {username} / {password} 占位符。",
  authUrl:
    "认证地址是门户登录页的地址（浏览器里打开的那个登录页），直连时作为脚本的 " +
    "ctx.auth_url，并在配置了加密脚本时用它抓取页面原文。\n\n" +
    "留空则回退用方案的认证地址（浏览器渠道也用它，两个渠道共用）；" +
    "导入别人分享的直连任务时通常要填上，否则得自己补认证页地址。",
  request:
    "GET 通常只需地址；POST 还要照抄表单内容，部分门户校验 Referer。\n\n" +
    "登录请求一般还要带 Content-Type: application/x-www-form-urlencoded，" +
    "从同一条请求的请求头里照抄即可。",
  headers: "每行一项，格式为「名称: 值」。GET 请求通常可以留空。",
  body: "照抄表单字段名，把值换成占位符，如 username={username}&password={password}。",
  placeholders:
    "地址、请求头与请求内容都支持这些占位符，发送前会被替换成真实值。\n\n" +
    "值原样替换、不做转义，特殊字符请在脚本中用 url_encode() 处理；" +
    "写错的占位符会原样发出去（便于在测试结果里发现）。\n\n" +
    HTTP_MAC_FORMAT_NOTE,
  cert:
    "校园网门户常用自签名证书。默认跟随「设置 · 浏览器」的「忽略 HTTPS 证书错误」，" +
    "与浏览器自动化保持一致；门户证书正常时可改为「严格校验」，" +
    "以降低携带凭据的请求被中间人截获的风险。",
  verdict:
    "填响应里出现的文字即可；留空则只要返回 2xx 就算成功。\n\n" +
    "判定顺序：先看失败关键字，再看成功关键字。门户响应恒为 HTTP 200 时" +
    "（JSONP 接口很常见）两个关键字都要填，否则凭据错误也会被判成成功。",
  success:
    "响应内容里出现这段文字即判定成功。留空时以 HTTP 2xx 判断，" +
    "但部分门户（如 Dr.COM / eportal）即使密码错误也返回 200，此时必须填写。",
  failure:
    "响应内容里出现这段文字即判定失败，并且不再重试（如密码错误这种重试无意义的结论）。" +
    "留空则不据此提前判失败。",
  scriptWhen:
    "门户要求密码加密（请求里的密码字段是一串看不懂的十六进制或 Base64），" +
    "或需要按本机 IP 算签名时才用。",
  scriptContract:
    "脚本要定义 transform(ctx)，返回对象的字段可直接用 {字段名} 占位符引用。\n\n" +
    "ctx 不含网络与文件访问能力；脚本最长执行 500 毫秒，超时即判为配置错误。" +
    "page 是认证地址的页面原文（抓取失败时为空串），local_ip / local_mac 取不到时为空串，" +
    "脚本需要自行容忍。isp 是方案的运营商字段原样透传（未选择时为空串），" +
    "门户侧的写法（如 Dr.COM 的 @cmcc 后缀）由脚本自行映射。",
  preRequest:
    "有些门户的登录请求要带一个先从别的接口取回的令牌（典型是 CSRF token），而且这个令牌" +
    "绑定连接：取令牌与发登录必须落在同一条 TCP 连接上，换连接服务器就回 " +
    "CSRF token mismatch。这里填的就是那次取令牌的请求——程序会用同一个连接紧接着发登录请求。\n\n" +
    "取值方式目前只支持 JSON：json:csrf_token 表示取响应里的 csrf_token 字段，" +
    "json:data.token 表示取嵌套字段；响应被 JSONP 包裹（dr1003({...})）时也能取到。\n\n" +
    "取到的值默认按字段名注册成占位符，登录请求的请求头与请求体里用 {csrf_token} 这样引用" +
    "（占位符名留空即取字段名，也可以自己改）。\n\n" +
    "用不到就留空，整块忽略。",
  logoutWhen:
    "反复登录时门户提示「IP 已在线」或「重复登录」的才需要：登录前先请求一次下线接口，" +
    "把旧会话踢掉再登录。\n\n" +
    "下线请求的成败不影响登录：接口地址不对、门户没开下线都只会记一条日志，登录照常进行。" +
    "下线接口常为 /logout、/para?name=logout 这类地址，可在门户页面找「注销 / 下线」按钮照抄。",
  logoutWait:
    "部分门户的下线是异步生效的：立即重连仍会被刚踢掉的旧会话占住。等 1~3 秒再发登录请求" +
    "能显著提高一次成功率；下线即时生效的门户保持 0 即可。",
} as const;

/** 表单控件 id 前缀：同页可能同时挂载多个实例（任务编辑器 + 向导） */
const uid = `http-task-${Math.random().toString(36).slice(2, 8)}`;

// 高级项（凭据变换脚本）默认收起：绝大多数门户不需要，展开后会占掉半屏
const scriptOpen = ref(false);

// 前置请求同属高级项（只有令牌绑连接的门户需要），收起策略与脚本一致
const preOpen = ref(false);

/** 已配置前置请求（地址非空即启用）时保持展开，避免收起遮蔽既有配置 */
const preHasContent = computed(() => props.model.pre_request_url.trim().length > 0);

// 退出登录同为高级项（只有「IP 已在线」类门户需要），收起策略一致
const logoutOpen = ref(false);

/** 已配置退出登录（地址非空即启用）时保持展开 */
const logoutHasContent = computed(() => props.model.logout_url.trim().length > 0);

/** 已展开过脚本区（或草稿里本来就有脚本）时保持展开，避免收起遮蔽既有配置 */
const scriptHasContent = computed(() => props.model.crypto_script.trim().length > 0);

const certPolicy = computed<string>(() => certPolicyFromValue(props.model.ignore_https_errors));

/** 证书气泡 = 固定口径 + 当前选择实际含义 */
const certHelp = computed(
  () =>
    `${HELP.cert}\n\n当前：${certPolicyHintText(certPolicyFromValue(props.model.ignore_https_errors))}`,
);

/** GET + 地址含 {password}：凭据落在查询串里，需要专门提示 */
const passwordInUrl = computed(() => isCredentialExposedViaGet(props.model.method, props.model.url));

function setCertPolicy(value: string): void {
  props.model.ignore_https_errors = certPolicyToValue(value as HttpCertPolicy);
}

function fillUrlExample(): void {
  props.model.url = "http://10.0.0.1/login?username={username}&password={password}";
}

function fillHeadersExample(): void {
  props.model.headers = HTTP_HEADERS_EXAMPLE;
}

function fillBodyExample(): void {
  props.model.body = HTTP_BODY_EXAMPLE;
}

function fillScriptSkeleton(): void {
  props.model.crypto_script = HTTP_CRYPTO_SCRIPT_SKELETON;
  scriptOpen.value = true;
}
</script>

<template>
  <div>
    <!-- ① 请求地址 -->
    <section class="http-step">
      <div class="http-step-head">
        <span class="http-step-num">1</span>
        <div class="http-step-title">
          <strong>填写门户的登录请求地址</strong>
          <FieldHelp :text="HELP.url" wide />
        </div>
      </div>

      <div class="form-row http-url-row">
        <div class="form-group http-method-field">
          <label :for="`${uid}-method`">方法</label>
          <CustomSelect :id="`${uid}-method`" v-model="model.method" :options="HTTP_METHOD_OPTIONS" />
        </div>
        <div class="form-group">
          <div class="field-label-row">
            <label :for="`${uid}-url`">请求地址</label>
            <button type="button" class="btn btn-link http-fill-btn" @click="fillUrlExample">填入示例</button>
          </div>
          <input :id="`${uid}-url`" v-model.trim="model.url" type="text"
            placeholder="http://10.0.0.1/login?username={username}&password={password}" />
        </div>
      </div>

      <!-- 认证地址：直连的 ctx.auth_url / 抓页来源。留空回退方案的同名字段，
           故与请求地址分开呈现——它不属于"这次登录请求"，而是"哪个登录页" -->
      <div class="form-group">
        <div class="field-label-row">
          <label :for="`${uid}-auth-url`">认证地址</label>
          <FieldHelp :text="HELP.authUrl" wide />
        </div>
        <input :id="`${uid}-auth-url`" v-model.trim="model.auth_url" type="text"
          placeholder="http://10.0.0.1/（可选，留空则用方案的认证地址）" />
      </div>

      <!-- 凭据进 URL 会落在网关/代理日志里：只在真的这么填了才提示，不折叠 -->
      <div v-if="passwordInUrl" class="note note--warn">
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
        <div class="http-step-title">
          <strong>补齐请求头与请求内容</strong>
          <FieldHelp :text="HELP.request" wide />
        </div>
      </div>

      <div class="form-group">
        <div class="field-label-row">
          <label :for="`${uid}-headers`">请求头</label>
          <FieldHelp :text="HELP.headers" />
          <button type="button" class="btn btn-link http-fill-btn" @click="fillHeadersExample">填入示例</button>
        </div>
        <textarea :id="`${uid}-headers`" v-model="model.headers" rows="3"
          :placeholder="HTTP_HEADERS_EXAMPLE"></textarea>
      </div>

      <div v-if="model.method === 'POST'" class="form-group">
        <div class="field-label-row">
          <label :for="`${uid}-body`">请求内容</label>
          <FieldHelp :text="HELP.body" />
          <button type="button" class="btn btn-link http-fill-btn" @click="fillBodyExample">填入示例</button>
        </div>
        <textarea :id="`${uid}-body`" v-model="model.body" rows="4"
          :placeholder="HTTP_BODY_EXAMPLE"></textarea>
      </div>

      <!-- 占位符速查：词表留在界面上（要照着抄进输入框），规则收进 ? -->
      <div class="http-template-row">
        <span class="http-template-label">占位符</span>
        <FieldHelp :text="HELP.placeholders" wide />
        <div class="chip-row chip-row--inline">
          <code v-for="ph in HTTP_TEMPLATE_PLACEHOLDERS" :key="ph" class="chip">{{ ph }}</code>
        </div>
      </div>

      <!-- 证书策略：https 门户自签证书时必需。默认跟随全局（与浏览器渠道同口径），
           显式收紧会让自签门户直连失败，故把三态讲清楚而不是简单开关。 -->
      <div class="form-group">
        <div class="field-label-row">
          <label :for="`${uid}-cert`">HTTPS 证书</label>
          <FieldHelp :text="certHelp" wide />
        </div>
        <CustomSelect
          :id="`${uid}-cert`"
          :model-value="certPolicy"
          :options="HTTP_CERT_POLICY_OPTIONS"
          @update:model-value="setCertPolicy"
        />
      </div>
    </section>

    <!-- ③ 成败判定 -->
    <section class="http-step">
      <div class="http-step-head">
        <span class="http-step-num">3</span>
        <div class="http-step-title">
          <strong>告诉程序怎么判断登录成功</strong>
          <FieldHelp :text="HELP.verdict" wide />
        </div>
      </div>

      <div class="form-row">
        <div class="form-group">
          <div class="field-label-row">
            <label :for="`${uid}-success`">成功关键字</label>
            <FieldHelp :text="HELP.success" />
          </div>
          <input :id="`${uid}-success`" v-model="model.success_pattern" type="text"
            placeholder="登录成功（留空则以 HTTP 2xx 判断）" />
        </div>
        <div class="form-group">
          <div class="field-label-row">
            <label :for="`${uid}-failure`">失败关键字</label>
            <FieldHelp :text="HELP.failure" flip />
          </div>
          <input :id="`${uid}-failure`" v-model="model.failure_pattern" type="text"
            placeholder="账号或密码错误" />
        </div>
      </div>
    </section>

    <!-- ④ 凭据变换（可选，默认收起） -->
    <section class="http-step http-step--advanced">
      <div class="http-advanced-row">
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
          </span>
        </button>
        <FieldHelp :text="HELP.scriptWhen" />
        <span v-if="scriptHasContent" class="http-advanced-badge">已配置</span>
      </div>

      <div v-show="scriptOpen || scriptHasContent" class="http-advanced-body">
        <div class="form-group">
          <div class="field-label-row">
            <label :for="`${uid}-script`">脚本内容</label>
            <FieldHelp :text="HELP.scriptContract" wide />
            <button type="button" class="btn btn-link http-fill-btn" @click="fillScriptSkeleton">填入骨架</button>
          </div>
          <textarea :id="`${uid}-script`" v-model="model.crypto_script" class="http-script-editor" rows="8"
            placeholder="function transform(ctx) {
  return { password: md5(ctx.password) };
}"></textarea>
        </div>

        <!-- 自动保存模式下没有"保存前确认"这个时机了（旧版在显式保存时弹一次），
             故把提示落在**字段旁边**：填了就看得见，而不是等出问题再回头找 -->
        <div v-if="model.crypto_script.trim()" class="note note--warn">
          <IconApp name="alert-triangle" class="icon-sm" />
          <span>
            这段脚本会在每次登录时执行（沙箱内运行，无网络与文件访问，最长 500 毫秒）。
            改一个字就会自动保存并立即生效——只在看得懂、或来源可信时才保留它。
          </span>
        </div>

        <div class="http-script-help">
          <div class="chip-row chip-row--inline">
            <span class="chip-row-label">可读入参 ctx</span>
            <code v-for="f in HTTP_CRYPTO_CTX_FIELDS" :key="f" class="chip">{{ f }}</code>
          </div>
          <div class="chip-row">
            <span class="chip-row-label">可用函数</span>
            <code v-for="fn in HTTP_CRYPTO_BUILTINS" :key="fn" class="chip chip--fn">{{ fn }}</code>
          </div>
        </div>
      </div>
    </section>

    <!-- ⑤ 前置请求（可选，默认收起）：CSRF 令牌绑连接的门户要"先取令牌再登录"。
         地址留空即整块忽略——不另设开关，避免"开关状态"与"字段内容"两份状态漂移 -->
    <section class="http-step http-step--advanced">
      <div class="http-advanced-row">
        <button
          type="button"
          class="http-advanced-toggle"
          :aria-expanded="preOpen || preHasContent"
          @click="preOpen = !preOpen"
        >
          <IconApp name="chevron-down" class="icon-sm http-advanced-arrow" :class="{ expanded: preOpen || preHasContent }" />
          <span class="http-step-num http-step-num--muted">5</span>
          <span class="http-advanced-copy">
            <strong>前置请求（可选）</strong>
          </span>
        </button>
        <FieldHelp :text="HELP.preRequest" wide />
        <span v-if="preHasContent" class="http-advanced-badge">已配置</span>
      </div>

      <div v-show="preOpen || preHasContent" class="http-advanced-body">
        <div class="form-row http-url-row">
          <div class="form-group http-method-field">
            <label :for="`${uid}-pre-method`">方法</label>
            <CustomSelect
              :id="`${uid}-pre-method`"
              v-model="model.pre_request_method"
              :options="HTTP_METHOD_OPTIONS"
            />
          </div>
          <div class="form-group">
            <label :for="`${uid}-pre-url`">请求地址</label>
            <input :id="`${uid}-pre-url`" v-model.trim="model.pre_request_url" type="text"
              placeholder="http://10.0.0.1/api/csrf-token（留空 = 不需要前置请求）" />
          </div>
        </div>

        <div class="form-row">
          <div class="form-group">
            <label :for="`${uid}-pre-extract`">取值方式</label>
            <input :id="`${uid}-pre-extract`" v-model.trim="model.pre_request_extract" type="text"
              placeholder="json:csrf_token" />
          </div>
          <div class="form-group">
            <label :for="`${uid}-pre-name`">占位符名</label>
            <input :id="`${uid}-pre-name`" v-model.trim="model.pre_request_name" type="text"
              placeholder="留空则用字段名（csrf_token）" />
          </div>
        </div>

        <div class="form-group">
          <label :for="`${uid}-pre-headers`">请求头</label>
          <textarea :id="`${uid}-pre-headers`" v-model="model.pre_request_headers" rows="2"
            placeholder="每行一项，如 X-Requested-With: XMLHttpRequest（通常可留空）"></textarea>
        </div>

        <div v-if="model.pre_request_method === 'POST'" class="form-group">
          <label :for="`${uid}-pre-body`">请求内容</label>
          <textarea :id="`${uid}-pre-body`" v-model="model.pre_request_body" rows="2"
            placeholder="POST 时照抄表单内容；GET 用不到"></textarea>
        </div>

        <p class="hint">
          取到的值在登录请求里用 <code>{csrf_token}</code> 这样引用（占位符名那一栏填什么，这里就写什么）；
          前置请求自己也能用 <code>{username}</code>、<code>{auth_url}</code> 等占位符。
        </p>
      </div>
    </section>

    <!-- ⑥ 退出登录（可选，默认收起）：「IP 已在线」类门户要"先踢旧会话再登录"。
         与前置请求同一套"地址留空即整块忽略"的口径，不另设开关。
         动作语义与前置请求不同：成败不判定，失败不拦登录——文案里讲清这点 -->
    <section class="http-step http-step--advanced">
      <div class="http-advanced-row">
        <button
          type="button"
          class="http-advanced-toggle"
          :aria-expanded="logoutOpen || logoutHasContent"
          @click="logoutOpen = !logoutOpen"
        >
          <IconApp name="chevron-down" class="icon-sm http-advanced-arrow" :class="{ expanded: logoutOpen || logoutHasContent }" />
          <span class="http-step-num http-step-num--muted">6</span>
          <span class="http-advanced-copy">
            <strong>退出登录请求（可选）</strong>
          </span>
        </button>
        <FieldHelp :text="HELP.logoutWhen" wide />
        <span v-if="logoutHasContent" class="http-advanced-badge">已配置</span>
      </div>

      <div v-show="logoutOpen || logoutHasContent" class="http-advanced-body">
        <div class="form-row http-url-row">
          <div class="form-group http-method-field">
            <label :for="`${uid}-logout-method`">方法</label>
            <CustomSelect
              :id="`${uid}-logout-method`"
              v-model="model.logout_method"
              :options="HTTP_METHOD_OPTIONS"
            />
          </div>
          <div class="form-group">
            <label :for="`${uid}-logout-url`">请求地址</label>
            <input :id="`${uid}-logout-url`" v-model.trim="model.logout_url" type="text"
              placeholder="http://10.0.0.1/logout（留空 = 不需要退出登录）" />
          </div>
        </div>

        <div class="form-row">
          <div class="form-group">
            <label :for="`${uid}-logout-wait`">下线后等待</label>
            <input :id="`${uid}-logout-wait`" v-model.number="model.logout_wait_secs" type="number"
              min="0" max="30" step="0.5" />
            <span class="hint">秒 · 下线异步生效的门户等 1~3 秒再登录</span>
          </div>
          <div class="form-group">
            <div class="field-label-row">
              <span class="http-logout-note">下线失败不影响登录</span>
              <FieldHelp :text="HELP.logoutWait" />
            </div>
            <span class="hint">请求发出即继续；门户不回或报错都只记一条日志</span>
          </div>
        </div>

        <div class="form-group">
          <label :for="`${uid}-logout-headers`">请求头</label>
          <textarea :id="`${uid}-logout-headers`" v-model="model.logout_headers" rows="2"
            placeholder="每行一项，通常可留空"></textarea>
        </div>

        <div v-if="model.logout_method === 'POST'" class="form-group">
          <label :for="`${uid}-logout-body`">请求内容</label>
          <textarea :id="`${uid}-logout-body`" v-model="model.logout_body" rows="2"
            placeholder="POST 时照抄下线表单内容，如 username={username}；GET 用不到"></textarea>
        </div>
      </div>
    </section>
  </div>
</template>

<style scoped>
/* 步骤块：左侧竖线把「编号 + 标题 + 字段」绑成一个视觉单元 */
.http-step {
  margin-top: var(--space-md);
  padding-left: 12px;
  border-left: 2px solid rgba(var(--accent-rgb), 0.25);
}

.http-step:first-of-type {
  margin-top: 0;
}

.http-step-head {
  display: flex;
  align-items: flex-start;
  gap: 10px;
  margin-bottom: var(--space-sm);
}

/* 步骤标题行：标题与其 `?` 说明气泡同排，气泡紧随文末（不再占一行正文） */
.http-step-title {
  display: flex;
  align-items: center;
  gap: 6px;
  min-width: 0;
}

.http-step-head strong {
  display: block;
  color: var(--text-primary);
  font-size: var(--text-base);
  font-weight: 600;
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

/* 风险提示改用全局 `.note .note--warn`（components/misc.css）——
   此处原是本文件私有的一份，与 HttpLoginWizard 的 `.wz-warn` 规则体逐字相同；
   只保留本文件特有的语境注释：它只在真的把凭据写进地址时出现，故保留整段文字
   而不折叠——它要拦的是用户看不到的后果（网关/代理日志留痕），藏进气泡就失去拦截力 */

/* 占位符速查：词表（要照着抄进输入框）留在界面上，规则收进 `?` 气泡 */
.http-template-row {
  display: flex;
  align-items: center;
  flex-wrap: wrap;
  gap: 6px;
  margin-bottom: var(--space-sm);
  padding: 8px 10px;
  border-radius: var(--radius-md);
  background: rgba(var(--slate-rgb), 0.05);
}

.http-template-label {
  color: var(--text-muted);
  font-size: var(--text-xs);
}

/* 词条视觉（.http-chip 家族）已收敛到全局 components/chip.css：
   scoped 样式无法跨组件复用，正是此前三份副本逐字重复的原因。
   现在模板直接引用 .chip / .chip--fn / .chip-row / .chip-row--inline / .chip-row-label。 */

/* ===== 高级项（凭据变换脚本） ===== */
.http-step--advanced {
  border-left-color: rgba(var(--slate-rgb), 0.25);
}

/* 折叠标题行：标题按钮按内容收缩，`?` 说明气泡与「已配置」徽标同排
   （气泡不进按钮内——嵌套可聚焦元素会让点击语义打架） */
.http-advanced-row {
  display: flex;
  align-items: center;
  gap: 8px;
}

.http-advanced-toggle {
  display: flex;
  align-items: center;
  flex: 0 1 auto;
  gap: 10px;
  min-width: 0;
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

/* 「下线失败不影响登录」说明行：与标签同排的弱化说明，不做成警示 */
.http-logout-note {
  color: var(--text-muted);
  font-size: var(--text-sm);
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

@media (max-width: 768px) {
  .http-url-row {
    grid-template-columns: 1fr;
  }
}
</style>
