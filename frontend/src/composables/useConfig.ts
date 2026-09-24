/**
 * 全局设置状态与操作（单例）。
 * 替代原 configData + configMethods + 部分 autostart/OCR/日志级别方法。
 * 修复 P1-12.8：用显式 dirty 标志替代每次 JSON.stringify 全量序列化。
 *
 * **边界：本 composable 只承载 `GlobalConfig`（浏览器/检测/重试/日志/应用/更新器）。
 * 账号、认证地址、登录方式、直连参数都属于 Profile，一律在「配置方案」页编辑
 * （`useProfiles`），不在此处保留副本。**此前这里有一份 `credentials` 投影 +
 * 独立 `password` 实例，带来两个实缺陷：① `GET/PATCH /api/config` 把活跃方案的
 * 凭据摊平在顶层，界面上看似「全局账号」而实际写的是活跃方案，「账号混在全局
 * 保存栏」导致检测候选地址被任意 Tab 的保存顺带落盘（known-issues #23 E1）；
 * ② 同一份数据有了设置页与方案页两个可写入口，「改哪边才生效」无从判断。
 */

import { reactive, ref, watch, nextTick } from "vue";
import type { Config, SaveConfigPayload } from "../api/types";
import { configApi, autostartApi, pureModeApi } from "../api";
import { ApiError, extractApiError } from "../api/client";
import { DEFAULT_CONFIG } from "../utils/constants";
import { frontendLogger } from "../utils/logger";
import { createFetchGuard } from "../utils/guards";
import { useStatus } from "./useStatus";
import { CONFIG_RANGES, validateRangeValues } from "../utils/configRanges";
import { useToast } from "./useToast";

const config = reactive<Config>(structuredClone(DEFAULT_CONFIG));
const defaultUrlCheckUrls = [...DEFAULT_CONFIG.monitor.url_check_urls];
const dirty = ref(false);
const saveFailed = ref(false);
// F2：配置加载失败标记，为 true 时 SettingsView 保存按钮禁用并提示重试
const configLoadFailed = ref(false);

// 纯净模式（本质是 config.browser.pure_mode，API 为 /api/pure-mode，
// 从 useTasks 迁入：独立于表单 dirty 流程的即时开关状态）。
// 注意：开关与 config.browser.pure_mode 是同一后端字段的两个视图，
// 切换成功必须回写 config.browser（见 togglePureMode），否则 saveConfig
// 会用表单里的旧值把它覆盖回去。
const pureMode = ref(true);
const pureModeLoading = ref(false);

let loadingConfig = false;
let saveSeq = 0;
let saveAbort: AbortController | null = null;
// G20：fetchConfig 请求序号（epoch）守卫——并发/迟到的旧响应不得写入状态，
// 否则会无条件把 dirty 置 false 清掉用户编辑标记
let fetchConfigEpoch = 0;

// 深监听配置变更 → 与最近一次已保存快照比对得出 dirty（加载期间抑制）。
// 早先版本是单向闩锁（动过即 dirty=true，仅保存/重载复位），开关关了再打开
// 仍显示"已变更"；现改为快照比对：值回原样 dirty 自动消失。
// P12：回调仅做一次 JSON.stringify 比对（配置体量小，开销可忽略），无需防抖；
// flush 'post'（渲染后微任务批量执行）。异步化后 fetchConfig 需在复位 loadingConfig
// 前 await nextTick()，让加载期间的赋值在抑制窗口内跑完回调（见 fetchConfig 内注释）。
let savedSnapshot = JSON.stringify(config);
// 程序化写入（服务端已即时保存的日志级别等）期间抑制 dirty 比对，结束后同步快照
let suppressDirty = false;
watch(
  config,
  () => {
    if (loadingConfig || suppressDirty) return;
    dirty.value = JSON.stringify(config) !== savedSnapshot;
  },
  { deep: true, flush: "post" },
);

// password 曾在此单独监听以触发 dirty。账号字段迁往方案页后本 composable
// 不再持有密码，无需该监听（方案侧的 dirty 由 useDirtySnapshot 全量快照负责）。

const { busy } = useStatus();

/** 拉取后端配置并写入表单；加载期间抑制 dirty，成功后以加载结果为新快照 */
async function fetchConfig(): Promise<void> {
  // G20：仅最新一次请求可写状态；迟到的旧响应直接丢弃
  const epoch = ++fetchConfigEpoch;
  try {
    const data = await configApi.fetch();
    if (epoch !== fetchConfigEpoch) return;
    loadingConfig = true;
    config.browser = { ...DEFAULT_CONFIG.browser, ...(data.browser || {}) };
    config.worker = { ...DEFAULT_CONFIG.worker, ...(data.worker || {}) };
    config.monitor = { ...DEFAULT_CONFIG.monitor, ...(data.monitor || {}) };
    config.pause = { ...DEFAULT_CONFIG.pause, ...(data.pause || {}) };
    config.logging = { ...DEFAULT_CONFIG.logging, ...(data.logging || {}) };
    config.retry = { ...DEFAULT_CONFIG.retry, ...(data.retry || {}) };
    // 顶层还带着活跃方案的凭据与直连参数（后端扁平响应，兼容既有客户端）：
    // 本 composable 明确不接收——它们属于 Profile，归方案页编辑。
    config.app_settings = { ...DEFAULT_CONFIG.app_settings, ...(data.app_settings || {}) };
    config.updater = { ...DEFAULT_CONFIG.updater, ...(data.updater || {}) };
    // 旧配置只有 proxy_port（可能非默认值）：派生完整地址，
    // 保证输入框显示与后端 resolved_proxy_url 实际使用一致
    if (!config.updater.proxy_url && config.updater.proxy_port > 0) {
      config.updater.proxy_url = `http://127.0.0.1:${config.updater.proxy_port}`;
    }
    // P12：watch 已是异步 flush，上面的加载赋值会在微任务中触发回调；
    // 先等待一轮刷新（回调在 loadingConfig=true 窗口内执行完、不计入 dirty），
    // 再以加载结果为新快照，保证加载不被误标为未保存修改
    await nextTick();
    // G20：nextTick 窗口内若又有更新的 fetchConfig 接管，交由它负责复位状态
    if (epoch !== fetchConfigEpoch) return;
    loadingConfig = false;
    savedSnapshot = JSON.stringify(config);
    dirty.value = false;
    configLoadFailed.value = false;
    frontendLogger.info("config", "配置已加载");
  } catch (error) {
    // G20：迟到/被取代的旧请求失败同样不写状态（避免覆盖新请求的结果或误报）
    if (epoch !== fetchConfigEpoch) return;
    frontendLogger.error("config", "获取配置失败", error);
    // F2：首次失败 toast 提示
    configLoadFailed.value = true;
    toastOnly(false, "加载配置失败");
    // FE1-1：本请求已置位 loadingConfig（或赋值段抛错）时必须复位——
    // 此前 catch 不复位，「并发取代后接管的新请求又失败」会让 loadingConfig
    // 永久停留 true，dirty deep watch 被永久抑制，设置页保存按钮失效。
    // :102 的早退分支不复位是有意设计（被取代的请求由接管者负责）
    loadingConfig = false;
  }
}

/** 保存前配置校验：errors 为阻断性硬错误，warnings 为需要用户知悉的疑点 */
function validateConfig(): { errors: string[]; warnings: string[] } {
  const errors: string[] = [];
  const warnings: string[] = [];
  // 与后端 build_proxied_client 的校验口径一致
  const proxyUrl = config.updater.proxy_url;
  if (config.updater.use_proxy && proxyUrl && !/^https?:\/\//.test(proxyUrl)) {
    warnings.push("代理地址必须以 http:// 或 https:// 开头");
  }
  // 数值字段的区间校验统一走 utils/configRanges 的单一出处（含端口）。
  // 界面上的 min/max 拦不住手工输入与程序化赋值，只有这里才是真正的闸门。
  const rangeResult = validateRangeValues(rangeValuesFromConfig());
  errors.push(...rangeResult.errors);
  warnings.push(...rangeResult.warnings);
  return { errors, warnings };
}

/**
 * 从表单模型取出 `CONFIG_RANGES` 覆盖到的数值字段。
 *
 * 按表里的键动态取值（而不是手写一份映射），这样往表里加一个字段不需要再改这里；
 * 表单里不存在的键不会出现在结果里，`validateRangeValues` 会跳过。
 */
function rangeValuesFromConfig(): Record<string, unknown> {
  const bag = config as unknown as Record<string, Record<string, unknown>>;
  const out: Record<string, unknown> = {};
  for (const key of Object.keys(CONFIG_RANGES)) {
    const dot = key.indexOf(".");
    const group = key.slice(0, dot);
    const field = key.slice(dot + 1);
    const section = bag[group];
    if (section && typeof section === "object" && field in section) out[key] = section[field];
  }
  return out;
}

const { toastOnly } = useToast();

/** 保存配置：校验硬错误阻断、警示仅提示；成功后以表单当前值刷新 dirty 快照 */
async function saveConfig(force = false): Promise<void> {
  if (!dirty.value && !force) return;

  const { errors, warnings } = validateConfig();
  if (errors.length > 0) {
    // 硬错误阻断保存：仅写日志用户不可见，非法端口会静默保存成功
    frontendLogger.warn("config", errors.join("；"));
    toastOnly(false, errors.join("；"));
    return;
  }
  // 警示不阻断（格式存疑的 URL、未启用检测等由用户自行判断），
  // 但必须 toast 出来——嵌入场景下用户不看日志面板
  const hints = [...warnings];
  if (!config.monitor.enable_tcp_check && !config.monitor.enable_http_check && !config.monitor.enable_url_check) {
    hints.push("未启用任何网络检测方式，自动认证可能无法正常工作");
  }
  if (hints.length > 0) {
    frontendLogger.warn("config", hints.join("；"));
    toastOnly(false, hints.join("；"));
  }

  saveSeq++;
  const currentSeq = saveSeq;
  if (saveAbort) saveAbort.abort();
  saveAbort = new AbortController();
  const controller = saveAbort;

  busy.save = true;
  saveFailed.value = false;
  // 载荷只含全局设置：凭据/直连字段一律不提交（它们属 Profile，见文件头边界说明）。
  // 此前这里带着 username/auth_url/login_channel 等 13 个方案字段，等于让任意
  // 全局保存都能改写活跃方案的凭据——检测候选被静默落盘正是由此而来。
  const payload: SaveConfigPayload = {
    browser: config.browser,
    worker: config.worker,
    monitor: config.monitor,
    pause: config.pause,
    logging: config.logging,
    retry: config.retry,
    app_settings: config.app_settings,
    updater: config.updater,
  };

  // ⚠ 快照必须在 `await` **之前**取。
  // 这里等的是本次 PATCH 的载荷（payload 在上面已经构造好），所以"已提交的基准"
  // 就是此刻的 config；PATCH 在途期间用户继续编辑产生的改动**不属于**这次提交。
  // 原实现在 await 之后才用"当前 config"当快照，于是途中的编辑被当成已保存基准、
  // dirty 被置 false —— 实际从未提交，属静默丢改动。
  const submittedSnapshot = JSON.stringify(config);

  try {
    await configApi.patch(payload, { signal: controller.signal });
    suppressDirty = true;
    try {
      await nextTick();
      // 以**已提交值**为新快照：用户把值改回原样时 dirty 自动消失
      savedSnapshot = submittedSnapshot;
      // 只有"当前值与已提交值一致"才清 dirty。在途编辑必须保留未保存标记，
      // 否则用户看不到脏提示、也不会再点一次保存（suppressDirty 窗口内被抑制的
      // watcher 不会补跑，故此处必须自己算一次）。
      dirty.value = JSON.stringify(config) !== submittedSnapshot;
    } finally {
      suppressDirty = false;
    }
    frontendLogger.info("config", "配置保存成功");
  } catch (error) {
    // G19：被 saveAbort.abort() 顶替的旧保存请求属预期取消（client.ts 已把
    // AbortError 转为 name="ApiError" 的 ApiError，此处检查 aborted 标记），
    // 静默返回：不弹失败 toast、不置 saveFailed
    if (error instanceof ApiError && error.aborted) return;
    const msg = extractApiError(error, "保存失败");
    frontendLogger.error("config", "保存配置失败", error);
    toastOnly(false, msg);
    saveFailed.value = true;
  } finally {
    if (saveSeq === currentSeq) busy.save = false;
  }
}

/** 拉取服务端已保存的日志级别并回填表单（不算未保存变更） */
async function fetchLogLevels(): Promise<void> {
  try {
    const data = await configApi.fetchLogLevels();
    // 读取的是服务端已保存值：抑制 dirty 并在无未保存编辑时同步快照
    const wasDirty = dirty.value;
    suppressDirty = true;
    config.logging.level = data.level;
    await nextTick();
    if (!wasDirty) savedSnapshot = JSON.stringify(config);
    suppressDirty = false;
  } catch (error) {
    frontendLogger.warn("config", "获取日志级别配置失败", error);
  }
}

/** 设置日志级别：走独立 API 即时保存，并同步前端日志输出级别 */
async function setLogLevel(level: string): Promise<void> {
  try {
    const data = await configApi.setLogLevel(level);
    // 日志级别走独立 API 即时保存，不算表单未保存变更
    const wasDirty = dirty.value;
    suppressDirty = true;
    config.logging.level = level;
    await nextTick();
    if (!wasDirty) savedSnapshot = JSON.stringify(config);
    suppressDirty = false;
    frontendLogger.setLevel(level);
    frontendLogger.info("config", `日志级别已设置: ${level}`);
    toastOnly(true, data?.message || "日志级别已设置");
  } catch (error) {
    const msg = extractApiError(error, "设置失败");
    frontendLogger.error("config", "设置日志级别失败", error);
    toastOnly(false, msg);
  }
}

/** 切换开机自启动；后端不支持（404）时给出升级提示而非笼统报错 */
async function toggleAutostart(enable: boolean): Promise<void> {
  busy.autostart = true;
  try {
    const data = await autostartApi.toggle(enable);
    frontendLogger.info("autostart", data?.message || `${enable ? "启用" : "关闭"}自启动成功`);
    toastOnly(true, data?.message || `${enable ? "启用" : "关闭"}自启动成功`);
  } catch (error) {
    if (error instanceof ApiError && error.status === 404) {
      frontendLogger.warn("autostart", "后端不支持开机自启动");
      toastOnly(false, "当前后端版本不支持开机自启动，请重启后端");
    } else {
      frontendLogger.error("autostart", `${enable ? "启用" : "关闭"}自启动异常`, error);
      toastOnly(false, `${enable ? "启用" : "关闭"}自启动失败`);
    }
  } finally {
    await useStatus().fetchAutostart();
    busy.autostart = false;
  }
}

// F9：5s 守卫——init 与设置页 mount 双触发不再重复请求；force 供重连等显式刷新绕过
const pureModeFetchGuard = createFetchGuard(5000);
/** 拉取纯净模式开关状态（带 5s 守卫，init 与设置页 mount 双触发不重复请求） */
async function fetchPureMode(force = false): Promise<void> {
  if (!pureModeFetchGuard.shouldFetch(force)) return;
  try {
    const data = await pureModeApi.fetch();
    pureMode.value = data.enabled;
    // 与 togglePureMode 同理：拉取到的后端权威值必须同步进表单模型，
    // 否则表单里的旧值仍会在下次保存时把它覆盖回去
    const wasDirty = dirty.value;
    suppressDirty = true;
    config.browser.pure_mode = data.enabled;
    await nextTick();
    if (!wasDirty) savedSnapshot = JSON.stringify(config);
    suppressDirty = false;
    pureModeFetchGuard.markSuccess();
  } catch (error) {
    frontendLogger.debug("config", "获取纯净模式失败，保持默认", error);
  }
}

/** 切换纯净模式；失败时回滚本地开关，保证 UI 与后端状态一致 */
async function togglePureMode(): Promise<void> {
  if (pureModeLoading.value) return;
  pureModeLoading.value = true;
  try {
    const data = await pureModeApi.toggle();
    const enabled = data?.enabled ?? false;
    pureMode.value = enabled;
    // 该开关的后端权威源就是 config.browser.pure_mode（/api/pure-mode 直接改它），
    // 因此必须同步表单模型：saveConfig 的载荷携带整个 config.browser，若不回写，
    // 任何一次普通保存都会用旧值把刚关掉的开关静默翻回（服务端 json_merge 递归覆盖）。
    // 与 setLogLevel 同款处理：抑制 dirty 比对，且仅在无未保存编辑时刷新快照，
    // 避免把用户其他未保存改动误判为已保存。
    const wasDirty = dirty.value;
    suppressDirty = true;
    config.browser.pure_mode = enabled;
    await nextTick();
    if (!wasDirty) savedSnapshot = JSON.stringify(config);
    suppressDirty = false;
    frontendLogger.info("config", `纯净模式已${enabled ? "开启" : "关闭"}`);
    toastOnly(true, `纯净模式已${enabled ? "开启" : "关闭"}`);
  } catch (error) {
    pureMode.value = !pureMode.value;
    frontendLogger.error("config", "切换纯净模式失败", error);
    toastOnly(false, "切换纯净模式失败");
  } finally {
    pureModeLoading.value = false;
  }
}

export function useConfig() {
  return {
    config,
    defaultUrlCheckUrls,
    dirty,
    saveFailed,
    configLoadFailed,
    pureMode,
    pureModeLoading,
    fetchConfig,
    saveConfig,
    fetchLogLevels,
    setLogLevel,
    toggleAutostart,
    fetchPureMode,
    togglePureMode,
  };
}
