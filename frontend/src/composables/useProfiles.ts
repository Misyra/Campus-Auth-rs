/**
 * 配置方案状态与操作（单例）。
 * 替代原 profileData + profileMethods。
 */

import { ref } from "vue";
import type { HttpLoginTestResult, Profile, ProfileSummary, NetworkDetectResult } from "../api/types";
import { profilesApi } from "../api";
import { extractApiError } from "../api/client";
import { DEFAULT_PROFILE_SETTINGS } from "../utils/constants";
import { createFetchGuard, createFirstFailNotifier } from "../utils/guards";
import { frontendLogger } from "../utils/logger";
import { useStatus } from "./useStatus";
import { useDirtySnapshot } from "./useDirtySnapshot";
import { useToast } from "./useToast";
import { useConfirm } from "./useConfirm";
import { useConfig } from "./useConfig";

export type EditingProfile = Profile & { id: string; _isNew: boolean };

const profiles = ref<Record<string, ProfileSummary>>({});
const activeProfileId = ref("default");
const autoSwitch = ref(true);
const editingProfile = ref<EditingProfile | null>(null);
// dirty 机制：基准快照由 useDirtySnapshot 统一维护，用于检测未保存改动（历史遗留 F4/F5）
const {
  isDirty: isProfileDirty,
  confirmDiscardIfDirty,
  refreshSnapshot: refreshProfileSnapshot,
  resetSnapshot: resetProfileSnapshot,
} = useDirtySnapshot(editingProfile, { entityName: "配置方案" });
const detectResult = ref<NetworkDetectResult | null>(null);
const editorDetectResult = ref<NetworkDetectResult | null>(null);
const httpTestResult = ref<HttpLoginTestResult | null>(null);
const httpTestRunning = ref(false);

const { busy } = useStatus();
const { toastOnly } = useToast();
const { confirm } = useConfirm();

// F3：首次失败 toast 通知（参照 useStatus.fetchStatus 的首败 notify 模式）
const fetchProfilesFail = createFirstFailNotifier();

// P15：5 秒内已成功拉取则跳过（useUi.init 已拉全部数据，View mount / 路由往返
// 不再重复请求）。失败不记录时间戳以便重试；force: true 供变更后刷新 /
// 重连回调等显式刷新场景绕过守卫。
const fetchGuard = createFetchGuard(5000);

async function fetchProfiles(force = false): Promise<void> {
  if (!fetchGuard.shouldFetch(force)) return;
  try {
    const data = await profilesApi.list();
    Object.keys(profiles.value).forEach((k) => delete profiles.value[k]);
    Object.assign(profiles.value, data.profiles || {});
    activeProfileId.value = data.active_profile || "default";
    autoSwitch.value = data.auto_switch !== false;
    fetchGuard.markSuccess();
    fetchProfilesFail.trackRecovery();
  } catch (error) {
    frontendLogger.error("profiles", "获取方案列表失败", error);
    // F3：首次失败 toast 通知，后续失败保持静默（log-only）
    if (fetchProfilesFail.trackFailure()) {
      toastOnly(false, "加载配置方案失败");
    }
  }
}

async function showProfileEditor(profileId?: string): Promise<void> {
  // 打开新编辑器前先检查当前是否有未保存改动，避免静默丢弃（历史遗留 F5）
  if (!(await confirmDiscardIfDirty())) return;
  editorDetectResult.value = null;
  httpTestResult.value = null;
  if (profileId && profiles.value[profileId]) {
    try {
      const data = await profilesApi.get(profileId);
      editingProfile.value = {
        ...data.settings,
        id: profileId,
        _isNew: false,
      } as EditingProfile;
    } catch {
      frontendLogger.error("profiles", "加载方案失败: " + profileId);
      toastOnly(false, "加载方案失败");
      return;
    }
  } else {
    editingProfile.value = {
      ...DEFAULT_PROFILE_SETTINGS,
      id: "",
      _isNew: true,
    } as EditingProfile;
  }
  // 记录初始快照作为 dirty 基准
  refreshProfileSnapshot();
}

/** 关闭编辑器（带 dirty 确认）。 */
async function closeProfileEditor(): Promise<void> {
  if (!(await confirmDiscardIfDirty())) return;
  editingProfile.value = null;
  resetProfileSnapshot();
}

/** 保存请求 in-flight 标记：防连点并发两次 PUT（新建方案第二次会撞"已存在"） */
const profileSaving = ref(false);

async function saveProfile(): Promise<boolean> {
  if (profileSaving.value) return false;
  if (!editingProfile.value) return false;
  const profile = editingProfile.value;
  const profileId = profile.id.trim();
  if (!profileId) {
    frontendLogger.warn("profiles", "保存方案被拒绝: 空 ID");
    toastOnly(false, "请输入方案 ID");
    return false;
  }
  if (!/^[a-zA-Z0-9_]+$/.test(profileId)) {
    frontendLogger.warn("profiles", "保存方案被拒绝: ID 格式无效");
    toastOnly(false, "方案 ID 只能包含字母、数字和下划线");
    return false;
  }
  const { id, _isNew, ...settings } = profile;
  // 自定义运营商：选中"自定义"但未输入关键字时拒绝保存（修复 P1-17）
  if (settings.isp === "自定义") {
    toastOnly(false, "请填写自定义运营商关键字");
    return false;
  }
  if (settings.login_channel === "http" && !String(settings.http_url ?? "").trim()) {
    toastOnly(false, "请填写直连请求地址");
    return false;
  }
  profileSaving.value = true;
  try {
    let data;
    if (_isNew) {
      // 新建方案：必填 4 字段 + 编辑器同屏的可选设置一次带上
      //（后端 ProfileCreateBody 已扩展，缺失会导致网关/SSID/认证地址等静默丢失）
      data = await profilesApi.create(profileId, {
        id: profileId,
        name: settings.name ?? "",
        username: settings.username ?? "",
        password: settings.password ?? "",
        auth_url: settings.auth_url ?? "",
        trigger_url: settings.trigger_url ?? "",
        isp: settings.isp ?? "",
        gateway_ip: settings.gateway_ip ?? "",
        wifi_ssid: settings.wifi_ssid ?? "",
        active_task: settings.active_task ?? "",
        login_channel: settings.login_channel ?? "browser",
        http_method: settings.http_method ?? "GET",
        http_url: settings.http_url ?? "",
        http_headers: settings.http_headers ?? "",
        http_body: settings.http_body ?? "",
        http_success_pattern: settings.http_success_pattern ?? "",
        http_failure_pattern: settings.http_failure_pattern ?? "",
        http_crypto_script: settings.http_crypto_script ?? "",
      });
    } else {
      data = await profilesApi.save(profileId, settings as Profile);
    }
    frontendLogger.info("profiles", "方案保存成功: " + profileId);
    toastOnly(true, data?.message || "方案保存成功");
    editingProfile.value = null;
    resetProfileSnapshot();
    await fetchProfiles(true);
    if (profileId === activeProfileId.value) {
      await refreshActiveProfileConfig();
    }
    return true;
  } catch (error) {
    const msg = extractApiError(error, "保存失败");
    frontendLogger.error("profiles", "方案保存异常: " + msg, error);
    toastOnly(false, msg);
    return false;
  } finally {
    profileSaving.value = false;
  }
}

/** 用编辑器当前值发送一次直连测试；不会保存方案，也不会触发登录状态机。 */
async function testHttpLogin(): Promise<void> {
  const profile = editingProfile.value;
  if (!profile || httpTestRunning.value) return;
  if (!profile.http_url.trim()) {
    toastOnly(false, "请填写直连请求地址");
    return;
  }
  if (!profile.username.trim()) {
    toastOnly(false, "请填写独立账号后再测试");
    return;
  }

  httpTestRunning.value = true;
  httpTestResult.value = null;
  try {
    const result = await profilesApi.testHttpLogin({
      profile_id: profile._isNew ? undefined : profile.id,
      username: profile.username,
      password: profile.password,
      http_method: profile.http_method,
      http_url: profile.http_url,
      http_headers: profile.http_headers,
      http_body: profile.http_body,
      http_success_pattern: profile.http_success_pattern,
      http_failure_pattern: profile.http_failure_pattern,
      http_crypto_script: profile.http_crypto_script,
      auth_url: profile.auth_url,
      fetch_page: true,
    });
    httpTestResult.value = result;
    toastOnly(result.outcome === "success", result.message);
  } catch (error) {
    const message = extractApiError(error, "测试请求失败");
    frontendLogger.error("profiles", "直连登录测试异常: " + message, error);
    toastOnly(false, message);
  } finally {
    httpTestRunning.value = false;
  }
}

async function deleteProfile(profileId: string): Promise<void> {
  const ok = await confirm({
    title: "删除配置方案",
    message: "确定要删除这个配置方案吗？",
    danger: true,
  });
  if (!ok) return;
  try {
    await profilesApi.delete(profileId);
    frontendLogger.info("profiles", "方案删除成功: " + profileId);
    toastOnly(true, "方案删除成功");
    if (editingProfile.value?.id === profileId) {
      editingProfile.value = null;
      resetProfileSnapshot();
    }
    await fetchProfiles(true);
    if (!profiles.value[activeProfileId.value]) activeProfileId.value = "default";
  } catch (error) {
    frontendLogger.error("profiles", "方案删除异常", error);
    toastOnly(false, "删除方案失败");
  }
}

async function setActiveProfile(profileId: string): Promise<void> {
  if (autoSwitch.value) return;
  try {
    const data = await profilesApi.setActive(profileId);
    activeProfileId.value = profileId;
    frontendLogger.info("profiles", data?.message || `已切换到方案 ${profileId}`);
    toastOnly(true, data?.message || `已切换到方案 ${profileId}`);
    await refreshActiveProfileConfig();
  } catch (error) {
    frontendLogger.error("profiles", "切换方案异常", error);
    toastOnly(false, "切换方案失败");
  }
}

/**
 * 刷新活跃方案的配置到设置页。
 *
 * 若设置页存在未保存修改（dirty），fetchConfig 的整体覆盖会静默丢弃它们（历史遗留 F5），
 * 因此先弹确认；用户取消则不刷新，保留当前编辑内容。
 */
async function refreshActiveProfileConfig(): Promise<void> {
  const { dirty, fetchConfig } = useConfig();
  if (dirty.value) {
    const ok = await confirm({
      title: "未保存的修改",
      message: "当前设置有未保存的修改，加载方案配置将覆盖它们。确定继续吗？",
    });
    // 仅 true 才继续覆盖；取消/被抢占（null）都不刷新，保留当前编辑内容
    if (!ok) return;
  }
  await fetchConfig();
}

async function detectNetworkForEditor(): Promise<void> {
  await _detectNetwork(true, "editorDetect", editorDetectResult, "编辑器网络检测失败", {
    gateway_ip: null,
    ssid: null,
  });
}

async function detectNetwork(): Promise<void> {
  await _detectNetwork(false, "detect", detectResult, "网络检测失败", {
    gateway_ip: null,
    ssid: null,
    matched_profile_id: null,
  });
}

async function _detectNetwork(
  _editor: boolean,
  busyKey: "detect" | "editorDetect",
  resultKey: typeof detectResult | typeof editorDetectResult,
  errorLabel: string,
  fallback: NetworkDetectResult,
): Promise<void> {
  busy[busyKey] = true;
  (resultKey as typeof detectResult).value = null;
  try {
    const data = await profilesApi.detect();
    (resultKey as typeof detectResult).value = data;
  } catch (error) {
    (resultKey as typeof detectResult).value = fallback;
    frontendLogger.error("profiles", errorLabel, error);
  } finally {
    busy[busyKey] = false;
  }
}

let autoSwitchInFlight = false;
async function toggleAutoSwitch(): Promise<void> {
  if (autoSwitchInFlight) return;
  autoSwitchInFlight = true;
  const newState = !autoSwitch.value;
  try {
    const data = await profilesApi.toggleAutoSwitch(newState);
    autoSwitch.value = newState;
    if (data?.active_profile) activeProfileId.value = data.active_profile;
    frontendLogger.info("profiles", data?.message || "自动切换已设置");
    toastOnly(true, data?.message || "自动切换已设置");
  } catch (error) {
    frontendLogger.error("profiles", "切换自动切换异常", error);
    toastOnly(false, "自动切换设置失败");
  } finally {
    autoSwitchInFlight = false;
  }
}

export function useProfiles() {
  return {
    profiles,
    activeProfileId,
    autoSwitch,
    editingProfile,
    detectResult,
    editorDetectResult,
    httpTestResult,
    httpTestRunning,
    fetchProfiles,
    showProfileEditor,
    saveProfile,
    testHttpLogin,
    profileSaving,
    deleteProfile,
    setActiveProfile,
    detectNetworkForEditor,
    detectNetwork,
    toggleAutoSwitch,
    isProfileDirty,
    closeProfileEditor,
  };
}
