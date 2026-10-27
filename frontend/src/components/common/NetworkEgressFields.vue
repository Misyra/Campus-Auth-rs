<script setup lang="ts">
/** 方案级校园网出口：登录、探测与 DNS 共用一份可保存草稿。 */
import { computed, ref, watch } from "vue";
import CustomSelect, { type SelectOption } from "./CustomSelect.vue";
import { monitorApi } from "@/api";
import { extractApiError } from "@/api/client";
import type { NetworkSettings, EgressAdapter } from "@/api/types";

const props = defineProps<{ modelValue?: NetworkSettings | null; scriptChannel?: boolean }>();
const emit = defineEmits<{ "update:modelValue": [value: NetworkSettings] }>();
const adapters = ref<EgressAdapter[]>([]);
const loading = ref(false);
const error = ref("");
const value = computed<NetworkSettings>(() => props.modelValue ?? { mode: "direct", interface_id: "", proxy_url: "", dns_servers: [] });
const modeOptions = computed<Array<SelectOption & { value: NetworkSettings["mode"] }>>(() => [
  { value: "direct", label: "直连（绕过系统代理）" },
  { value: "interface", label: "指定校园网网卡（IPv4）", disabled: props.scriptChannel },
  { value: "proxy", label: "指定 HTTP / SOCKS5 代理" },
  { value: "system", label: "系统默认（允许系统代理）" },
]);
const mode = computed({
  get: () => value.value.mode,
  set: (next: string) => {
    const option = modeOptions.value.find(option => option.value === next && !option.disabled);
    if (option) emit("update:modelValue", { ...value.value, mode: option.value, dns_servers: option.value === "interface" ? value.value.dns_servers : [] });
  },
});
const interfaceId = computed({ get: () => value.value.interface_id, set: (interface_id: string) => emit("update:modelValue", { ...value.value, interface_id }) });
const proxyUrl = computed({ get: () => value.value.proxy_url, set: (proxy_url: string) => emit("update:modelValue", { ...value.value, proxy_url }) });
const dnsText = computed({ get: () => value.value.dns_servers.join("\n"), set: (text: string) => emit("update:modelValue", { ...value.value, dns_servers: text.split(/[\n,]+/).map(s => s.trim()).filter(Boolean) }) });
const selected = computed(() => adapters.value.find(a => a.id.toLowerCase() === interfaceId.value.toLowerCase()));
const adapterOptions = computed<SelectOption[]>(() => [
  ...(interfaceId.value && !selected.value ? [{ value: interfaceId.value, label: "已保存网卡当前不可用（保留原选择）" }] : []),
  ...adapters.value.map(adapter => ({ value: adapter.id, label: `${adapter.name} · ${adapter.ipv4}` })),
]);

/** 只读取接口状态；刷新不自动更换用户选中的网卡。 */
async function refreshAdapters(): Promise<void> {
  if (loading.value) return;
  loading.value = true;
  error.value = "";
  try { adapters.value = await monitorApi.interfaces(); }
  catch (cause) { adapters.value = []; error.value = extractApiError(cause, "读取网卡失败，请重试"); }
  finally { loading.value = false; }
}
watch(mode, next => { if (next === "interface") void refreshAdapters(); }, { immediate: true });
</script>

<template>
  <div class="network-egress-fields">
    <div class="form-group">
      <label>校园网出口</label>
      <CustomSelect v-model="mode" :options="modeOptions" aria-label="校园网出口" />
      <span class="hint">登录与网络检测共用此出口，保存方案后生效。</span>
      <span v-if="scriptChannel" class="hint">脚本自行控制网络连接，无法强制绑定网卡；如需统一绑定，请使用浏览器或 HTTP 登录。</span>
    </div>
    <template v-if="mode === 'interface'">
      <div class="form-group">
        <label>校园网网卡</label>
        <div class="network-adapter-control">
          <CustomSelect v-model="interfaceId" :options="adapterOptions" aria-label="校园网网卡"
            :placeholder="loading ? '正在读取网卡…' : '请选择连接校园网的物理网卡'" :disabled="loading" />
          <button type="button" class="btn btn-secondary btn-sm" :disabled="loading" @click="refreshAdapters">{{ loading ? '读取中…' : '刷新网卡' }}</button>
        </div>
        <span v-if="error" class="hint" role="alert">{{ error }}</span>
        <span v-else-if="!loading && !adapters.length" class="hint">没有可用物理网卡，请检查校园网连接。</span>
        <span class="hint">网卡不可用时停止连接，不切换到其他网卡。</span>
      </div>
      <div class="form-group">
        <label>学校 DNS（可选，每行一个，最多 4 个）</label>
        <textarea v-model="dnsText" rows="2" aria-label="学校 DNS" placeholder="留空使用所选网卡的 DNS；也可填写学校提供的 IPv4 DNS" />
        <span class="hint">当前网卡 DNS：{{ selected?.dns_servers.length ? selected.dns_servers.join('、') : '未读取到，请手动填写学校 DNS' }}。如果网卡 DNS 已改成 1.1.1.1，请在此填学校 DNS。</span>
      </div>
      <p class="hint">VPN 仍可能拦截所选网卡，排查方法见配置指南。</p>
    </template>
    <div v-if="mode === 'proxy'" class="form-group">
      <label>代理地址</label>
      <input v-model.trim="proxyUrl" type="text" aria-label="代理地址" placeholder="http://127.0.0.1:7890" autocomplete="off" />
      <span class="hint">支持 HTTP / SOCKS5，代理须能访问校园网。</span>
    </div>
    <p v-if="mode === 'direct'" class="hint">直连绕过系统代理；使用 TUN 时可选择校园网网卡。</p>
    <p v-if="mode === 'system'" class="hint">代理可用不代表校园网已经认证。</p>
  </div>
</template>

<style scoped>
.network-adapter-control { display: flex; align-items: center; gap: var(--space-sm); }
.network-adapter-control > .custom-select { flex: 1; min-width: 0; }
.network-adapter-control > button { flex-shrink: 0; white-space: nowrap; }
@media (max-width: 600px) {
  .network-adapter-control { flex-direction: column; align-items: stretch; }
  .network-adapter-control > button { align-self: flex-start; }
}
</style>
