<script setup lang="ts">
import { computed, ref, onMounted, watch } from "vue";
import { useRouter } from "vue-router";
import { useConfig } from "@/composables/useConfig";
import { useProfiles } from "@/composables/useProfiles";
import CustomSelect from "@/components/common/CustomSelect.vue";
import type { SelectOption } from "@/components/common/CustomSelect.vue";
import IconApp from "@/components/common/IconApp.vue";
import FieldHelp from "@/components/common/FieldHelp.vue";
import { CARRIER_OPTIONS, DEFAULT_TRIGGER_URL } from "@/utils/constants";

const config = useConfig();
// 使用 useConfig 单例中的 password 字段（与 saveConfig 共用同一实例）
const passwordDisplay = config.passwordDisplay;
const passwordSaved = config.passwordSaved;
const editingPassword = config.editingPassword;
const onPasswordFocus = config.onPasswordFocus;
const onPasswordBlur = config.onPasswordBlur;
const onPasswordInput = config.onPasswordInput;
const { profiles, activeProfileId } = useProfiles();
const router = useRouter();

const carrierOptions: SelectOption[] = CARRIER_OPTIONS;
const currentProfileName = ref("");
onMounted(() => {
  currentProfileName.value = profiles.value[activeProfileId.value]?.name || activeProfileId.value || "默认方案";
});

// 自定义运营商：独立状态控制输入框显隐（修复 P1-17 敲第一个字符输入框即消失）。
// 逻辑统一为：isp 非空且不在预设值列表 → 视为"自定义"（含"自定义"开关项与已加载的自定义关键字）。
const showCustomCarrier = ref(false);
// 运营商预设值（不含空与"自定义"开关项）
const carrierPresetValues = CARRIER_OPTIONS.map((o) => o.value).filter((v) => v !== "" && v !== "自定义");
watch(
  () => config.config.credentials.isp,
  (val) => {
    const isp = val || "";
    showCustomCarrier.value = isp !== "" && !carrierPresetValues.includes(isp);
  },
  { immediate: true },
);
// 重定向模式开关：以 trigger_url 非空为唯一状态源；打开且为空时填默认触发地址，关闭则清空（=直连）
const redirectEnabled = computed({
  get: () => !!config.config.credentials.trigger_url,
  set: (v: boolean) => {
    config.config.credentials.trigger_url = v
      ? config.config.credentials.trigger_url || DEFAULT_TRIGGER_URL
      : "";
  },
});
</script>

<template>
  <div class="settings-panel-grid">
    <!-- 当前方案提示 -->
    <div class="current-profile-hint">
      <IconApp name="user" class="icon-sm" />
      <span>当前方案：<strong>{{ currentProfileName }}</strong></span>
      <a href="#" @click.prevent="router.push({ name: 'profiles' })" class="hint-link">管理方案</a>
    </div>

    <section class="card settings-panel">
      <div class="settings-card-header">
        <IconApp name="user" class="settings-card-icon" />
        <h2>账号配置</h2>
      </div>
      <div class="card-body">
        <div class="form-group">
          <div class="field-label-row">
            <label for="settings-username">账号</label>
            <FieldHelp text="学校分配的上网账号，通常为学号。留空将无法自动认证。" />
          </div>
          <input id="settings-username" v-model.trim="config.config.credentials.username" name="username" type="text" placeholder="学号" autocomplete="username" />
        </div>
        <div class="form-group">
          <div class="field-label-row">
            <label for="settings-password">密码</label>
            <FieldHelp text="加密保存于本地配置文件。更换时直接输入新密码并保存。" />
          </div>
          <input id="settings-password"
            :value="passwordDisplay"
            @input="onPasswordInput"
            @focus="onPasswordFocus()" @blur="onPasswordBlur()"
            name="password" type="password"
            :placeholder="passwordSaved ? '已保存，输入新密码可更换' : '输入上网密码'"
            autocomplete="current-password" />
          <span class="hint" v-if="passwordSaved && !editingPassword">密码已加密保存于本地，点击输入框可更换</span>
          <span class="hint" v-else-if="passwordSaved && editingPassword">为空则保留原密码，输入则替换为新密码</span>
          <span class="hint" v-else>首次设置，保存后加密存放于本地</span>
        </div>
        <div class="form-group">
          <div class="field-label-row">
            <label for="settings-auth-url">认证地址</label>
<FieldHelp text="校园网认证页面的地址，以 http:// 或 https:// 开头。重定向模式下可留空，仅填触发地址。" />
          </div>
          <input id="settings-auth-url" v-model.trim="config.config.credentials.auth_url" type="text" placeholder="https://auth.example.edu.cn" />
        </div>
        <div class="form-group">
          <div class="toggle-with-help">
            <label class="toggle toggle-help-inline">
              <input type="checkbox" v-model="redirectEnabled" />
              <span class="toggle-slider"></span>
              <span class="toggle-label">重定向模式（劫持型门户）</span>
            </label>
            <FieldHelp text="打开则自动填入默认触发地址并走重定向登录；关闭则清空触发地址回到直连。" />
          </div>
        </div>
        <div v-if="redirectEnabled" class="form-group">
          <div class="field-label-row">
            <label for="settings-trigger-url">重定向触发地址</label>
            <FieldHelp text="明文 http 探测地址，留空会回落默认值。填写后首导航到此地址并跟随 302 到真门户。" />
          </div>
          <input id="settings-trigger-url" v-model.trim="config.config.credentials.trigger_url" type="text" :placeholder="DEFAULT_TRIGGER_URL" />
        </div>
        <div class="form-group">
          <div class="field-label-row">
            <label for="settings-carrier">运营商</label>
            <FieldHelp text="仅当登录页包含运营商选项时需要选择。选“不选择”跳过该步骤；选“自定义”按关键字匹配。" />
          </div>
          <CustomSelect v-model="config.config.credentials.isp" :options="carrierOptions" />
        </div>
        <div v-if="showCustomCarrier" class="form-group">
          <label for="settings-carrier-custom">自定义运营商关键字</label>
          <input id="settings-carrier-custom" v-model.trim="config.config.credentials.isp" type="text" placeholder="例如：宿舍宽带" />
        </div>
      </div>
    </section>

    <!-- 自定义变量（已移除）：保留一行迁移提示，不再占用整卡 -->
    <p class="form-help-text">
      自定义变量功能已移除。如需在任务中使用变量，请在任务 JSON 的 <code>variables</code> 字段中直接定义。
    </p>
  </div>
</template>
