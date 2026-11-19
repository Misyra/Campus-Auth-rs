/**
 * 设置页 schema：分区 / 面板 / 标准行 / 控件类型与搜索文本的单一出处。
 *
 * 「schema 驱动混合架构」的数据层：
 * - **标准设置行**（label + hint + 右侧开关/输入/下拉/分段）声明为 `SettingRowDef`，
 *   由 `SchemaRow.vue` 渲染器按绑定路径（PATCH /api/config 载荷中的点分路径）直连
 *   `useConfig().config` 草稿；
 * - **异型面板 / 异型行**（浏览器选择卡、服务模式分段、环境状态行、通知通道卡片、
 *   外观滑杆等）声明为槽位（`slot` 键），组件在 `panels.ts` 注册，原样挂进分区；
 * - 每个分区同时携带 tab 与搜索所需的全部文本（label / title / hint / keywords），
 *   `utils/settingsSearch.ts` 据此做纯数据匹配，不再抓取 DOM textContent。
 *
 * schema 只描述**静态结构**：平台差异（OpenWrt 不支持的分区 / 行）在模块装配期
 * 以数据过滤表达；任何需要运行时联动的行（开关展开输入框、confirm 动作等）都
 * 归槽位组件，不做进 schema。
 *
 * 面板归属与拆分口径（2026-11-20 设计系统收口阶段 4③，用户三拍板）：
 * 网络检测 ← 原「检测」页的检测频率 / 网络状态检测 / 诊断与恢复辅助；
 * 登录与时段 ← 登录重试 / 暂停时段；浏览器、任务与环境、推送通知、外观整段平移；
 * 系统 ← 配置预设 / 启动与运行 / 控制台与通知 / 访问与安全 / 维护操作；
 * 更新与日志 ← 日志设置 / 自动更新（更新代理随「自动更新」面板整体平移）。
 */
import type { IconName } from "@/components/common/IconApp.vue";
import { IS_OPENWRT } from "@/utils/platform";

// ===== 控件与行 =====

/** 下拉 / 分段共用选项（value 均为字符串，数值字段由渲染器按 numeric 转换） */
export interface SettingOptionDef {
  value: string;
  label: string;
}

/**
 * 标准行右侧控件。binding 是相对 GlobalConfig 草稿的点分路径
 * （如 `monitor.check_interval_seconds`），渲染器经 useConfig() 直连。
 */
export type SettingControlDef =
  | { kind: "switch"; binding: string }
  | { kind: "number"; binding: string; inputId: string; min?: number; max?: number }
  | { kind: "text"; binding: string; inputId: string; placeholder?: string }
  | {
      kind: "select";
      binding: string;
      options: readonly SettingOptionDef[];
      /** CustomSelect 触发器的可访问名（缺省回退为当前值，显式绑定设置项名称） */
      ariaLabel?: string;
      /** 数值字段的字符串下拉（如检查频率 0/24/168）：写入时转 Number */
      numeric?: boolean;
      /** 绑定路径为假值时禁用（如检查频率受总开关联动） */
      disabledBy?: string;
    }
  | { kind: "segmented"; binding: string; options: readonly SettingOptionDef[]; ariaLabel?: string };

export interface SettingRowDef {
  /** 行标识（全局唯一，同时是搜索命中单元与 DOM data-settings-row 的值） */
  id: string;
  label: string;
  /** 说明文案（设置行左列第二行） */
  hint?: string;
  /** 名称旁的静态徽标（如「推荐开启」） */
  badge?: { text: string; variant?: "info" };
  /** FieldHelp 气泡文案 */
  help?: string;
  /** 搜索补充词（通常放控件绑定路径等英文关键词） */
  keywords?: string;
  control: SettingControlDef;
}

/** 手写槽位（异型面板体 / 异型行）：组件在 panels.ts 按 slot 键注册 */
export interface SettingSlotDef {
  slot: string;
  /** 搜索单元标识（渲染器落到槽位根元素的 data-settings-unit） */
  id: string;
  /** 搜索文本：该块内容的摘要词（schema 是搜索的唯一事实源） */
  text: string;
}

export type SettingsPanelEntry = SettingRowDef | SettingSlotDef;

export interface SettingsPanelDef {
  id: string;
  /** 卡头标题（chromeless 面板由槽位组件自绘，此处仍作搜索文本） */
  title: string;
  /** 面板级搜索补充词（异型面板的全部检索词都声明在此） */
  keywords?: string;
  /** true = 整面板交给槽位组件自绘（折叠卡 / 动态卡头等）；缺省走渲染器统一 chrome */
  chromeless?: boolean;
  /** chromeless 面板的槽位组件键 */
  component?: string;
  /** 非 chromeless：标准行 + 槽位按序混排（顺序即渲染顺序） */
  entries?: readonly SettingsPanelEntry[];
}

export interface SettingsSectionDef {
  /** 分区锚点：tab id 与 URL hash（/settings#network） */
  id: string;
  /** tab 文案与分区标题 */
  label: string;
  /** tab 悬停说明（全称 + 一句说明） */
  title: string;
  icon: IconName;
  /** 挂在分区容器上的页面作用域类（如 environment-page / system-update-page 的 CSS 作用域） */
  bodyClass?: string;
  panels: readonly SettingsPanelDef[];
}

// ===== 共享槽位文本 =====

/** 「出口统一走方案 + 代理例外」说明块（网络检测 / 浏览器两分区共用 ProxyNote 槽位） */
const PROXY_NOTE_SLOT: SettingSlotDef = {
  slot: "proxy-note",
  id: "proxy-note",
  text: "登录、重定向检测和网络探测统一使用方案中的校园网出口 前往方案设置出口 网卡 DNS 代理 例外 Clash TUN 学校 DNS 配置指南",
};

// ===== 分区装配 =====

/** 网络检测：检测频率（标准行）+ 网络状态检测 / 诊断与恢复辅助（槽位） */
const NETWORK_SECTION: SettingsSectionDef = {
  id: "network",
  label: "网络检测",
  title: "检测频率、检测目标与诊断辅助",
  icon: "wifi",
  panels: [
    {
      id: "detect-frequency",
      title: "检测频率",
      keywords: "检测间隔 检测超时 check_interval network_check_timeout",
      entries: [
        {
          id: "monitor-interval",
          label: "检测间隔（秒）",
          hint: "较短间隔能更快发现断线，建议 60–300 秒，默认 300 秒",
          help: "两次网络检测之间的间隔。过短增加资源消耗，过长延迟断线发现。建议 60~300 秒，默认 300 秒（后端钳制 20~1200 秒）。",
          keywords: "check_interval_seconds",
          control: { kind: "number", binding: "monitor.check_interval_seconds", inputId: "settings-interval", min: 20, max: 1200 },
        },
        {
          id: "monitor-timeout",
          label: "检测超时（秒）",
          hint: "弱网可适当调高，默认 2 秒",
          help: "单次检测的等待上限。默认 2 秒，弱网环境可放宽至 5 秒。",
          keywords: "network_check_timeout",
          control: { kind: "number", binding: "monitor.network_check_timeout", inputId: "settings-network-check-timeout", min: 1, max: 30 },
        },
      ],
    },
    {
      id: "probe-checks",
      title: "网络状态检测",
      keywords: "204 门户检测 generate_204 TCP 检测 URL 内容检测 关键字 门户劫持 检测目标 推荐开启 补充 enable_http_check enable_tcp_check enable_url_check test_urls ping_targets",
      entries: [
        { slot: "monitor-probe", id: "monitor-probe-rows", text: "204 门户检测 通过轻量请求识别公网连接和认证门户 TCP 检测 补充检查传输层连通性 URL 内容检测 通过页面关键字辅助判断网络状态" },
      ],
    },
    {
      id: "diagnose",
      title: "诊断与恢复辅助",
      keywords: "登录严格模式 误触发 两级认证 strict_login_mode 代理例外",
      entries: [
        {
          id: "strict-login-mode",
          label: "登录严格模式",
          badge: { text: "推荐开启", variant: "info" },
          hint: "仅在确认门户劫持后自动登录，减少误触发",
          help: "默认开启。只有探测到明确的门户劫持证据才自动登录，证据不足时保持观察、不打扰。若遇到断网后未自动登录，请关闭本开关：关闭后只要网卡已连接、探测未确认在线就尝试登录，适用于「先登录学校门户、再进校园网认证选运营商」两级认证的网络——这类网关可能直接放行探测请求，严格模式下会误判为已在线而永不尝试。注意：关闭后若认证地址填错或门户无需登录，也会真的拉起浏览器，请配合暂停时段使用。",
          keywords: "strict_login_mode",
          control: { kind: "switch", binding: "monitor.strict_login_mode" },
        },
        PROXY_NOTE_SLOT,
      ],
    },
  ],
};

/** 登录与时段：登录重试（标准行）+ 暂停时段（槽位） */
const LOGIN_SECTION: SettingsSectionDef = {
  id: "login",
  label: "登录与时段",
  title: "登录重试节奏与暂停时段",
  icon: "clock",
  panels: [
    {
      id: "login-retry",
      title: "登录重试",
      keywords: "最大重试次数 重试间隔 登录后延迟 max_retries retry_interval post_login_delay",
      entries: [
        {
          id: "max-retries",
          label: "最大重试次数",
          hint: "连续失败后结束本轮尝试，默认 3 次",
          help: "登录失败后的最大重试次数。默认 3 次。",
          keywords: "max_retries",
          control: { kind: "number", binding: "retry.max_retries", inputId: "settings-max-retries", min: 1, max: 5 },
        },
        {
          id: "retry-interval",
          label: "重试间隔（秒）",
          hint: "失败后逐次延长等待时间，如 5 → 10 → 20 秒",
          help: "首次重试的等待间隔，之后每次重试翻倍（如 5 → 10 → 20 秒）。过短可能触发登录页限流。默认 5 秒。",
          keywords: "retry_interval",
          control: { kind: "number", binding: "retry.retry_interval", inputId: "settings-retry-interval", min: 1, max: 300 },
        },
        {
          id: "post-login-delay",
          label: "登录后延迟（秒）",
          hint: "等待认证生效，再复查网络连接",
          help: "登录完成后等待认证生效的时间，之后再复查网络。默认 5 秒。",
          keywords: "post_login_delay",
          control: { kind: "number", binding: "monitor.post_login_delay", inputId: "settings-post-login-delay", min: 0, max: 60 },
        },
      ],
    },
    {
      id: "pause-window",
      title: "暂停时段",
      keywords: "启用暂停时段 开始时间 结束时间 跨天 定时断网 24 小时制 pause enabled start_hour end_hour",
      entries: [
        { slot: "monitor-pause", id: "monitor-pause-rows", text: "启用暂停时段 每天在指定时段暂停检测和自动登录 开始时间（时） 结束时间（时） 使用 24 小时制 支持跨天" },
      ],
    },
  ],
};

/** 浏览器：原浏览器页整段平移（选择卡为槽位，基本设置为标准行 + 共享说明块） */
const BROWSER_SECTION: SettingsSectionDef = {
  id: "browser",
  label: "浏览器",
  title: "浏览器选择、超时与反检测参数",
  icon: "chrome",
  panels: [
    {
      id: "browser-select",
      title: "浏览器选择",
      keywords: "登录浏览器 安装浏览器 Chromium Edge Chrome Firefox 未安装 官网下载 运行环境未就绪 切换到 Chromium browser_channel",
      entries: [
        { slot: "browser-select", id: "browser-select-rows", text: "登录浏览器 选择用于自动登录的浏览器 安装浏览器 Playwright 浏览器可直接安装 当前选择的浏览器未安装 运行环境未就绪" },
      ],
    },
    {
      id: "browser-basics",
      title: "基本设置",
      keywords: "页面操作超时 页面打开超时 登录等待上限 后台运行 低资源模式 浏览器语言 浏览器时区 timeout navigation_timeout login_timeout headless low_resource_mode locale timezone_id",
      entries: [
        {
          id: "browser-page-timeout",
          label: "页面操作超时（秒）",
          hint: "单次页面操作的等待上限",
          help: "单次页面操作的等待上限。默认 30 秒。",
          keywords: "timeout",
          control: { kind: "number", binding: "browser.timeout", inputId: "settings-browser-timeout", min: 1, max: 120 },
        },
        {
          id: "browser-nav-timeout",
          label: "页面打开超时（秒）",
          hint: "打开认证页面的最长等待时间",
          help: "打开认证页面的最长等待时间。默认 15 秒。",
          keywords: "navigation_timeout",
          control: { kind: "number", binding: "browser.navigation_timeout", inputId: "settings-browser-navigation-timeout", min: 3, max: 120 },
        },
        {
          id: "browser-login-timeout",
          label: "登录等待上限（秒）",
          hint: "手动登录的最长等待时间",
          help: "手动登录的最长等待时间。默认 120 秒。",
          keywords: "login_timeout",
          control: { kind: "number", binding: "browser.login_timeout", inputId: "settings-login-timeout", min: 10, max: 600 },
        },
        {
          id: "browser-headless",
          label: "后台运行",
          hint: "不显示浏览器窗口",
          help: "不显示浏览器窗口。首次配置任务或排查问题时可关闭。",
          keywords: "headless",
          control: { kind: "switch", binding: "browser.headless" },
        },
        PROXY_NOTE_SLOT,
        {
          id: "browser-low-resource",
          label: "低资源模式",
          hint: "不加载图片，降低资源占用",
          help: "不加载图片，降低资源占用。登录页使用图片验证码时请关闭。",
          keywords: "low_resource_mode",
          control: { kind: "switch", binding: "browser.low_resource_mode" },
        },
        {
          id: "browser-locale",
          label: "浏览器语言",
          hint: "发送给网站的语言偏好",
          help: "发送给网站的语言偏好。默认 zh-CN。",
          keywords: "locale",
          control: { kind: "text", binding: "browser.locale", inputId: "settings-browser-locale", placeholder: "zh-CN" },
        },
        {
          id: "browser-timezone",
          label: "浏览器时区",
          hint: "标准时区名称，留空跟随系统",
          help: "标准时区名称，留空跟随系统。默认 Asia/Shanghai。",
          keywords: "timezone_id",
          control: { kind: "text", binding: "browser.timezone_id", inputId: "settings-browser-timezone", placeholder: "Asia/Shanghai" },
        },
      ],
    },
    {
      id: "browser-security",
      title: "安全与反检测",
      keywords: "禁用网页安全限制 反自动化检测 自定义脚本 内置脚本 disable_web_security stealth_mode stealth_custom_script",
      entries: [
        {
          id: "browser-disable-web-security",
          label: "禁用网页安全限制",
          hint: "仅在登录页出现跨域错误时启用，日常保持关闭",
          help: "仅在登录页出现跨域错误时启用，日常保持关闭。",
          keywords: "disable_web_security",
          control: { kind: "switch", binding: "browser.disable_web_security" },
        },
        { slot: "browser-stealth", id: "browser-stealth-rows", text: "反自动化检测 隐藏自动化特征 自定义反检测脚本 留空使用内置脚本 填入内置脚本" },
      ],
    },
    {
      id: "browser-session",
      title: "会话保持",
      keywords: "保持浏览器进程 空闲超时 保存浏览器数据 Cookie 持久化 数据目录 Firefox 关闭浏览器 立即关闭 keep_alive idle_timeout_seconds persistent_context",
      entries: [
        { slot: "browser-session", id: "browser-session-rows", text: "登录完成后保持浏览器进程 空闲超时（秒） 保存浏览器数据 数据目录 关闭浏览器 立即关闭浏览器 会中断正在进行的登录" },
      ],
    },
    {
      id: "browser-advanced",
      title: "高级设置",
      keywords: "纯净模式 窗口尺寸 用户代理 UA 额外启动参数 推荐参数 viewport_width viewport_height user_agent browser_args",
      entries: [
        { slot: "browser-advanced", id: "browser-advanced-rows", text: "纯净模式（推荐） 浏览器窗口尺寸 用户代理（UA） 额外启动参数 加载推荐参数 每行一个 # 开头为注释" },
      ],
    },
  ],
};

/** 任务与环境：原任务与环境页整段平移（全部为动作/状态槽位） */
const TASKS_SECTION: SettingsSectionDef = {
  id: "tasks",
  label: "任务与环境",
  title: "运行环境、任务概览、录制器与 OCR",
  icon: "terminal",
  bodyClass: "environment-page",
  panels: [
    {
      id: "env-runtime",
      title: "运行环境",
      chromeless: true,
      component: "env-runtime",
      keywords: "运行环境 初始化环境 重新同步 同步运行环境 uv Python 认证核心 浏览器 组件状态 已就绪 未就绪 检测中 环境组件",
    },
    {
      id: "task-overview",
      title: "任务概览",
      keywords: "当前任务 管理任务 现成任务库 任务仓库 选择现成任务 导入任务文件 从文件导入 刷新任务列表",
      entries: [
        { slot: "task-overview", id: "task-overview-rows", text: "当前任务 管理任务 现成任务库 选择现成任务 任务仓库 导入任务文件 从文件导入 刷新任务列表" },
      ],
    },
    {
      id: "task-recorder",
      title: "任务录制器",
      keywords: "录制登录流程 Tampermonkey 安装录制器脚本 任务编写指南 下载编写指南 教程视频 任务手册",
      entries: [
        { slot: "task-recorder", id: "task-recorder-rows", text: "录制登录流程 安装 Tampermonkey 安装录制器脚本 任务编写指南 下载编写指南 教程视频 任务手册" },
      ],
    },
    {
      id: "ocr",
      title: "验证码识别（OCR）",
      keywords: "OCR 依赖 安装 OCR 依赖 卸载 OCR 依赖 验证码识别测试 选择图片 开始识别 识别结果 120MB 未安装 已安装",
      entries: [
        { slot: "ocr", id: "ocr-rows", text: "OCR 依赖 安装 OCR 依赖 卸载 OCR 依赖 验证码识别测试 选择图片 开始识别 识别结果" },
      ],
    },
  ],
};

/** 系统：服务形态与安全边界（配置预设 / 启动与运行 / 访问与安全 / 维护操作为槽位，控制台与通知为标准行） */
const SYSTEM_SECTION: SettingsSectionDef = {
  id: "system",
  label: "系统",
  title: "配置预设、启动行为、控制台、访问安全与维护",
  icon: "settings",
  panels: [
    {
      id: "run-mode",
      title: "配置预设",
      chromeless: true,
      component: "run-mode",
      keywords: "配置预设 运行模式 预设模式 标准模式 轻量模式 极简模式 自定义 当前 启动行为 开机自启",
    },
    {
      id: "system-startup",
      title: "启动与运行",
      keywords: "启动后执行 开始检测 登录一次后退出 无操作 服务模式 完整模式 轻量模式 开机自启动 注册 取消注册 startup_action runtime_mode autostart",
      entries: [
        { slot: "system-startup", id: "system-startup-rows", text: "启动后执行 开始检测 登录一次后退出 无操作 服务模式 完整模式 轻量模式 开机自启动 取消注册 当前程序 已注册路径" },
      ],
    },
    {
      id: "console",
      title: "控制台与通知",
      keywords: "控制台端口 启动时打开控制台 任务通知 显示系统托盘图标 port auto_start_browser task_notification show_tray",
      entries: [
        {
          id: "console-port",
          label: "控制台端口",
          hint: "修改后重启生效，默认 50721。本机访问地址一般为 http://127.0.0.1:端口",
          help: "Web 控制台的监听端口。修改后重启生效，默认 50721。",
          keywords: "port",
          control: { kind: "number", binding: "app_settings.port", inputId: "settings-app-port", min: 1, max: 65535 },
        },
        {
          id: "auto-open-console",
          label: "启动时打开控制台",
          hint: "启用后，程序启动时自动打开 Web 控制台",
          help: "启用后，程序启动时自动打开 Web 控制台。",
          keywords: "auto_start_browser",
          control: { kind: "switch", binding: "app_settings.auto_start_browser" },
        },
        {
          id: "task-notification",
          label: "任务通知",
          hint: "关键事件完成时弹出系统通知",
          help: "关键事件完成时弹出系统通知。",
          keywords: "task_notification",
          control: { kind: "switch", binding: "app_settings.task_notification" },
        },
        {
          id: "show-tray",
          label: "显示系统托盘图标",
          hint: "关闭后无托盘图标，仅可通过 Web 控制台操作",
          help: "关闭后无托盘图标，仅可通过 Web 控制台操作。修改后重启生效。",
          keywords: "show_tray",
          control: { kind: "switch", binding: "app_settings.show_tray" },
        },
      ],
    },
    {
      id: "system-access",
      title: "访问与安全",
      keywords: "管理凭据 远程访问引导密码 显示 隐藏 management_password CAMPUS_AUTH_MANAGEMENT_PASSWORD",
      entries: [
        { slot: "system-access", id: "system-access-rows", text: "管理凭据（远程访问引导密码） 仅远程（非回环）访问时作为界面引导凭据 显示 隐藏 留空表示未设置" },
      ],
    },
    {
      id: "system-maintenance",
      title: "维护操作",
      keywords: "定时自重启 配置热重载 重新加载配置 回收内存 auto_restart_hours reload",
      entries: [
        { slot: "system-maintenance", id: "system-maintenance-rows", text: "定时自重启 不启用 每 6 小时 每 12 小时 每 24 小时 每 48 小时 每周 配置热重载 重新加载配置" },
      ],
    },
  ],
};

/** 更新与日志：日志设置（标准行 + 槽位）+ 自动更新（提示块/标准行/代理/动作混排） */
const UPDATE_SECTION: SettingsSectionDef = {
  id: "update",
  label: "更新与日志",
  title: "日志设置、更新通道与自动检查",
  icon: "download",
  bodyClass: "system-update-page",
  panels: [
    {
      id: "logs",
      title: "日志设置",
      keywords: "日志保留天数 启用文件日志 全局日志级别 导出诊断日志 日志压缩包 retention_days file_enabled level export",
      entries: [
        {
          id: "log-retention",
          label: "日志保留天数",
          hint: "日志和失败截图按天归档，超过设定天数自动清理",
          help: "日志和失败截图按天归档，超过设定天数自动清理。",
          keywords: "retention_days",
          control: { kind: "number", binding: "logging.retention_days", inputId: "settings-log-retention", min: 1, max: 365 },
        },
        {
          id: "log-file-enabled",
          label: "启用文件日志",
          keywords: "file_enabled",
          control: { kind: "switch", binding: "logging.file_enabled" },
        },
        { slot: "system-logs-extra", id: "system-logs-extra-rows", text: "全局日志级别 TRACE DEBUG INFO WARN ERROR 导出诊断日志 导出日志压缩包 打包运行日志 登录历史 脱敏环境摘要" },
      ],
    },
    {
      id: "update",
      title: "自动更新",
      keywords: "自动检查更新 检查频率 每次启动 每天一次 每周一次 更新通道 正式版 测试版 全通道最新版 使用代理 代理地址 手动选择安装包 立即检查 上次检查时间 查看更新日志 立即更新 前往下载 使用本地包更新 当前已是最新版本 auto_check_enabled check_interval_hours channel use_proxy proxy_url",
      entries: [
        { slot: "update-tip", id: "update-tip", text: "下载慢？可把安装包放进 update/ 目录 复制路径 SHA256 内容判断 完全离线 整包覆盖 pending.json staging" },
        {
          id: "update-auto-check",
          label: "自动检查更新",
          hint: "关闭后不再自动检查更新，仅保留手动“立即检查”",
          help: "关闭后不再自动检查更新，仅保留手动“立即检查”。保存后即时生效。",
          keywords: "auto_check_enabled",
          control: { kind: "switch", binding: "updater.auto_check_enabled" },
        },
        {
          id: "update-check-frequency",
          label: "检查频率",
          hint: "选择启动时检查或定期检查",
          help: "每次启动：仅启动时检查一次；每天/每周：启动时先检查一次，之后按周期自动检查。",
          keywords: "check_interval_hours",
          control: {
            kind: "select",
            binding: "updater.check_interval_hours",
            ariaLabel: "检查频率",
            numeric: true,
            disabledBy: "updater.auto_check_enabled",
            options: [
              { value: "0", label: "每次启动" },
              { value: "24", label: "每天一次" },
              { value: "168", label: "每周一次" },
            ],
          },
        },
        { slot: "update-proxy", id: "update-proxy-rows", text: "下载与任务仓库代理 使用代理 代理地址 http://127.0.0.1:7890 仅影响版本更新检查 检测流量的代理设置" },
        {
          id: "update-channel",
          label: "更新通道",
          hint: "选择正式版、测试版或全通道最新版",
          help: "正式版仅跟随稳定发布；测试版跟随预发布（alpha/beta）；全通道最新版取两者中更高者。",
          keywords: "channel",
          control: {
            kind: "segmented",
            binding: "updater.channel",
            ariaLabel: "更新通道",
            options: [
              { value: "stable", label: "正式版" },
              { value: "prerelease", label: "测试版" },
              { value: "all", label: "全通道最新版" },
            ],
          },
        },
        { slot: "update-actions", id: "update-actions-rows", text: "手动选择安装包 选择安装包 立即检查 上次检查时间 查看更新日志 立即更新 前往下载 使用本地包更新 当前已是最新版本 从未检查 下载更新" },
      ],
    },
  ],
};

/** 推送通知：推送设置（标准行）+ 触发事件 / 免打扰时段 / 推送通道（槽位） */
const NOTIFICATION_SECTION: SettingsSectionDef = {
  id: "notification",
  label: "推送通知",
  title: "推送通道、触发事件与免打扰时段",
  icon: "bell",
  panels: [
    {
      id: "push-settings",
      title: "推送设置",
      keywords: "启用推送通知 失败重试次数 重试基准间隔 enabled retry_max_attempts retry_base_interval_seconds",
      entries: [
        {
          id: "notification-enabled",
          label: "启用推送通知",
          hint: "关闭后不推送任何通知；「测试发送」不受此开关限制",
          help: "总开关。开启前建议先添加通道并用「测试发送」验证。通知发送失败会自动重试并只记日志，不影响自动登录。",
          keywords: "enabled",
          control: { kind: "switch", binding: "notification.enabled" },
        },
        {
          id: "notification-retry-max",
          label: "失败重试次数",
          hint: "失败后按基准间隔逐次翻倍重试（如 2 → 4 → 8 秒），耗尽后只记日志",
          help: "单次推送失败后的重试次数（不含首次）。0 表示只发送一次；退避间隔随次数翻倍。",
          keywords: "retry_max_attempts",
          control: { kind: "number", binding: "notification.retry_max_attempts", inputId: "settings-notification-retry", min: 0, max: 10 },
        },
        {
          id: "notification-retry-base",
          label: "重试基准间隔（秒）",
          hint: "首次重试前的等待秒数，之后逐次翻倍",
          keywords: "retry_base_interval_seconds",
          control: { kind: "number", binding: "notification.retry_base_interval_seconds", inputId: "settings-notification-retry-base", min: 1, max: 60 },
        },
      ],
    },
    {
      id: "notification-events",
      title: "触发事件",
      keywords: "触发事件 登录成功 登录失败 网络断开 网络恢复 任务完成 任务失败 发现更新 环境就绪 档案切换 events",
      entries: [
        { slot: "notification-events", id: "notification-events-rows", text: "触发事件 登录成功 登录失败 网络断开 网络恢复 任务完成 任务失败 发现更新 环境就绪 档案切换" },
      ],
    },
    {
      id: "notification-quiet",
      title: "免打扰时段",
      keywords: "启用免打扰时段 开始时间 结束时间 跨零点 全天免打扰 quiet_enabled",
      entries: [
        { slot: "notification-quiet", id: "notification-quiet-rows", text: "启用免打扰时段 时段内事件只记日志不推送 开始时间 结束时间 使用 24 小时制 支持次日时间" },
      ],
    },
    {
      id: "notification-channels",
      title: "推送通道",
      chromeless: true,
      component: "notification-channels",
      keywords: "推送通道 添加通道 测试发送 删除通道 Webhook Bark Server酱 Telegram 系统通知 Device Key SendKey Bot Token Chat ID 请求头 JSON 模板 占位符 密钥 明文存储 通道类型 名称 超时",
    },
  ],
};

/** 外观：外观页整段平移（滑杆 / 色板 / 背景等全部为异型面板，整体作为槽位） */
const APPEARANCE_SECTION: SettingsSectionDef = {
  id: "appearance",
  label: "外观",
  title: "主题、背景与卡片样式",
  icon: "palette",
  panels: [
    {
      id: "appearance-preferences",
      title: "外观偏好",
      chromeless: true,
      component: "appearance",
      keywords: "主题与配色 显示模式 浅色 深色 跟随系统 主题色 页面背景色 卡片与层次 快捷风格 卡片不透明度 边框强度 毛玻璃效果 玻璃模糊度 背景图片 随机壁纸 上传背景 恢复默认 恢复全部默认 阅读偏好 字号 界面缩放 动效 减少动态效果 即时生效 本浏览器",
    },
  ],
};

/** 全部分区（tab 顺序即页面顺序）；OpenWrt 裁掉需要浏览器 / Worker 的分区 */
function supportedSections(sections: readonly SettingsSectionDef[]): readonly SettingsSectionDef[] {
  return IS_OPENWRT ? sections.filter((section) => !["browser", "tasks"].includes(section.id)) : sections;
}

/** OpenWrt 不展示的行（控制台端口 / 托盘图标在 router 版无意义） */
function supportedRows(rows: readonly SettingRowDef[]): readonly SettingRowDef[] {
  return IS_OPENWRT ? rows.filter((row) => !["console-port", "show-tray"].includes(row.id)) : rows;
}

function trimSection(section: SettingsSectionDef): SettingsSectionDef {
  return {
    ...section,
    panels: section.panels.map((panel) =>
      panel.entries
        ? {
            ...panel,
            // 槽位条目原样保留；仅对标准行应用平台过滤
            entries: panel.entries.filter(
              (entry) => !("control" in entry) || supportedRows([entry as SettingRowDef]).length > 0,
            ),
          }
        : panel,
    ),
  };
}

/** 设置页分区清单（单一事实源）：SettingsView 顶栏 tab、页面分区、设置搜索与命令面板共用 */
export const SETTINGS_SECTIONS: readonly SettingsSectionDef[] = supportedSections([
  NETWORK_SECTION,
  LOGIN_SECTION,
  BROWSER_SECTION,
  TASKS_SECTION,
  SYSTEM_SECTION,
  UPDATE_SECTION,
  NOTIFICATION_SECTION,
  APPEARANCE_SECTION,
].map(trimSection));
