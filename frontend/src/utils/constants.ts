/**
 * 全局常量与默认值。
 * 从 legacy js/constants.js 迁移，补充 TypeScript 类型标注。
 */

import type { Config, Appearance, Profile } from "./appearance-types";

export const TIMING = {
  STATUS_POLL_INTERVAL: 30000,
  /** 状态轮询断连退避间隔：连续失败 3 次后切换至此，成功即恢复常规间隔 */
  STATUS_POLL_SLOW_INTERVAL: 300000,
  AUTOSTART_POLL_INTERVAL: 60000,
  TOAST_DURATION: 3000,
  TOAST_LEAVE_DELAY: 300,
  NOTIFICATION_MAX: 30,
  OPENAPI_TIMEOUT: 5000,
  DRAG_SWAP_COOLDOWN: 120,
  WS_BACKOFF_BASE: 1000,
  WS_PING_INTERVAL: 30000,
} as const;
/** 登录网址留空时的默认触发地址：与 Windows NCSI 同源，明文请求可被校园网网关劫持 */
export const DEFAULT_TRIGGER_URL = "http://www.msftconnecttest.com/connecttest.txt";

/**
 * 任务仓库坐标（单一事实源）。
 *
 * 任务由**独立仓库**承载，与主程序仓库不是一个：「仓库导入」读它的 `index.json`、
 * 录制器脚本引导用户把任务提交到它的 Issues、「分享适配」按钮也指向它。此前
 * 「分享适配」误指主程序仓库（`Campus-Auth-rs`），点过去找不到任何可分享的任务。
 * 集中在此以免三处各写一份 host/owner 而再次漂移。
 */
export const TASK_REPO_OWNER = "Misyra";
export const TASK_REPO_NAME = "campus-auth-tasks";
/** 仓库主页（「分享适配」「任务仓库 →」等人类可点击入口） */
export const TASK_REPO_URL = `https://github.com/${TASK_REPO_OWNER}/${TASK_REPO_NAME}`;
/** GitHub 源的索引地址（仓库导入预设源） */
export const TASK_REPO_INDEX_URL = `https://raw.githubusercontent.com/${TASK_REPO_OWNER}/${TASK_REPO_NAME}/master/index.json`;
/** Gitee 镜像源的索引地址 */
export const TASK_REPO_INDEX_URL_GITEE = `https://raw.giteeusercontent.com/${TASK_REPO_OWNER}/${TASK_REPO_NAME}/raw/master/index.gitee.json`;
/** Gitee 镜像的仓库主页（浏览用，非索引地址） */
export const TASK_REPO_URL_GITEE = `https://gitee.com/${TASK_REPO_OWNER}/${TASK_REPO_NAME}`;

/**
 * 主程序仓库主页与发布页（单一事实源）。
 *
 * 更新弹窗的「在 GitHub 查看」用它拼具体 tag 的发布页；此前该地址只在
 * `AboutView.vue` 的模板里硬编码过一次，弹窗再抄一份会两处漂移。
 */
export const APP_REPO_URL = "https://github.com/Misyra/Campus-Auth-rs";
export const APP_RELEASES_URL = `${APP_REPO_URL}/releases`;

/** 具体版本的发布页地址；版本号为空时退回发布列表页 */
export function releaseTagUrl(version: string | undefined | null): string {
  const v = (version ?? "").trim();
  return v ? `${APP_RELEASES_URL}/tag/v${v.replace(/^v/, "")}` : APP_RELEASES_URL;
}

/**
 * B 站 UP 主主页。
 *
 * 「关于」页的外部入口。
 */
export const BILIBILI_SPACE_URL = "https://space.bilibili.com/5608024";
/**
 * 使用教程视频，直达 03:29 的录制器演示片段。
 *
 * 任务页的「使用教程」入口、录制器卡片（`设置 · 任务与环境`）与 AI 页的排障提示共用。
 * 此前 AI 页指引写「按照视频教程操作」，但全仓没有任何视频地址，用户照做找不到内容。
 * 链接去掉了分享追踪参数（`share_source` / `vd_source`），只保留 `t=209` 的起播时间。
 */
export const TUTORIAL_VIDEO_URL = "https://www.bilibili.com/video/BV1EdNg6VEbp/?t=209";

/** 仓库导入的源类型：两个预设镜像 + 用户自填地址 */
export type TaskRepoSourceId = "github" | "gitee" | "custom";

/**
 * 仓库导入的「源」选项表（单一事实源）。
 *
 * 三个字段各有用途，**不可合并**：`indexUrl` 是给程序 GET 的 raw JSON 地址，
 * `homeUrl` 是给人点开浏览的仓库页面——真实缺陷：空态里「直接查看仓库」原先把
 * `indexUrl` 当作可读页面链接，点开是一屏 raw JSON 而不是仓库首页。
 * 预设源的 `homeUrl` 只在「自定义」时为空（用户自填的地址未必有对应主页）。
 */
export const TASK_REPO_SOURCES: readonly {
  id: TaskRepoSourceId;
  label: string;
  /** 该源在「源」选择器旁的补充说明（空则不显示） */
  hint: string;
  indexUrl: string;
  homeUrl: string;
}[] = [
  {
    id: "github",
    label: "GitHub",
    hint: "国内访问可能较慢或加载失败，卡住时请改用 Gitee 镜像",
    indexUrl: TASK_REPO_INDEX_URL,
    homeUrl: TASK_REPO_URL,
  },
  {
    id: "gitee",
    label: "Gitee",
    hint: "国内访问更快，推荐国内用户使用",
    indexUrl: TASK_REPO_INDEX_URL_GITEE,
    homeUrl: TASK_REPO_URL_GITEE,
  },
  {
    id: "custom",
    label: "自定义",
    hint: "",
    indexUrl: "",
    homeUrl: "",
  },
];

export const LIMITS = {
  LOG_MAX_ENTRIES: 100,
  FILE_UPLOAD_MAX: 5 * 1024 * 1024,
  WS_LOG_BUFFER_MAX: 100,
} as const;

// 级别权重表。后端 tracing 实际发出 "WARN"，前端 logger 内部用 "WARNING"，
// 两者同秩，避免默认 INFO 过滤下 WARN 日志被错杀或选 WARN 显示全部级别。
export const LEVEL_VALUES: Record<string, number> = {
  TRACE: 0,
  DEBUG: 1,
  INFO: 2,
  WARN: 3,
  WARNING: 3,
  ERROR: 4,
};

/**
 * 日志来源 → 中文标签的唯一映射。
 * Dashboard 的来源筛选下拉与日志条目来源徽标均由此派生；
 * 后端 normalize_source 将 target 归一为五大域，本表须与之同步维护；
 * 未登记的来源（第三方 crate target）回退显示原始标识。
 */
export const LOG_SOURCE_LABELS: Record<string, string> = {
  app: "系统",
  auth: "认证",
  task: "任务",
  worker: "执行器",
  frontend: "前端",
};

export const BROWSER_ARGS_DEFAULT = [
  "--disable-blink-features=AutomationControlled",
  "--disable-software-rasterizer",
  "--disable-extensions",
  "--disable-background-timer-throttling",
  "--disable-backgrounding-occluded-windows",
  "--disable-renderer-backgrounding",
  "--disable-features=TranslateUI,BlinkGenPropertyTrees",
  "--disable-ipc-flooding-protection",
  "--disable-hang-monitor",
  "--disable-popup-blocking",
].join("\n");

export const DEFAULT_CONFIG: Config = {
  browser: {
    headless: true,
    // 以下数值以后端 schema.rs 的 impl Default 为准（加载失败兜底显示用，正常以服务端下发为准）：
    // 页面操作超时 30s、导航超时 15s、登录等待 120s
    timeout: 30,
    navigation_timeout: 15,
    login_timeout: 120,
    // 留空使用浏览器默认值；勿填死 UA，否则换 Firefox/WebKit 通道后仍伪装 Chrome
    user_agent: "",
    low_resource_mode: false,
    disable_web_security: false,
    extra_headers_json: "",
    // 后端默认仅追加 --disable-blink-features=AutomationControlled（无副作用反检测）；
    // BROWSER_ARGS_DEFAULT 为浏览器页"加载推荐参数"按钮的预设来源
    browser_args: "",
    stealth_mode: true,
    stealth_custom_script: "",
    locale: "zh-CN",
    timezone_id: "Asia/Shanghai",
    viewport_width: 1280,
    viewport_height: 720,
    pure_mode: true,
    browser_channel: "msedge",
    browser_custom_path: "",
    custom_browser_engine: "chromium",
    persistent_context: false,
    ignore_https_errors: true,
    bind_proxy: "",
  },
  worker: {
    idle_timeout_seconds: 300,
    keep_alive: false,
  },
  monitor: {
    check_interval_seconds: 120,
    network_check_timeout: 2,
    ping_targets: ["8.8.8.8:53", "114.114.114.114:53", "www.baidu.com:443"],
    enable_tcp_check: false,
    // 默认启用 HTTP 检测（generate_204 门户探测），比 TCP 更贴近真实网络连通性
    enable_http_check: true,
    test_urls: [
      "http://connect.rom.miui.com/generate_204",
      "http://connectivitycheck.platform.hicloud.com/generate_204",
      "http://wifi.vivo.com.cn/generate_204",
    ],
    enable_url_check: false,
    check_auth_url: false,
    auth_url_targets: [],
    url_check_urls: [
      "http://captive.apple.com/hotspot-detect.html|Success",
      "http://www.msftconnecttest.com/connecttest.txt|Microsoft Connect Test",
      "http://detectportal.firefox.com/success.txt|success",
    ],
    enable_local_check: false,
    // 严格登录模式默认开启：仅探测给出明确门户结论才自动登录，与历史行为一致。
    // 关闭后为宽松口径（网卡已连接且未确认在线即尝试），面向「学校门户 → 校园网认证」
    // 两级认证；代价是配置有误时会反复拉起浏览器
    strict_login_mode: true,
    // 网络检测默认禁用代理（直连），避免代理故障误判离线；关闭后跟随系统代理（下一轮生效）
    disable_proxy: true,
    script_timeout: 60,
    post_login_delay: 5,
    // 网卡绑定：后端仅预留 EgressBinder 接口未实现。保留默认值不提交该字段，
    // 后端 monitor 配置往返时缺失字段以空串兜底（见 web/routes/config.rs），行为不变。
    // bind_interface_name: "",
  },
  pause: {
    // 默认启用夜间暂停（23:00–06:00，跨天）：宿舍定时断网时段反复重连无意义，
    // 与运行模式「默认模式」预设的取值一致（镜像后端 PauseSettings 的 Default）
    enabled: true,
    start_hour: 23,
    start_minute: 0,
    end_hour: 6,
    end_minute: 0,
  },
  logging: {
    level: "INFO",
    retention_days: 7,
    file_enabled: true,
  },
  retry: {
    max_retries: 3,
    retry_interval: 5,
  },
  // credentials 段已移除：账号/认证地址/登录方式属 Profile，统一在「配置方案」页
  // 编辑（DEFAULT_PROFILE_SETTINGS 是其默认值来源）。此处保留全局设置默认值。
  app_settings: {
    auto_start_browser: true,
    // 与后端 AppSettings::default 一致（2026-09-16 起默认 none）：启动不自动开始监测。
    // 这是加载失败时的兜底显示值，正常以服务端下发为准。
    startup_action: "none",
    runtime_mode: "full",
    port: 50721,
    autostart_enabled: false,
    task_notification: true,
    show_tray: true,
    auto_restart_hours: 24,
  },
  updater: {
    check_on_startup: true,
    auto_check_enabled: true,
    channel: "stable",
    release_source_url: "https://api.github.com/repos/Misyra/Campus-Auth-rs/releases/latest",
    check_interval_hours: 24,
    use_proxy: false,
    // 必须保持空串：后端 resolved_proxy_url 靠空串回退旧版 proxy_port，
    // 填完整地址会覆盖存量配置的自定义端口（见 schema.rs 注释）
    proxy_url: "",
    proxy_port: 7890,
  },
};

/** 设置页 Tab 清单（SettingsView 消费的单一来源；hint 作为 Tab 的悬停提示）。
 *
 * **只列 `GlobalConfig` 域的设置**：账号/认证地址/登录方式/直连参数都是方案字段，
 * 统一在「配置方案」页编辑（单一入口），此处不再有「账号」Tab。 */
export const SETTINGS_TABS = [
  { id: "monitor", label: "检测", hint: "在线检测、登录重试与暂停时段" },
  { id: "browser", label: "浏览器", hint: "浏览器选择、超时与反检测参数" },
  { id: "tasks", label: "任务与环境", hint: "Python 环境、录制器与 OCR" },
  { id: "system", label: "系统", hint: "启动行为、日志与界面" },
  { id: "network", label: "网络与更新", hint: "端口、代理、自动更新与维护" },
  { id: "appearance", label: "外观", hint: "主题、背景与卡片样式" },
] as const;

/**
 * 单色主题色的哨兵值：不落具体色值，应用时按**有效主题**解析为浅色黑 / 深色白。
 *
 * 主题色只存一个 hex，而 `theme` 可以是 light/dark/auto，纯 hex 无法同时表达
 * 「日间黑、夜间白」；故默认值用该哨兵，由 `useAppearance::resolveAccentColor`
 * 在应用与渲染时解析。它不是合法 CSS 颜色，任何直接当作色值使用的位置都必须先解析。
 */
export const MONO_ACCENT = "mono";
/** 单色主题色在浅色主题下的取值（日间黑） */
export const MONO_ACCENT_LIGHT = "#000000";
/** 单色主题色在深色主题下的取值（夜间白） */
export const MONO_ACCENT_DARK = "#ffffff";

export const DEFAULT_APPEARANCE: Appearance = {
  background_url: "",
  background_filename: "",
  wallpaper_api_url: "",
  background_blur: 10,
  background_opacity: 0.3,
  background_color: "",
  card_opacity: 0.45,
  card_blur: 12,
  border_intensity: 1.0,
  sidebar_opacity: 0.95,
  sidebar_color: "",
  sidebar_accent: "",
  backdrop_filter: false,
  accent_color: MONO_ACCENT,
  theme: "light",
};

export const DARK_BG_COLORS = [
  { value: "#0f172a", label: "深空蓝" },
  { value: "#111827", label: "墨石黑" },
  { value: "#1a1a2e", label: "暗夜紫" },
  { value: "#16213e", label: "藏青" },
  { value: "#1b2838", label: "Steam 暗" },
  { value: "#0d1117", label: "GitHub 暗" },
];

export const LIGHT_BG_COLORS = [
  { value: "#eef2f7", label: "默认灰白" },
  { value: "#f8fafc", label: "纯白" },
  { value: "#f1f5f9", label: "浅灰" },
  { value: "#e8edf5", label: "淡蓝灰" },
  { value: "#fef3c7", label: "暖黄" },
  { value: "#ecfdf5", label: "薄荷绿" },
];

export const DEFAULT_CUSTOM_COLORS = {
  accent: [] as string[],
  bg: [] as string[],
  sidebar: [] as string[],
  sidebar_accent: [] as string[],
};

export const ACCENT_COLORS = [
  { value: MONO_ACCENT, label: "黑白（日间黑 / 夜间白）" },
  { value: "#22d3ee", label: "青色" },
  { value: "#3b82f6", label: "蓝色" },
  { value: "#8b5cf6", label: "紫色" },
  { value: "#ec4899", label: "粉色" },
  { value: "#f59e0b", label: "橙色" },
  { value: "#10b981", label: "绿色" },
  { value: "#ef4444", label: "红色" },
];

export const DEFAULT_PROFILE_SETTINGS: Profile = {
  id: "",
  name: "",
  gateway_ip: "",
  wifi_ssid: "",
  username: "",
  password: "",
  auth_url: "",
  trigger_url: "",
  active_task: "",
  isp: "",
  login_channel: "browser",
  /** 直连渠道绑定的直连任务 ID（空 = 未绑定；直连没有内置兜底任务，故必须显式选） */
  active_http_task: "",
};

/**
 * 内置默认浏览器任务 ID。
 *
 * 与后端 `crate::tasks::DEFAULT_TASK_ID` 同值：该任务由程序播种（`tasks/browser/default.json`，
 * 名称「通用登录」），不可删除（后端 `delete_task` 对它返回 `DeleteDefaultTask`）。
 * 方案的 `active_task` 为空时登录即回退到它（`resolve_task_choice`），故两者等价。
 */
export const DEFAULT_TASK_ID = "default";

/** 派生选项（原 app-options.js data 中的静态选项） */
export const CARRIER_OPTIONS = [  { value: "", label: "不选择" },
  { value: "移动", label: "移动" },
  { value: "联通", label: "联通" },
  { value: "电信", label: "电信" },
  { value: "自定义", label: "自定义" },
];
