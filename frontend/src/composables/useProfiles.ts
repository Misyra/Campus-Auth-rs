/**
 * 配置方案状态与操作（单例）。
 * 替代原 profileData + profileMethods。
 */

import { ref } from "vue";
import type {
  Profile,
  ProfileSharePayload,
  ProfileSummary,
  ProfileUpdatePayload,
  NetworkDetectResult,
} from "../api/types";
import { profilesApi } from "../api";
import { extractApiError } from "../api/client";
import { DEFAULT_PROFILE_SETTINGS } from "../utils/constants";
import { pickFile } from "../utils/file";
import { createFetchGuard, createFirstFailNotifier } from "../utils/guards";
import { frontendLogger } from "../utils/logger";
import { useStatus } from "./useStatus";
import { useDirtySnapshot } from "./useDirtySnapshot";
import { useToast } from "./useToast";
import { useConfirm } from "./useConfirm";

/** 编辑中的方案草稿。`_` 前缀为编辑器专属字段，不参与 PUT 载荷。 */
export type EditingProfile = Profile & {
  id: string;
  _isNew: boolean;
  /**
   * 显式清除已保存密码（请求态，保存后生效）。
   *
   * 必须放在草稿对象内而非独立 ref：dirty 判定是草稿的 JSON 全量比对
   * （见 useDirtySnapshot），放在对象外则「只点了清除」不会产生未保存标记，
   * 用户离开时静默丢失该意图。也**不能**用 `password = ""` 表达清除——PUT 的
   * 空串契约是「保留原密码」，见 ProfileUpdatePayload.clear_password。
   */
  _clearPassword?: boolean;
};

const profiles = ref<Record<string, ProfileSummary>>({});
const activeProfileId = ref("default");
// 与后端 SettingsData::default 一致（2026-09-16 起默认关闭自动切换）；
// 加载后即以服务端下发为准，此处只是首帧占位
const autoSwitch = ref(false);
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
/**
 * 编辑中的方案是否已保存密码。
 *
 * 后端 GET 不回传密码（`settings.password` 恒为空串），故必须单独持有：
 * 编辑器的占位文案与「清除已保存密码」按钮的显隐都据此判定。口径与后端
 * 一致（反映「可解密」而非「非空」）。
 */
const editorHasPassword = ref(false);
/** 方案导入 in-flight：防连点导致重复导入同一条（后端会各分配一个 ID） */
const profileImporting = ref(false);

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
  if (profileId && profiles.value[profileId]) {
    try {
      const data = await profilesApi.get(profileId);
      editingProfile.value = {
        ...data.settings,
        id: profileId,
        _isNew: false,
        _clearPassword: false,
      } as EditingProfile;
      editorHasPassword.value = data.has_password === true;
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
      _clearPassword: false,
    } as EditingProfile;
    editorHasPassword.value = false;
  }
  // 记录初始快照作为 dirty 基准
  refreshProfileSnapshot();
}

/** 关闭编辑器（带 dirty 确认）。 */
async function closeProfileEditor(): Promise<void> {
  if (!(await confirmDiscardIfDirty())) return;
  editingProfile.value = null;
  editorHasPassword.value = false;
  resetProfileSnapshot();
}

/**
 * 打开**当前活跃方案**的编辑器；返回是否真的打开了。
 *
 * 供「方案」页进入时自动展示在用方案（改账号是这一页最高频的用途，
 * 先看列表再找卡片点「编辑」平白多两步）。
 *
 * 与 `showProfileEditor(id)` 的关键区别：**活跃方案不在已加载列表里时返回 false，
 * 绝不退化成新建草稿**。后者对缺失 id 的既有语义是「打开空白新建表单」
 * （`showProfileEditor` 的 else 分支），用作自动打开时，一旦方案列表尚未拉取成功
 * （`activeProfileId` 初始值恒为 `"default"` 而 `profiles` 为空），用户进入页面
 * 会看到一个空白表单，误以为配置丢了——比留在列表页更糟。故此处显式前置校验。
 */
async function openActiveProfileForEdit(): Promise<boolean> {
  // 已有草稿（上次离开本页时编辑器未关闭，或正在编辑另一个方案）→ 直接复用。
  // 不能重载：重载会走 confirmDiscardIfDirty 弹「放弃未保存的修改」，
  // 等于用户每次回到本页都被问一次是否丢弃。
  if (editingProfile.value) return true;
  const id = activeProfileId.value;
  if (!id || !profiles.value[id]) return false;
  await showProfileEditor(id);
  return editingProfile.value !== null;
}

/**
 * 标记「保存时清除已保存密码」。
 *
 * 只是请求态：写进草稿后由 dirty 快照照常标记未保存，点「保存方案」才落盘。
 * 立即清空输入框（密码不回传，框里本就没有真实值），避免用户看到掩码误以为仍保留。
 */
function requestClearPassword(): void {
  const profile = editingProfile.value;
  if (!profile) return;
  profile._clearPassword = true;
  profile.password = "";
}

/** 撤销「清除密码」请求（误点后可退回） */
function cancelClearPassword(): void {
  const profile = editingProfile.value;
  if (profile) profile._clearPassword = false;
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
  // 字符集必须与后端 `is_valid_profile_id` 一致（字母/数字/下划线/连字符）。
  // 曾只允许下划线：而 `create_profile` 的 slugify 会把 `_` 归一为 `-`，于是新建
  // `my_profile` 落盘成 `my-profile`，此后每次编辑保存都被这里拦下——方案一旦创建
  // 就再也改不动（ID 输入框还是 disabled 的，用户连改名的入口都没有）。
  // 引导导入的方案 id 同样来自该 slugify，故此处必须接受连字符。
  if (!/^[a-zA-Z0-9_-]+$/.test(profileId)) {
    frontendLogger.warn("profiles", "保存方案被拒绝: ID 格式无效");
    toastOnly(false, "方案 ID 只能包含字母、数字、下划线和连字符");
    return false;
  }
  const { id, _isNew, _clearPassword, ...settings } = profile;
  // 自定义运营商：选中"自定义"但未输入关键字时拒绝保存（修复 P1-17）
  if (settings.isp === "自定义") {
    toastOnly(false, "请填写自定义运营商关键字");
    return false;
  }
  // 直连 / 脚本渠道都没有可内置的兜底任务（直连的门户地址因人而异、脚本的登录逻辑
  // 只能自己写；浏览器渠道才有内置 default），故未绑定时直接拒绝保存：放过去只会
  // 在登录时才失败，且失败点在别处
  if (settings.login_channel === "http" && !String(settings.active_http_task ?? "").trim()) {
    toastOnly(false, "请为直连渠道选择一个直连任务（任务页 · 直连任务）");
    return false;
  }
  if (settings.login_channel === "script" && !String(settings.active_script_task ?? "").trim()) {
    toastOnly(false, "请为自定义脚本渠道选择一个脚本任务（任务页 · 脚本）");
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
        active_http_task: settings.active_http_task ?? "",
        active_script_task: settings.active_script_task ?? "",
      });
    } else {
      data = await profilesApi.save(profileId, {
        ...settings,
        // 显式清除已保存密码：不能靠 password="" 表达（那是「保留原密码」）
        clear_password: _clearPassword === true,
      } as ProfileUpdatePayload);
    }
    frontendLogger.info("profiles", "方案保存成功: " + profileId);
    toastOnly(true, data?.message || "方案保存成功");
    editingProfile.value = null;
    editorHasPassword.value = false;
    resetProfileSnapshot();
    await fetchProfiles(true);
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
  } catch (error) {
    frontendLogger.error("profiles", "切换方案异常", error);
    toastOnly(false, "切换方案失败");
  }
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

/** 导出方案为分享文件：拉取载荷后交由调用方下载（本函数不触碰 DOM） */
async function exportProfile(id: string): Promise<ProfileSharePayload | null> {
  try {
    const payload = await profilesApi.export(id);
    frontendLogger.info("profiles", `方案已导出: ${id}`);
    return payload;
  } catch (error) {
    const msg = extractApiError(error, "导出失败");
    frontendLogger.error("profiles", `方案导出失败: ${id}`, error);
    toastOnly(false, msg);
    return null;
  }
}

/** 导入分享的方案文件；成功返回后端分配的实际 ID（冲突时已自动改名） */
async function importProfile(payload: unknown): Promise<string | null> {
  if (profileImporting.value) return null;
  profileImporting.value = true;
  try {
    const { id, legacy_http_config_dropped } = await profilesApi.import(
      payload as ProfileSharePayload,
    );
    frontendLogger.info("profiles", `方案已导入: ${id}`);
    await fetchProfiles(true);
    // 旧版分享文件里的直连配置（方案内联 http_* 字段）在 v10 已无法承载：
    // 后端忽略它们，这里必须说一声，否则用户以为导入后直连还能用
    if (legacy_http_config_dropped) {
      toastOnly(false, "该分享文件包含旧版直连配置，已忽略；请在「任务 · 直连任务」里重新配置");
    }
    return id;
  } catch (error) {
    const msg = extractApiError(error, "导入失败");
    frontendLogger.error("profiles", "方案导入失败", error);
    toastOnly(false, msg);
    return null;
  } finally {
    profileImporting.value = false;
  }
}

/** 读取用户选择的 JSON 文件并解析（取消/非法 JSON 返回 null，由调用方决定提示） */
async function readShareFile(): Promise<{ payload: unknown; fileName: string } | null> {
  const file = await pickFile("application/json,.json");
  if (!file) return null;
  try {
    const text = await file.text();
    return { payload: JSON.parse(text) as unknown, fileName: file.name };
  } catch (error) {
    frontendLogger.error("profiles", "分享文件解析失败", error);
    toastOnly(false, "无法解析该文件，请确认是本应用导出的方案文件");
    return null;
  }
}

export function useProfiles() {
  return {
    profiles,
    activeProfileId,
    autoSwitch,
    editingProfile,
    editorHasPassword,
    requestClearPassword,
    cancelClearPassword,
    detectResult,
    editorDetectResult,
    fetchProfiles,
    showProfileEditor,
    openActiveProfileForEdit,
    saveProfile,
    profileSaving,
    deleteProfile,
    setActiveProfile,
    detectNetworkForEditor,
    detectNetwork,
    toggleAutoSwitch,
    exportProfile,
    importProfile,
    readShareFile,
    profileImporting,
    isProfileDirty,
    closeProfileEditor,
  };
}
