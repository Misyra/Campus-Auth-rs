<script setup lang="ts">
import IconApp from "@/components/common/IconApp.vue";
import { onMounted, ref, watch } from "vue";
import { useProfiles } from "@/composables/useProfiles";
import { useStatus } from "@/composables/useStatus";
import { CARRIER_OPTIONS } from "@/utils/constants";
import CustomSelect from "@/components/common/CustomSelect.vue";
import type { SelectOption } from "@/components/common/CustomSelect.vue";

const p = useProfiles();
const { busy } = useStatus();

onMounted(() => { void p.fetchProfiles(); });

// 编辑模式：true = 显示编辑器，false = 显示列表
const showEditor = ref(false);

// 自定义运营商：独立状态控制输入框显隐（修复 P1-17 敲第一个字符输入框即消失）。
// 逻辑统一为：isp 非空且不在预设值列表 → 视为"自定义"（含"自定义"开关项与已加载的自定义关键字）。
const showCustomCarrier = ref(false);
// 运营商预设值（不含空与"自定义"开关项）
const carrierPresetValues = CARRIER_OPTIONS.map((o) => o.value).filter((v) => v !== "" && v !== "自定义");
watch(
  () => p.editingProfile.value?.isp,
  (val) => {
    const isp = val || "";
    showCustomCarrier.value = isp !== "" && !carrierPresetValues.includes(isp);
  },
  { immediate: true },
);

async function openEditor(profileId: string | null) {
  await p.showProfileEditor(profileId ?? undefined);
  // 仅当编辑器真正打开（未被 dirty 确认拦截）时才切到编辑视图
  if (p.editingProfile.value) showEditor.value = true;
}

async function closeEditor() {
  // 带未保存确认：若用户取消放弃，则保留在编辑视图（历史遗留 F4/F5）
  await p.closeProfileEditor();
  if (!p.editingProfile.value) showEditor.value = false;
}

async function saveAndClose() {
  // 保存成功才关闭编辑器；失败/校验拒绝时保持打开（数据仍在）
  const ok = await p.saveProfile();
  if (ok) showEditor.value = false;
}

// carrierOptions → SelectOption[]
const carrierOptions: SelectOption[] = CARRIER_OPTIONS;
</script>

<template>
  <div class="page-content">
    <!-- ===== 编辑器模式 ===== -->
    <template v-if="showEditor && p.editingProfile.value">
      <div class="profile-editor-topbar">
        <button class="btn btn-sm" @click="closeEditor">
          <IconApp name="arrow-left" class="icon-sm" />
          返回方案列表
        </button>
        <h2>{{ p.editingProfile.value._isNew ? '新建方案' : '编辑方案' }}</h2>
        <div></div>
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
                <span class="hint">字母、数字、下划线</span>
              </div>
              <div class="form-group">
                <label for="prof-name">方案名称</label>
                <input id="prof-name" v-model="p.editingProfile.value.name" type="text" placeholder="宿舍 WiFi" />
              </div>
            </div>
          </div>

          <!-- 网络匹配 -->
          <div class="editor-section">
            <div class="editor-section-label">
              网络匹配
              <span class="field-help" tabindex="0" role="note" data-tip="设置匹配规则后，自动切换时会根据当前网络环境选择对应方案。两项都留空则仅手动切换。">?</span>
            </div>
            <div class="editor-network-detect">
              <button class="btn btn-sm" @click="p.detectNetworkForEditor()" :disabled="busy.editorDetect">
                <IconApp name="globe" class="icon-sm" />
                {{ busy.editorDetect ? '检测中...' : '检测当前网络' }}
              </button>
              <span v-if="p.editorDetectResult.value" class="editor-detect-info">
                <span v-if="p.editorDetectResult.value.gateway_ip" class="editor-detect-tag">
                  网关 <code>{{ p.editorDetectResult.value.gateway_ip }}</code>
                  <button class="btn-link" @click="p.editingProfile.value.gateway_ip = p.editorDetectResult.value.gateway_ip">填入</button>
                </span>
                <span v-if="p.editorDetectResult.value.ssid" class="editor-detect-tag">
                  SSID <code>{{ p.editorDetectResult.value.ssid }}</code>
                  <button class="btn-link" @click="p.editingProfile.value.wifi_ssid = p.editorDetectResult.value.ssid">填入</button>
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

          <!-- 账号凭证 -->
          <div class="editor-section">
            <div class="editor-section-label">账号凭证</div>
            <div class="profile-credentials-section">
              <div class="form-row">
                <div class="form-group">
                  <label for="prof-username">独立账号</label>
                  <input id="prof-username" v-model.trim="p.editingProfile.value.username" type="text" placeholder="留空使用全局" />
                </div>
                <div class="form-group">
                  <label for="prof-password">独立密码</label>
                  <input id="prof-password" v-model="p.editingProfile.value.password" type="password"
                    :placeholder="p.editingProfile.value.password && p.editingProfile.value.password.startsWith('•') ? '已保存，清空可更新' : '留空使用全局'"
                    @focus="($event.target as HTMLInputElement).select()" />
                  <span class="hint">密码不会随配置切换导出，仅在本机生效</span>
                </div>
              </div>
              <div class="form-row">
                <div class="form-group">
                  <label for="prof-carrier">运营商</label>
                  <CustomSelect v-model="p.editingProfile.value.isp" :options="carrierOptions" />
                </div>
                <div v-if="showCustomCarrier" class="form-group">
                  <label for="prof-carrier-custom">自定义运营商</label>
                  <input id="prof-carrier-custom" v-model.trim="p.editingProfile.value.isp" type="text" placeholder="校园专网" />
                </div>
              </div>
            </div>
          </div>

          <!-- 认证设置 -->
          <div class="editor-section">
            <div class="editor-section-label">认证设置</div>
            <div class="form-group">
              <label for="prof-auth-url">认证地址</label>
              <input id="prof-auth-url" v-model.trim="p.editingProfile.value.auth_url" type="text" placeholder="http://（重定向模式可留空）" />
            </div>
            <div class="form-group">
              <label for="prof-trigger-url">重定向触发地址</label>
              <input id="prof-trigger-url" v-model.trim="p.editingProfile.value.trigger_url" type="text" placeholder="http://captive.apple.com/hotspot-detect.html（直连留空）" />
              <span class="hint">仅劫持型门户填写：明文 http 地址，Worker 跟随 302 到真门户</span>
            </div>
          </div>
        </div>
        <div class="card-footer">
          <button class="btn btn-secondary" @click="closeEditor">取消</button>
          <button class="btn btn-primary" @click="saveAndClose">保存方案</button>
        </div>
      </div>
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

      <!-- 空状态 -->
      <div v-if="!Object.keys(p.profiles.value).length" class="card">
        <div class="profiles-empty">
          <IconApp name="wifi" :stroke-width="1.5" />
          <span class="profiles-empty-title">暂无配置方案</span>
          <span class="profiles-empty-desc">为不同网络环境创建独立的认证配置</span>
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
            </div>
          </div>
          <div class="profile-card-actions" @click.stop>
            <button class="btn btn-xs" @click="p.setActiveProfile(pid)"
              :disabled="p.activeProfileId.value === pid || p.autoSwitch.value"
              :title="p.autoSwitch.value ? '自动切换已开启，无法手动切换' : ''">
              {{ p.activeProfileId.value === pid ? '使用中' : (p.autoSwitch.value ? '自动' : '切换') }}
            </button>
            <button class="btn btn-xs" @click="openEditor(pid)">编辑</button>
            <button class="btn btn-xs btn-danger" @click="p.deleteProfile(pid)" :disabled="pid === 'default'">删除</button>
          </div>
        </div>
      </div>
    </template>
  </div>
</template>
