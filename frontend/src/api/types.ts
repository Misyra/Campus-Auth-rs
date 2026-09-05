/**
 * API 类型定义
 *
 * 后端使用 snake_case JSON 字段名，前端类型与之一致。
 * 本文件的手写类型是唯一来源（source of truth）；
 * 根目录 openapi.json 仅作为 API 路径清单参考，不参与类型生成。
 */

/**
 * 变更类端点（POST/PUT/PATCH/DELETE）成功时的业务负载。
 * spec 下 HTTP 2xx 即代表成功，无 success 字段；message 为可选的业务提示。
 * client.ts 已解包 { data: ... } 信封，调用方直接拿到此对象。
 */
export interface MutationResult {
  message?: string;
  [key: string]: unknown;
}

/** 背景图上传/拉取返回的业务负载 */
export interface BackgroundUploadResult {
  filename?: string;
  url?: string;
  message?: string;
}

/**
 * 状态快照（前端形态，经 useStatus.mapBackendStatus 从后端原始快照映射而来）。
 * P17：不保留索引签名——后端原始字段一律显式映射，拼写错误应在编译期暴露。
 */
export interface StatusSnapshot {
  monitoring: boolean;
  /** 网络探测累计次数（后端 probe_total；旧后端缺字段时沿用当前值） */
  network_check_count: number;
  /** 登录尝试累计次数（后端 login_total；旧后端缺字段时沿用当前值） */
  login_attempt_count: number;
  /** 当前连续探测失败次数（瞬时计数，用于状态卡片副文案） */
  consecutive_failures: number;
  /** 当前登录重试计数（瞬时计数，用于状态卡片副文案） */
  retry_count: number;
  last_check_time: string | null;
  /** 本次监控连续运行时长（秒）；未监控时为 0 */
  monitoring_seconds: number;
  runtime_seconds: number;
  network_connected: boolean;
  network_state: string;
  login_status?: string;
  engine_state?: string;
  /** 快照单调版本号（后端每次发布 +1；旧后端缺字段时为 0，回退 uptime 比较） */
  snapshot_version?: number;
}

/** 开机自启动状态 */
export interface AutostartStatus {
  platform: string;
  enabled: boolean;
  method: string;
  location: string;
  runtime_mode: string;
}

/** 卸载检测项（GET /api/uninstall/detect） */
export interface UninstallDetectItem {
  key: string;
  label: string;
  exists: boolean;
  description: string;
}

/** 卸载清理单步结果（POST /api/uninstall） */
export interface UninstallStepResult {
  key: string;
  label: string;
  success: boolean;
  message: string;
}

/** 卸载清理响应 */
export interface UninstallResponse {
  results: UninstallStepResult[];
  message: string;
}

/** 日志条目 */
export interface LogEntry {
  /**
   * 全局单调序号（P10）：/ws/logs 实时推送携带，用于稳定 v-for key 与按 seq 去重。
   * 注意 /api/logs 历史条目的 seq 每次请求重新分配、不跨请求稳定；
   * 旧后端与前端本地构造的条目可能缺失该字段（回退内容键逻辑）。
   */
  seq?: number;
  timestamp: string;
  level: string;
  source: string;
  message: string;
}

/** 通知条目（前端内存态） */
export interface NotificationEntry {
  success: boolean;
  message: string;
  time: string;
  category: string;
  icon: string;
  label: string;
  action: NotificationAction | null;
}

/** 通知可点击行为 */
export interface NotificationAction {
  label: string;
  page: string;
}

/** 浏览器配置 */
export interface BrowserConfig {
  headless: boolean;
  timeout: number;
  navigation_timeout: number;
  login_timeout: number;
  user_agent: string;
  low_resource_mode: boolean;
  disable_web_security: boolean;
  extra_headers_json: string;
  browser_args: string;
  stealth_mode: boolean;
  stealth_custom_script: string;
  locale: string;
  timezone_id: string;
  viewport_width: number;
  viewport_height: number;
  pure_mode: boolean;
  browser_channel: string;
  browser_custom_path: string;
  custom_browser_engine: string;
  persistent_context: boolean;
  ignore_https_errors: boolean;
  bind_proxy: string;
}

/** Worker（浏览器进程）配置 */
export interface WorkerConfig {
  idle_timeout_seconds: number;
  keep_alive: boolean;
}

/** 网络监控配置 */
export interface MonitorConfig {
  check_interval_seconds: number;
  network_check_timeout: number;
  ping_targets: string[];
  enable_tcp_check: boolean;
  enable_http_check: boolean;
  test_urls: string[];
  check_auth_url: boolean;
  auth_url_targets: string[];
  url_check_urls: string[];
  enable_local_check: boolean;
  /** 网络检测禁用代理（默认 true 直连；关闭后 HTTP/URL 探测跟随系统代理，重启生效） */
  disable_proxy: boolean;
  script_timeout: number;
  post_login_delay: number;
  // 网卡绑定：后端仅预留 EgressBinder 接口未实现，字段暂不暴露（配置往返保真见 constants.ts 注释）
  // bind_interface_name: string;
}

/** 暂停时段配置 */
export interface PauseConfig {
  enabled: boolean;
  start_hour: number;
  start_minute: number;
  end_hour: number;
  end_minute: number;
}

/** 日志配置 */
export interface LoggingConfig {
  level: string;
  file_enabled: boolean;
  retention_days: number;
}

/** 重试配置 */
export interface RetryConfig {
  max_retries: number;
  retry_interval: number;
}

/** 凭据配置（前端内部嵌套结构） */
export interface CredentialsConfig {
  username: string;
  password: string;
  auth_url: string;
  trigger_url: string;
  isp: string;
}

/** 应用设置 */
export interface AppSettings {
  auto_start_browser: boolean;
  runtime_mode: string;
  startup_action: string;
  port: number;
  autostart_enabled: boolean;
  task_notification: boolean;
  show_tray: boolean;
  /** 定时自重启间隔（小时，0 = 不启用） */
  auto_restart_hours: number;
}

/** 更新器设置（GET/PATCH /api/config 的 updater 段） */
export interface UpdaterConfig {
  check_on_startup: boolean;
  release_source_url: string;
  check_interval_hours: number;
  /** 下载更新与仓库任务走显式代理（地址见 proxy_url） */
  use_proxy: boolean;
  /** 代理地址，如 http://127.0.0.1:7890（支持非本机代理） */
  proxy_url: string;
  /** 旧版"本地代理端口"字段：仅兼容保留，后端在 proxy_url 为空时用它派生 */
  proxy_port: number;
}

/** 完整配置（前端内部表示，凭据嵌套） */
export interface Config {
  browser: BrowserConfig;
  worker: WorkerConfig;
  monitor: MonitorConfig;
  pause: PauseConfig;
  logging: LoggingConfig;
  retry: RetryConfig;
  credentials: CredentialsConfig;
  active_task: string;
  app_settings: AppSettings;
  updater: UpdaterConfig;
}

/** GET /api/config 返回结构（凭据平铺在顶层） */
export interface ConfigResponse {
  browser: BrowserConfig;
  worker: WorkerConfig;
  monitor: MonitorConfig;
  pause: PauseConfig;
  logging: LoggingConfig;
  retry: RetryConfig;
  app_settings: AppSettings;
  updater?: UpdaterConfig;
  active_task: string;
  has_password: boolean;
  username: string;
  auth_url: string;
  trigger_url: string;
  isp: string;
  carrier_custom: string;
  password?: string;
}

/** PATCH /api/config 请求体（凭据平铺） */
export interface SaveConfigPayload {
  browser: BrowserConfig;
  worker: WorkerConfig;
  monitor: MonitorConfig;
  pause: PauseConfig;
  logging: LoggingConfig;
  retry: RetryConfig;
  app_settings: AppSettings;
  updater: UpdaterConfig;
  active_task: string;
  username: string;
  auth_url: string;
  trigger_url: string;
  isp: string;
  password: string | null;
}

/** 配置方案 */
export interface Profile {
  id: string;
  name: string;
  username: string;
  password: string;
  auth_url: string;
  trigger_url: string;
  isp: string;
  gateway_ip: string;
  wifi_ssid: string;
  active_task: string;
  [key: string]: unknown;
}

/** 方案列表响应 */
export interface ProfileListResponse {
  profiles: Record<string, Profile>;
  active_profile: string;
  auto_switch: boolean;
}

/** 网络检测结果 */
export interface NetworkDetectResult {
  gateway_ip: string | null;
  ssid: string | null;
  matched_profile_id?: string | null;
  matched_profile_name?: string | null;
}

/** 浏览器信息 */
export interface BrowserInfo {
  channel: string;
  name: string;
  installed: boolean;
  custom?: boolean;
}

/** 浏览器列表响应 */
export interface BrowserListResponse {
  browsers: BrowserInfo[];
  current: string;
}

/** OCR 状态 */
export interface OcrStatus {
  installed: boolean;
  /** 项目是否在 pyproject.toml 中声明了 ddddocr 依赖（即是否支持 OCR） */
  declared?: boolean;
  size_mb: number;
}

/** 任务（浏览器任务 / 脚本的列表项） */
export interface TaskItem {
  id: string;
  name: string;
  description?: string;
  type?: string;
  url?: string;
  [key: string]: unknown;
}

/** 任务摘要（列表/概览用，对应后端 TaskSummary） */
export interface TaskSummary {
  id: string;
  name: string;
  description: string;
  /** 任务类型：browser / script / shell */
  task_type: string;
}

/** 任务完整配置（对应后端 TaskKind，按 type 区分 browser/script/shell） */
export interface TaskConfig {
  type?: string;
  name?: string;
  description?: string;
  url?: string;
  steps?: Array<Record<string, unknown>>;
  variables?: Record<string, unknown>;
  [key: string]: unknown;
}

/** 单个任务详情（对应后端 TaskDetail：{ summary, config }） */
export interface TaskDetail {
  summary?: TaskSummary;
  config?: TaskConfig;
}

/** 远程仓库任务索引条目 */
export interface RepoTask {
  id: string;
  name: string;
  description?: string;
  tags?: string[];
  author?: string;
  version?: string;
  url: string;
}

/** 脚本 */
export interface Script {
  id: string;
  name: string;
  description?: string;
  content?: string;
  binary_path?: string;
  [key: string]: unknown;
}

/** 二进制信息 */
export interface BinaryInfo {
  path: string;
  name: string;
}

/** 定时任务 */
export interface ScheduledTask {
  id: string;
  name: string;
  description?: string;
  task_type: string;
  target_id: string;
  cron: string;
  profile_id?: string | null;
  timeout?: number | null;
  enabled: boolean;
  last_run?: string | null;
  last_result?: string | null;
  /** cron 表达式解析失败（enabled 却永不触发），需编辑修正 */
  schedule_invalid?: boolean;
  [key: string]: unknown;
}

/** 定时任务执行历史条目（后端 job_history 扁平数组：{ run_at, success, message, duration }） */
export interface ScheduledTaskHistoryItem {
  run_at: string;
  success: boolean;
  message: string;
  duration?: number;
  [key: string]: unknown;
}

/** 登录历史条目 */
export interface LoginHistoryItem {
  timestamp: string;
  source: string;
  profile_id: string;
  result: "success" | "failed" | "cancelled";
  message: string;
  duration_secs: number;
  [key: string]: unknown;
}

/** 调试步骤 */
export interface DebugStep {
  index?: number;
  description?: string;
  type?: string;
  [key: string]: unknown;
}

/** 调试步骤结果 */
export interface DebugStepResult {
  step_index: number;
  success: boolean;
  /** 进行中标记：WS step_progress 事件置位，步骤真实结果返回后被覆盖（B7 F5） */
  running?: boolean;
  message?: string;
  screenshot_url?: string | null;
  [key: string]: unknown;
}

/** 调试会话 */
export interface DebugSession {
  running: boolean;
  task_id: string | null;
  current_step: number;
  total_steps: number;
  steps: DebugStep[];
  results: DebugStepResult[];
  screenshot_url: string | null;
}

/** 更新信息 */
export interface UpdateInfo {
  has_update: boolean;
  latest?: string;
  current?: string;
  error?: string;
  /** 发布页/下载页链接（AboutView 展示“前往下载”按钮） */
  url?: string;
  [key: string]: unknown;
}

/** 环境安装进度（后端 InstallProgress） */
export interface InstallProgress {
  phase: string;
  percent: number;
  message: string;
}

/** 环境状态（后端 EnvironmentStatus，经 GET /api/init-status.environment 透出） */
export interface EnvironmentStatus {
  uv_ready: boolean;
  python_ready: boolean;
  playwright_ready: boolean;
  capability_ready: boolean;
  stage: string;
  progress: InstallProgress | null;
  last_error: string | null;
}

/** 初始化状态 */
export interface InitStatus {
  agreed: boolean;
  ready?: boolean;
  password_decryption_failed?: boolean;
  /** @deprecated 扁平兼容字段，优先读 environment.* */
  python_ready?: boolean;
  /** @deprecated 扁平兼容字段，优先读 environment.* */
  playwright_ready?: boolean;
  environment?: EnvironmentStatus;
}

/** 健康检查 */
export interface HealthInfo {
  version?: string;
  python_version?: string;
}

/** 危险步骤（保存任务前确认） */
export interface DangerStep {
  stepIndex: number;
  stepType: string;
  description: string;
  code: string;
}
