/**
 * 全局常量与默认值。
 * 从 legacy js/constants.js 迁移，补充 TypeScript 类型标注。
 */

import type { Config, Appearance, Profile } from "./appearance-types";

export const TIMING = {
  STATUS_POLL_INTERVAL: 30000,
  AUTOSTART_POLL_INTERVAL: 60000,
  TOAST_DURATION: 3000,
  TOAST_LEAVE_DELAY: 300,
  NOTIFICATION_MAX: 30,
  OPENAPI_TIMEOUT: 5000,
  DRAG_SWAP_COOLDOWN: 120,
  WS_BACKOFF_BASE: 1000,
  WS_PING_INTERVAL: 30000,
} as const;

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
 * 未登记的来源（后端新模块 / 前端新 scope）回退显示原始标识。
 */
export const LOG_SOURCE_LABELS: Record<string, string> = {
  app: "应用",
  launcher: "启动器",
  container: "容器",
  engine: "引擎",
  login: "登录",
  monitor: "监测",
  bridge: "Bridge",
  scheduler: "调度",
  web: "Web",
  config: "配置",
  tray: "托盘",
  updater: "更新",
  network: "网络",
  tasks: "任务",
  environment: "环境",
  python_worker: "Python Worker",
  frontend: "前端",
  notification: "通知",
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
    // 后端默认为空（不追加额外参数）；历史预设保留在 BROWSER_ARGS_DEFAULT 备查
    browser_args: "",
    stealth_mode: false,
    stealth_custom_script: "",
    locale: "zh-CN",
    timezone_id: "Asia/Shanghai",
    viewport_width: 1280,
    viewport_height: 720,
    pure_mode: false,
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
    check_interval_seconds: 300,
    network_check_timeout: 2,
    ping_targets: ["8.8.8.8:53", "114.114.114.114:53", "www.baidu.com:443"],
    enable_tcp_check: false,
    // 默认启用 HTTP 检测（generate_204 门户探测），比 TCP 更贴近真实网络连通性
    enable_http_check: true,
    test_urls: [
      "https://connect.rom.miui.com/generate_204",
      "https://connectivitycheck.platform.hicloud.com/generate_204",
    ],
    check_auth_url: false,
    auth_url_targets: [],
    url_check_urls: [
      "http://captive.apple.com/hotspot-detect.html|Success",
      "http://www.msftconnecttest.com/connecttest.txt|Microsoft Connect Test",
      "http://detectportal.firefox.com/success.txt|success",
    ],
    enable_local_check: true,
    // 网络检测默认禁用代理（直连），避免代理故障误判离线；关闭后跟随系统代理（重启生效）
    disable_proxy: true,
    script_timeout: 60,
    post_login_delay: 5,
    // 网卡绑定：后端仅预留 EgressBinder 接口未实现。保留默认值不提交该字段，
    // 后端 monitor 配置往返时缺失字段以空串兜底（见 web/routes/config.rs），行为不变。
    // bind_interface_name: "",
  },
  pause: {
    // 后端 PauseSettings 派生 Default：默认不启用（enabled=false），此处镜像后端
    enabled: false,
    start_hour: 0,
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
  credentials: {
    username: "",
    password: "",
    auth_url: "",
    isp: "",
  },
  active_task: "",
  app_settings: {
    auto_start_browser: true,
    startup_action: "monitor",
    runtime_mode: "full",
    port: 50721,
    autostart_enabled: false,
    task_notification: true,
    show_tray: true,
    auto_restart_hours: 0,
  },
  updater: {
    check_on_startup: true,
    release_source_url: "https://api.github.com/repos/Misyra/Campus-Auth-rs/releases/latest",
    check_interval_hours: 24,
    use_proxy: false,
    // 必须保持空串：后端 resolved_proxy_url 靠空串回退旧版 proxy_port，
    // 填完整地址会覆盖存量配置的自定义端口（见 schema.rs 注释）
    proxy_url: "",
    proxy_port: 7890,
  },
};

/** 设置页 Tab 清单（SettingsView 消费的单一来源；hint 作为 Tab 的悬停提示） */
export const SETTINGS_TABS = [
  { id: "account", label: "账号", hint: "账号、密码、认证地址与运营商" },
  { id: "monitor", label: "监测", hint: "在线检测、登录重试与暂停时段" },
  { id: "system", label: "系统", hint: "启动行为、日志、端口与代理" },
  { id: "browser", label: "浏览器", hint: "浏览器选择、超时与反检测参数" },
  { id: "tasks", label: "任务", hint: "任务录制器、OCR 与任务入口" },
] as const;

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
  accent_color: "#22d3ee",
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
  active_task: "",
  isp: "",
};

/** 派生选项（原 app-options.js data 中的静态选项） */
export const CARRIER_OPTIONS = [
  { value: "", label: "不选择" },
  { value: "移动", label: "移动" },
  { value: "联通", label: "联通" },
  { value: "电信", label: "电信" },
  { value: "自定义", label: "自定义" },
];
