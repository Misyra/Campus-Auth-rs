<script setup lang="ts">
/** 认证方案页：方案列表与编辑器、重定向检测及活动方案切换 */
import IconApp from "@/components/common/IconApp.vue";
import LoginChannelField from "@/components/common/LoginChannelField.vue";
import HttpLoginWizard from "@/components/common/HttpLoginWizard.vue";
import { computed, onMounted, ref, watch } from "vue";
import { useProfiles } from "@/composables/useProfiles";
import { useRedirectTest } from "@/composables/useRedirectTest";
import { useCarrierField } from "@/composables/useCarrierField";
import { useStatus } from "@/composables/useStatus";
import { CARRIER_OPTIONS, DEFAULT_TRIGGER_URL } from "@/utils/constants";
import { downloadBlob, isProfileSharePayload, shareFileName, unwrapSharePayload } from "@/utils/file";
import Modal from "@/components/common/Modal.vue";
import CustomSelect from "@/components/common/CustomSelect.vue";
import type { SelectOption } from "@/components/common/CustomSelect.vue";
import type { ProfileSharePayload } from "@/api/types";
import { useToast } from "@/composables/useToast";
import { frontendLogger } from "@/utils/logger";

const p = useProfiles();
const { busy } = useStatus();
const redirectTest = useRedirectTest();
const { toastOnly } = useToast();

/** 用草稿中的自定义触发地址（空值即内置默认）启动一次可见浏览器检测。 */
async function testRedirectForEditor(): Promise<void> {
  await redirectTest.testRedirect(p.editingProfile.value?.trigger_url ?? "");
}

/**
 * 进入本页时自动展示**当前活跃方案**的编辑器。
 *
 * 「方案」是账号、认证地址与登录方式的唯一入口（原「设置 · 账号」已并入），
 * 因此进入这一页最高频的用途就是改当前方案的账号；若先落列表再找卡片点「编辑」，
 * 每个用户每次都要多两步。返回列表后不会再次自动打开（本函数只在挂载时跑一次），
 * 用户选择「返回方案列表」的意图会被尊重。
 *
 * 必须 `await fetchProfiles()` 之后再尝试：方案列表为空时活跃方案无从加载，
 * `openActiveProfileForEdit` 会（有意地）返回 false 并留在列表页——
 * 见该函数对「绝不退化成新建草稿」的说明。
 */
onMounted(async () => {
  await p.fetchProfiles();
  if (await p.openActiveProfileForEdit()) showEditor.value = true;
});

// 编辑模式：true = 显示编辑器，false = 显示列表
const showEditor = ref(false);

// 直连登录配置向导：与编辑器共用同一份草稿（p.editingProfile），故关闭向导不丢改动
const showHttpWizard = ref(false);

/**
 * 「网络匹配」区折叠态：默认收起。
 *
 * 多数用户只有一个网络环境、用不到按网关/SSID 自动切换，展开会白占首屏。
 * 但**已配过匹配规则的方案必须默认展开**——收起会让人看不到自己设过的规则，
 * 以为丢了。故在打开编辑器时按草稿内容决定初值（见下 watch），而不是恒为 false。
 */
const matchExpanded = ref(false);

// 编辑器草稿出现/切换时重算折叠初值：有匹配规则就展开，否则收起。
// 只在"换了一份草稿"时重算——用户在同一次编辑里手动折叠后不受影响
// （否则任何字段改动都会把它弹回展开态，手动折叠形同虚设）。
watch(
  () => p.editingProfile.value,
  (profile) => {
    if (!profile) return;
    redirectTest.result.value = null;
    matchExpanded.value = Boolean(profile.gateway_ip?.trim() || profile.wifi_ssid?.trim());
  },
);

/** 折叠时的摘要：透出已配置的匹配规则，避免"看不见就以为没配" */
const matchSummary = computed(() => {
  const ep = p.editingProfile.value;
  if (!ep) return "";
  const parts: string[] = [];
  if (ep.gateway_ip?.trim()) parts.push(`网关 ${ep.gateway_ip.trim()}`);
  if (ep.wifi_ssid?.trim()) parts.push(`SSID ${ep.wifi_ssid.trim()}`);
  return parts.join(" · ");
});

const profileCarrier = computed<string>({
  get: () => p.editingProfile.value?.isp ?? "",
  set: (value) => {
    const profile = p.editingProfile.value;
    if (profile) profile.isp = value;
  },
});
const { showCustomCarrier, carrierSelection, customCarrierValue } = useCarrierField(profileCarrier);

async function openEditor(profileId: string | null) {
  await p.showProfileEditor(profileId ?? undefined);
  // 仅当编辑器真正打开（未被 dirty 确认拦截）时才切到编辑视图
  if (p.editingProfile.value) showEditor.value = true;
}

async function closeEditor() {
  // 带未保存确认：若用户取消放弃，则保留在编辑视图（历史遗留 F4/F5）
  await p.closeProfileEditor();
  if (!p.editingProfile.value) {
    showEditor.value = false;
    // 编辑器已关闭时向导失去草稿，必须一并关闭，否则留下悬浮在全屏遮罩上的空向导
    showHttpWizard.value = false;
  }
}

async function saveAndClose() {
  // 保存成功才关闭编辑器；失败/校验拒绝时保持打开（数据仍在）
  const ok = await p.saveProfile();
  if (ok) showEditor.value = false;
}

/** 编辑器顶部的方案切换下拉：在编辑器内直接换方案，不必退回列表再进来 */
const editorProfileOptions = computed<SelectOption[]>(() =>
  Object.entries(p.profiles.value).map(([id, info]) => ({
    value: id,
    label: `${info.name || id}${id === p.activeProfileId.value ? "（当前使用）" : ""}`,
  })),
);

/**
 * 编辑器内切换方案。
 *
 * 复用 `openEditor` 的既有语义：`showProfileEditor` 内部先做 dirty 确认，
 * 用户取消时 `editingProfile` 保持原样（仍指向旧方案），下拉显示随之回退——
 * 这里不额外回写，避免出现「下拉已变、实际还在编旧方案」的不一致。
 */
async function switchEditingProfile(id: string): Promise<void> {
  await openEditor(id);
}

// carrierOptions → SelectOption[]
const carrierOptions: SelectOption[] = CARRIER_OPTIONS;
/**
 * 用户可见的唯一登录网址。
 *
 * 兼容旧配置：历史上 `trigger_url` 非空代表显式开启重定向，即使同时保存了
 * `auth_url` 也不会使用后者，因此这里显示为空。用户一旦填写固定网址，就清掉
 * 旧触发值，使“填写=直接使用、留空=浏览器跟随重定向”成为唯一可见规则。
 */
const loginUrl = computed<string>({
  get: () => {
    const ep = p.editingProfile.value;
    if (!ep) return "";
    if (ep.login_channel === "browser" && ep.trigger_url?.trim()) return "";
    return ep.auth_url ?? "";
  },
  set: (value) => {
    const ep = p.editingProfile.value;
    if (!ep) return;
    ep.auth_url = value;
    if (ep.login_channel === "browser" && value.trim()) ep.trigger_url = "";
  },
});

/** 浏览器渠道下登录网址留空即使用重定向，不再另设模式开关 */
const followsRedirect = computed(
  () => p.editingProfile.value?.login_channel === "browser" && !loginUrl.value.trim(),
);

// ---- 方案分享（导出 / 导入） ----

/** 导出：拉取分享载荷并触发浏览器下载（账号/密码已在后端剔除） */
async function exportProfile(id: string): Promise<void> {
  const payload = await p.exportProfile(id);
  if (!payload) return;
  const info = p.profiles.value[id];
  downloadBlob(
    JSON.stringify(payload, null, 2),
    shareFileName(info?.name ?? "", id),
    "application/json",
  );
  toastOnly(true, "方案已导出（未含账号与密码，导入后需重新填写）");
}

/** 导入确认弹窗：null = 未打开。导入前先展示内容，尤其是有凭据变换脚本时 */
const importPreview = ref<{
  payload: ProfileSharePayload;
  fileName: string;
} | null>(null);

/** 选择文件 → 解析 → 弹确认预览（不直接导入） */
async function pickImportFile(): Promise<void> {
  const picked = await p.readShareFile();
  if (!picked) return;
  if (!isProfileSharePayload(picked.payload)) {
    frontendLogger.warn("profiles", "导入文件缺少方案标记");
    toastOnly(false, "不是有效的方案分享文件（缺少 campus_auth_profile 标记）");
    return;
  }
  // 形状已由守卫确认（标记 + profile 对象）；细粒度字段校验交给后端，
  // 前端只按需读取，避免在此重复实现一套契约
  importPreview.value = {
    payload: picked.payload as unknown as ProfileSharePayload,
    fileName: picked.fileName,
  };
}

/** 导入预览里的方案体（兼容 data 信封包裹） */
const importProfileBody = computed(() => {
  const root = unwrapSharePayload(importPreview.value?.payload);
  return (root?.profile ?? null) as Record<string, unknown> | null;
});

/** 预览里是否有会被执行的凭据变换脚本——导入他人方案等于执行他人 JS，必须显式提示 */
const importHasScript = computed(() => {
  const script = importProfileBody.value?.http_crypto_script;
  return typeof script === "string" && script.trim().length > 0;
});

const importing = ref(false);

/** 确认导入：交给后端分配 ID（冲突自动改名） */
async function confirmImport(): Promise<void> {
  if (!importPreview.value || importing.value) return;
  importing.value = true;
  try {
    const id = await p.importProfile(importPreview.value.payload);
    if (id) {
      toastOnly(true, `方案已导入：${id}（请补充账号与密码后再使用）`);
      importPreview.value = null;
    }
  } finally {
    importing.value = false;
  }
}
</script>

<template>
  <div class="page-content">
    <!-- ===== 编辑器模式 ===== -->
    <template v-if="showEditor && p.editingProfile.value">
      <div class="profiles-topbar profile-editor-topbar">
        <div class="profile-editor-topbar-left">
          <button class="btn btn-sm" @click="closeEditor">
            <IconApp name="arrow-left" class="icon-sm" />
            返回方案列表
          </button>
          <h2>{{ p.editingProfile.value._isNew ? '新建方案' : '编辑方案' }}</h2>
        </div>
        <!-- 方案切换器：编辑中直接换方案，无需退回列表找卡片。新建草稿不显示 -->
        <CustomSelect
          v-if="!p.editingProfile.value._isNew"
          :model-value="p.editingProfile.value.id"
          :options="editorProfileOptions"
          compact
          title="切换到其它方案"
          class="profile-editor-switch"
          @update:model-value="switchEditingProfile"
        />
        <span
          v-if="!p.editingProfile.value._isNew && p.editingProfile.value.id === p.activeProfileId.value"
          class="profile-badge active"
          title="自动登录使用这个方案的配置"
        >当前使用</span>
      </div>

      <div class="card profile-editor-card">
        <div class="card-body profile-editor-body">
          <!-- 基本信息 -->
          <div class="editor-section">
            <div class="editor-section-label">基本信息</div>
            <div class="form-row">
              <div class="form-group">
                <label for="prof-id">方案 ID</label>
                <input id="prof-id" v-model="p.editingProfile.value.id" type="text" placeholder="dorm" :disabled="!p.editingProfile.value._isNew" />
                <span class="hint">字母、数字、下划线、连字符</span>
              </div>
              <div class="form-group">
                <label for="prof-name">方案名称</label>
                <input id="prof-name" v-model="p.editingProfile.value.name" type="text" placeholder="宿舍 WiFi" />
              </div>
            </div>
          </div>

          <!-- 网络匹配：默认折叠。多数用户只有一个网络环境、不用不到按网关/SSID
               自动切换，展开会白占首屏；已填过匹配规则的方案默认展开，否则用户看不到
               自己配过什么。折叠状态放在本组件（不持久化）——它只是当次的查看偏好。 -->
          <div class="editor-section editor-collapse-section" :class="{ 'editor-collapsed': !matchExpanded }">
            <div
              class="editor-section-label editor-collapse-header"
              role="button"
              tabindex="0"
              :aria-expanded="matchExpanded"
              @click="matchExpanded = !matchExpanded"
              @keydown.enter.prevent="matchExpanded = !matchExpanded"
              @keydown.space.prevent="matchExpanded = !matchExpanded"
            >
              网络匹配
              <span class="field-help" tabindex="0" role="note" data-tip="设置匹配规则后，自动切换时会根据当前网络环境选择对应方案。两项都留空则仅手动切换。">?</span>
              <!-- 折叠时把已配置的值透出来，避免"看不见就以为没配" -->
              <span v-if="!matchExpanded && matchSummary" class="editor-collapse-summary">{{ matchSummary }}</span>
              <IconApp name="chevron-down" class="editor-collapse-chevron" />
            </div>
            <div v-show="matchExpanded">
              <div class="editor-network-detect">
                <button class="btn btn-sm" @click="p.detectNetworkForEditor()" :disabled="busy.editorDetect">
                  <IconApp name="globe" class="icon-sm" />
                  {{ busy.editorDetect ? '检测中...' : '检测当前网络' }}
                </button>
                <span v-if="p.editorDetectResult.value" class="editor-detect-info">
                  <span v-if="p.editorDetectResult.value.gateway_ip" class="editor-detect-tag">
                    网关 <code>{{ p.editorDetectResult.value.gateway_ip }}</code>
                    <button class="btn btn-link" @click="p.editingProfile.value.gateway_ip = p.editorDetectResult.value.gateway_ip">填入</button>
                  </span>
                  <span v-if="p.editorDetectResult.value.ssid" class="editor-detect-tag">
                    SSID <code>{{ p.editorDetectResult.value.ssid }}</code>
                    <button class="btn btn-link" @click="p.editingProfile.value.wifi_ssid = p.editorDetectResult.value.ssid">填入</button>
                  </span>
                  <span v-if="!p.editorDetectResult.value.gateway_ip && !p.editorDetectResult.value.ssid" class="editor-detect-tag muted">未能获取网络信息</span>
                </span>
              </div>
              <div class="form-row">
                <div class="form-group">
                  <label for="prof-gateway">网关 IP</label>
                  <input id="prof-gateway" v-model.trim="p.editingProfile.value.gateway_ip" type="text" placeholder="192.168.1.1" />
                </div>
                <div class="form-group">
                  <label for="prof-ssid">WiFi 名称（SSID）</label>
                  <input id="prof-ssid" v-model.trim="p.editingProfile.value.wifi_ssid" type="text" placeholder="Campus-Dorm-5G" />
                </div>
              </div>
            </div>
          </div>

          <!-- 账号凭证 -->
          <div class="editor-section">
            <div class="editor-section-label">账号凭证</div>
            <div class="profile-credentials-section">
              <div class="form-row">
                <div class="form-group">
                  <label for="prof-username">账号</label>
                  <!-- 此前占位写「留空使用全局」是错的：没有全局账号，登录仅在方案之间
                       回退（resolve_profile），留空即校验失败。文案必须与实际行为一致。 -->
                  <input id="prof-username" v-model.trim="p.editingProfile.value.username" type="text" placeholder="学号 / 上网账号" />
                  <span class="hint">留空无法自动认证。</span>
                </div>
                <div class="form-group">
                  <label for="prof-password">密码</label>
                  <!-- 后端不回传密码：占位与「清除」按钮都以编辑器持有的 has_password 为准。
                       清除不能靠清空输入框表达——PUT 的空串契约是「保留原密码」。 -->
                  <input id="prof-password" v-model="p.editingProfile.value.password" type="password"
                    :placeholder="p.editingProfile.value._clearPassword
                      ? '保存后将清除已保存密码'
                      : (p.editorHasPassword.value
                        ? '已保存，留空保留；输入新密码则更新'
                        : (p.editingProfile.value._isNew ? '可留空，稍后在方案里填写' : '未设置密码'))"
                    :disabled="p.editingProfile.value._clearPassword === true"
                    @focus="($event.target as HTMLInputElement).select()" />
                  <button v-if="p.editorHasPassword.value && !p.editingProfile.value._clearPassword"
                    type="button" class="btn btn-danger-ghost btn-sm" @click="p.requestClearPassword()">
                    清除已保存密码
                  </button>
                  <button v-else-if="p.editingProfile.value._clearPassword"
                    type="button" class="btn btn-secondary btn-sm" @click="p.cancelClearPassword()">
                    撤销清除
                  </button>
                  <span class="hint" v-if="p.editingProfile.value._clearPassword">保存后该方案的密码将被清空，自动登录会提示缺密码。</span>
                  <span class="hint" v-else>密码加密保存于本机，不随方案导出。</span>
                </div>
              </div>
              <div class="form-row">
                <div class="form-group">
                  <label for="prof-carrier">运营商</label>
                  <CustomSelect v-model="carrierSelection" :options="carrierOptions" />
                </div>
                <div v-if="showCustomCarrier" class="form-group">
                  <label for="prof-carrier-custom">自定义运营商</label>
                  <input id="prof-carrier-custom" v-model.trim="customCarrierValue" type="text" placeholder="校园专网" />
                </div>
              </div>
            </div>
          </div>

          <!-- 认证设置 -->
          <div class="editor-section">
            <div class="editor-section-label">认证设置</div>
            <div v-if="p.editingProfile.value.login_channel === 'browser'" class="redirect-test-card">
              <div class="redirect-test-copy">
                <strong>先测试是否需要认证地址</strong>
                <span>请先退出校园网登录，再点击检测；检测时会打开一个浏览器窗口。</span>
                <span
                  v-if="redirectTest.result.value"
                  class="redirect-test-result"
                  :class="redirectTest.result.value.status"
                >{{ redirectTest.result.value.message }}</span>
              </div>
              <button
                type="button"
                class="btn btn-secondary btn-sm"
                :disabled="redirectTest.testing.value"
                @click="testRedirectForEditor"
              >
                <IconApp name="globe" class="icon-sm" />
                {{ redirectTest.testing.value ? '检测中…' : '重定向检测' }}
              </button>
            </div>
            <div class="form-group">
              <label for="prof-auth-url">认证地址（可选）</label>
              <input id="prof-auth-url" v-model.trim="loginUrl" type="text" placeholder="重定向检测成功时无需填写；无法重定向时手动填写" />
              <span class="hint" v-if="p.editingProfile.value.login_channel === 'http'">直连登录使用下方的直连请求地址；这里仅作为脚本抓取认证页的来源，通常可留空。</span>
              <span class="hint" v-else-if="followsRedirect">当前将打开默认触发地址并由浏览器跟随门户跳转；多数校园网无需填写。</span>
              <span class="hint" v-else>已填写时直接打开这个网址，不再经过重定向触发页。</span>
            </div>
            <details v-if="followsRedirect" class="redirect-advanced">
              <summary>重定向高级设置</summary>
              <div class="form-group">
                <label for="prof-trigger-url">自定义触发地址（可选）</label>
                <input id="prof-trigger-url" v-model.trim="p.editingProfile.value.trigger_url" type="text" :placeholder="DEFAULT_TRIGGER_URL" />
                <span class="hint">留空默认使用 <code>{{ DEFAULT_TRIGGER_URL }}</code>。非特殊网络无需修改；必须使用明文 http 才能被未认证网关劫持。</span>
              </div>
            </details>
          </div>

          <!-- 登录方式：由 LoginChannelField 承载（与设置页、引导向导共用） -->
          <div class="editor-section">
            <LoginChannelField
              v-if="p.editingProfile.value"
              v-model="p.editingProfile.value"
              :profile-id="p.editingProfile.value._isNew ? undefined : p.editingProfile.value.id"
              :username="p.editingProfile.value.username"
              :password="p.editingProfile.value.password"
              :auth-url="p.editingProfile.value.auth_url"
              show-guide
              @open-guide="showHttpWizard = true"
            />
          </div>
        </div>
        <div class="card-footer">
          <button class="btn btn-secondary" @click="closeEditor">取消</button>
          <button class="btn btn-primary" @click="saveAndClose" :disabled="p.profileSaving.value">保存方案</button>
        </div>
      </div>

      <!-- 直连登录配置向导：与编辑器共用同一草稿对象，关闭不丢改动 -->
      <HttpLoginWizard
        v-if="p.editingProfile.value"
        :open="showHttpWizard"
        :draft="p.editingProfile.value"
        :profile-id="p.editingProfile.value._isNew ? undefined : p.editingProfile.value.id"
        :username="p.editingProfile.value.username"
        :password="p.editingProfile.value.password"
        :auth-url="p.editingProfile.value.auth_url"
        @close="showHttpWizard = false"
      />
    </template>

    <!-- ===== 列表模式 ===== -->
    <template v-else>
      <div class="profiles-topbar card">
        <div class="profiles-topbar-left">
          <div class="profiles-status-icon" :class="p.autoSwitch.value ? 'on' : 'off'">
            <IconApp name="wifi" />
          </div>
          <div class="profiles-topbar-info">
            <h2>配置方案</h2>
            <p>{{ p.autoSwitch.value ? '自动切换已开启' : '自动切换已关闭' }} · {{ Object.keys(p.profiles.value).length }} 个方案</p>
          </div>
        </div>
        <div class="profiles-topbar-actions">
          <label class="toggle compact" title="开启后根据检测到的网关 IP 自动切换到匹配的方案">
            <input type="checkbox" :checked="p.autoSwitch.value" @change="p.toggleAutoSwitch()" />
            <span class="toggle-slider"></span>
            <span class="toggle-label">自动切换</span>
          </label>
          <button class="btn btn-sm" @click="p.detectNetwork()" :disabled="busy.detect" title="检测当前网络环境">
            <IconApp name="globe" class="icon-sm" />
            {{ busy.detect ? '检测中...' : '检测网络' }}
          </button>
          <button class="btn btn-sm" @click="pickImportFile" :disabled="p.profileImporting.value" title="从分享文件导入方案（不含账号密码，需导入后自行填写）">
            <IconApp name="upload" class="icon-sm" />
            导入方案
          </button>
          <button class="btn btn-sm btn-primary" @click="openEditor(null)">
            <IconApp name="plus" class="icon-sm" />
            新建方案
          </button>
        </div>
      </div>

      <!-- 检测结果 -->
      <div v-if="p.detectResult.value" class="detect-banner card" :class="p.detectResult.value.matched_profile_id ? 'matched' : 'unmatched'">
        <div class="detect-banner-icon">
          <IconApp :name="p.detectResult.value.matched_profile_id ? 'check-circle' : 'info'" />
        </div>
        <div class="detect-banner-info">
          <template v-if="p.detectResult.value.matched_profile_id">
            <strong>匹配方案: {{ p.detectResult.value.matched_profile_name || p.detectResult.value.matched_profile_id }}</strong>
          </template>
          <template v-else><strong>未匹配到任何方案</strong></template>
          <span class="detect-details">
            <span v-if="p.detectResult.value.gateway_ip">网关 {{ p.detectResult.value.gateway_ip }}</span>
            <span v-if="p.detectResult.value.gateway_ip && p.detectResult.value.ssid"> · </span>
            <span v-if="p.detectResult.value.ssid">SSID "{{ p.detectResult.value.ssid }}"</span>
          </span>
        </div>
        <button class="btn btn-icon-only btn-sm" @click="p.detectResult.value = null" title="关闭">
          <IconApp name="close" class="icon-sm" />
        </button>
      </div>

      <!-- 使用说明 -->
      <div class="profiles-guide card">
        <div class="profiles-guide-intro">
          <p>配置方案适用于<strong>在多个网络环境间切换</strong>的用户。每个方案可独立配置账号、认证地址和匹配规则。</p>
        </div>
        <div class="profiles-guide-steps">
          <div class="guide-step"><span class="guide-step-num">1</span><div class="guide-step-content"><strong>创建方案</strong><p>为不同网络分别创建配置方案</p></div></div>
          <div class="guide-step-divider"></div>
          <div class="guide-step"><span class="guide-step-num">2</span><div class="guide-step-content"><strong>设置匹配规则</strong><p>填写网关 IP 或 WiFi 名称，系统自动识别</p></div></div>
          <div class="guide-step-divider"></div>
          <div class="guide-step"><span class="guide-step-num">3</span><div class="guide-step-content"><strong>开启自动切换</strong><p>开启自动切换后，连接新网络时自动应用对应方案</p></div></div>
        </div>
      </div>

      <!-- 空状态：复用全局 .empty-state（misc.css），标题/描述走通用工具类 -->
      <div v-if="!Object.keys(p.profiles.value).length" class="card">
        <div class="empty-state">
          <IconApp name="wifi" :stroke-width="1.5" />
          <strong class="empty-title">暂无配置方案</strong>
          <span class="empty-desc">为不同网络环境创建独立的认证配置</span>
          <button class="btn btn-sm btn-primary" @click="openEditor(null)">创建第一个方案</button>
        </div>
      </div>

      <!-- 方案列表 -->
      <div v-else class="profiles-list">
        <div v-for="(info, pid) in p.profiles.value" :key="pid" class="profile-card" :class="{ active: p.activeProfileId.value === pid }">
          <div class="profile-card-main" @click="!p.autoSwitch.value && p.setActiveProfile(pid)" :class="{ disabled: p.autoSwitch.value && p.activeProfileId.value !== pid }">
            <div class="profile-card-header">
              <div class="profile-card-title">
                <span class="profile-card-name">{{ info.name || pid }}</span>
                <span v-if="p.activeProfileId.value === pid" class="profile-badge active">当前</span>
              </div>
              <span class="profile-card-id">{{ pid }}</span>
            </div>
            <div class="profile-card-meta">
              <span v-if="info.gateway_ip" class="profile-tag">
                <IconApp name="server" class="icon-xs" />
                {{ info.gateway_ip }}
              </span>
              <span v-if="info.wifi_ssid" class="profile-tag">
                <IconApp name="wifi" class="icon-xs" />
                {{ info.wifi_ssid }}
              </span>
              <span v-if="!info.gateway_ip && !info.wifi_ssid" class="profile-tag">
                <IconApp name="x-circle" class="icon-xs" />
                无匹配规则
              </span>
              <span class="profile-tag" :title="info.login_channel === 'http' ? '直连请求：不启动浏览器' : '浏览器自动化：按任务操作网页'">
                <IconApp :name="info.login_channel === 'http' ? 'globe' : 'chrome'" class="icon-xs" />
                {{ info.login_channel === 'http' ? '直连请求' : '浏览器' }}
              </span>
            </div>
          </div>
          <div class="profile-card-actions" @click.stop>
            <button class="btn btn-xs" @click="p.setActiveProfile(pid)"
              :disabled="p.activeProfileId.value === pid || p.autoSwitch.value"
              :title="p.autoSwitch.value ? '自动切换已开启，无法手动切换' : ''">
              {{ p.activeProfileId.value === pid ? '使用中' : (p.autoSwitch.value ? '自动' : '切换') }}
            </button>
            <button class="btn btn-xs" @click="openEditor(pid)">编辑</button>
            <button class="btn btn-xs" @click="exportProfile(pid)" title="导出为分享文件（不含账号与密码）">导出</button>
            <button class="btn btn-xs btn-danger" @click="p.deleteProfile(pid)" :disabled="pid === 'default'">删除</button>
          </div>
        </div>
      </div>
    </template>

    <!-- 导入确认：先展示将导入的内容（含脚本原文）再落盘 -->
    <Modal
      :open="!!importPreview"
      title="导入配置方案"
      @close="importPreview = null"
    >
      <div v-if="importPreview">
        <p class="import-subtitle">将从此文件导入方案</p>
        <div class="import-file-name">{{ importPreview.fileName }}</div>

        <div v-if="importProfileBody" class="import-summary">
          <span class="import-summary-name">{{ importProfileBody.name || '(未命名)' }}</span>
          <span class="import-summary-meta">
            {{ importProfileBody.login_channel === 'http' ? '直连请求（免 Python 与浏览器）' : '浏览器自动化' }}
            <template v-if="importProfileBody.wifi_ssid"> · WiFi {{ importProfileBody.wifi_ssid }}</template>
            <template v-if="importProfileBody.gateway_ip"> · 网关 {{ importProfileBody.gateway_ip }}</template>
          </span>
        </div>

        <div class="import-hint-box">
          导入后账号与密码为空，需要你填写自己的。方案名重复时会自动改名，不会覆盖已有方案。
        </div>

        <!-- 凭据变换脚本是会被执行的代码：导入他人方案前必须让用户看到原文 -->
        <div v-if="importHasScript" class="import-script-warn">
          <strong>此方案包含凭据变换脚本，导入后登录时会执行以下 JavaScript：</strong>
          <pre class="import-script">{{ importProfileBody?.http_crypto_script }}</pre>
          <span class="hint">脚本在无网络、无文件访问的沙箱中运行（仅可做计算），请确认来源可信。</span>
        </div>
      </div>

      <template #footer>
        <button class="btn btn-ghost btn-sm" @click="importPreview = null" :disabled="importing">取消</button>
        <button class="btn btn-primary btn-sm" @click="confirmImport" :disabled="importing">
          {{ importing ? '导入中...' : '确认导入' }}
        </button>
      </template>
    </Modal>
  </div>
</template>
