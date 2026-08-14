/**
 * 配置方案状态与操作（单例）。
 * 替代原 profileData + profileMethods。
 */

import { ref } from "vue";
import type { Profile, ProfileListResponse, NetworkDetectResult } from "../api/types";
import { profilesApi } from "../api";
import { extractApiError } from "../api/client";
import { DEFAULT_PROFILE_SETTINGS } from "../utils/constants";
import { frontendLogger } from "../utils/logger";
import { useStatus } from "./useStatus";
import { useToast } from "./useToast";
import { useConfirm } from "./useConfirm";

export type EditingProfile = Profile & { id: string; _isNew: boolean };

const profiles = ref<Record<string, Profile>>({});
const activeProfileId = ref("default");
const autoSwitch = ref(true);
const editingProfile = ref<EditingProfile | null>(null);
// dirty 机制：记录打开编辑器时的原始快照，用于检测未保存改动（历史遗留 F4/F5）
let editingProfileSnapshot = "";
const detectResult = ref<NetworkDetectResult | null>(null);
const editorDetectResult = ref<NetworkDetectResult | null>(null);

const { busy } = useStatus();
const { toastOnly } = useToast();
const { confirm } = useConfirm();

async function fetchProfiles(): Promise<void> {
  try {
    const data = await profilesApi.list();
    Object.keys(profiles.value).forEach((k) => delete profiles.value[k]);
    Object.assign(profiles.value, data.profiles || {});
    activeProfileId.value = data.active_profile || "default";
    autoSwitch.value = data.auto_switch !== false;
  } catch (error) {
    frontendLogger.error("profiles", "获取方案列表失败", error);
  }
}

async function showProfileEditor(profileId?: string): Promise<void> {
  // 打开新编辑器前先检查当前是否有未保存改动，避免静默丢弃（历史遗留 F5）
  if (!(await confirmDiscardIfDirty())) return;
  editorDetectResult.value = null;
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
  editingProfileSnapshot = snapshotOf(editingProfile.value);
}

/** 计算当前编辑中的方案是否存在未保存改动。 */
function snapshotOf(p: EditingProfile | null): string {
  return p ? JSON.stringify(p) : "";
}

/** 当前编辑器是否有未保存改动。 */
function isProfileDirty(): boolean {
  return (
    editingProfile.value !== null &&
    snapshotOf(editingProfile.value) !== editingProfileSnapshot
  );
}

/** 若存在未保存改动，弹窗确认是否放弃；无改动则直接放行。 */
async function confirmDiscardIfDirty(): Promise<boolean> {
  if (!isProfileDirty()) return true;
  return confirm({
    title: "放弃未保存的修改",
    message: "当前配置方案有未保存的修改，确定放弃吗？",
    danger: true,
  });
}

/** 关闭编辑器（带 dirty 确认）。 */
async function closeProfileEditor(): Promise<void> {
  if (!(await confirmDiscardIfDirty())) return;
  editingProfile.value = null;
  editingProfileSnapshot = "";
}

async function saveProfile(): Promise<void> {
  if (!editingProfile.value) return;
  const profile = editingProfile.value;
  const profileId = profile.id.trim();
  if (!profileId) {
    frontendLogger.warn("profiles", "保存方案被拒绝: 空 ID");
    toastOnly(false, "请输入方案 ID");
    return;
  }
  if (!/^[a-zA-Z0-9_]+$/.test(profileId)) {
    frontendLogger.warn("profiles", "保存方案被拒绝: ID 格式无效");
    toastOnly(false, "方案 ID 只能包含字母、数字和下划线");
    return;
  }
  const { id, _isNew, ...settings } = profile;
  try {
    const data = await profilesApi.save(profileId, settings as Profile);
    frontendLogger.info("profiles", "方案保存成功: " + profileId);
    toastOnly(true, data?.message || "方案保存成功");
    editingProfile.value = null;
    editingProfileSnapshot = "";
    await fetchProfiles();
    if (profileId === activeProfileId.value) {
      await refreshActiveProfileConfig();
    }
  } catch (error) {
    const msg = extractApiError(error, "保存失败");
    frontendLogger.error("profiles", "方案保存异常: " + msg, error);
    toastOnly(false, msg);
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
      editingProfileSnapshot = "";
    }
    await fetchProfiles();
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
  const { useConfig } = await import("./useConfig");
  const { dirty, fetchConfig } = useConfig();
  if (dirty.value) {
    const ok = await confirm({
      title: "未保存的修改",
      message: "当前设置有未保存的修改，加载方案配置将覆盖它们。确定继续吗？",
    });
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
    fetchProfiles,
    showProfileEditor,
    saveProfile,
    deleteProfile,
    setActiveProfile,
    detectNetworkForEditor,
    detectNetwork,
    toggleAutoSwitch,
    isProfileDirty,
    closeProfileEditor,
  };
}
