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

/** 手动登录业务结果；登录失败仍以 HTTP 200 返回，由 success 表达终态。 */
export interface LoginResultResponse {
  success: boolean;
  message: string;
  duration: number;
}

/**
 * 手动执行任务的业务结果（后端 `tasks::executor::TaskResult`）。
 *
 * 任务**执行失败**同样以 HTTP 200 返回（`POST /api/tasks/{id}/execute` 直接
 * `Ok(data(result))`），成败由 `success` 表达——调用方不得因 2xx 就当成功。
 */
export interface TaskExecuteResult {
  success: boolean;
  output: string;
  exit_code: number;
  duration_ms: number;
  error: string | null;
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
  network_state: NetworkState;
  /** Engine 的暂停状态独立于网络事实；暂停时保留最后一次网络结论 */
  pause_active: boolean;
  /** 是否因连续登录失败处于冷却期 */
  cooling_down: boolean;
  /** 冷却剩余秒数 */
  cooling_down_remaining: number | null;
  /** 最近一次连通性解释 */
  connectivity: ConnectivityAssessment;
  /** 最近一次原始探测证据 */
  last_probe_evidence: ProbeEvidence | null;
  login_status?: string;
  engine_state?: string;
  /** 快照单调版本号（后端每次发布 +1；旧后端缺字段时为 0，回退 uptime 比较） */
  snapshot_version?: number;
  /** 更新下载进度（下载期间有值，结束/失败后清空） */
  update_progress?: { phase: string; percent: number; message: string } | null;
}

export type NetworkState = "online" | "captive_portal" | "offline" | "unknown";
export type AssessmentConfidence = "high" | "medium" | "low";
export type AssessmentReason =
  | "not_checked"
  | "internet_verified"
  | "captive_detected"
  | "external_failed_auth_reachable"
  | "link_up_login_assumed"
  | "all_probes_failed"
  | "weak_evidence_only"
  | "inconclusive_evidence"
  | "conflicting_evidence"
  | "no_probes_enabled";
export type AuthEndpointState =
  | "not_checked"
  | "reachable"
  | "unreachable"
  | "invalid"
  | "missing"
  | "skipped_redirect_mode";
export type RecoveryAdvice =
  | "no_action"
  | "attempt_login"
  | "attempt_login_once"
  | "wait_for_network"
  | "wait_for_more_evidence"
  | "fix_configuration"
  | "no_probe_evidence"
  | "not_evaluated";
export type ProbeOutcome = "pass" | "captive" | "inconclusive" | "fail" | "disabled";
export type LocalLinkState = "not_checked" | "available" | "unavailable" | "probe_failed";

/** 后端对一轮网络证据的统一解释 */
export interface ConnectivityAssessment {
  status: NetworkState;
  confidence: AssessmentConfidence;
  reason: AssessmentReason;
  auth_endpoint: AuthEndpointState;
  recovery_advice: RecoveryAdvice;
}

/** 最近一次探测的原始证据 */
export interface ProbeEvidence {
  tcp: ProbeOutcome;
  http: ProbeOutcome;
  url: ProbeOutcome;
  local_link: LocalLinkState;
}

/** POST /api/monitor/test 返回的手动诊断报告 */
export interface NetworkTestResult {
  status: NetworkState;
  confidence: AssessmentConfidence;
  reason: AssessmentReason;
  local_link: LocalLinkState;
  auth_endpoint: AuthEndpointState;
  details: { tcp: string[]; http: string[]; url: string[] };
  duration_ms: number;
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
  /** 自增唯一 id（渲染 :key 用；time+message 组合在同秒同文案时会冲突） */
  id: number;
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
  enable_url_check: boolean;
  check_auth_url: boolean;
  auth_url_targets: string[];
  url_check_urls: string[];
  enable_local_check: boolean;
  /**
   * 严格登录模式（默认开启）：仅在探测给出明确门户结论时才自动登录。
   * 关闭后为宽松口径——网卡已连接且探测未确认在线即尝试登录，适用于
   * 「学校门户 → 校园网认证」两级认证；代价是配置有误时会反复拉起浏览器。
   */
  strict_login_mode: boolean;
  /** 网络检测禁用代理（默认 true 直连；关闭后 HTTP/URL 探测跟随系统代理，下一轮生效） */
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
  /**
   * 本方案自动登录使用的浏览器任务 ID（空 = 未绑定，回退内置默认任务）。
   * 按方案绑定：切方案即切任务，取代旧的全局启用任务。
   */
  active_task: string;
  /** 登录执行渠道；同时可在「配置方案」编辑器与设置页「账号」修改 */
  login_channel: LoginChannel;
  http_method: HttpLoginMethod;
  http_url: string;
  http_headers: string;
  http_body: string;
  http_success_pattern: string;
  http_failure_pattern: string;
  http_crypto_script: string;
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

/** 更新通道（正式版 / 测试版 / 全通道最新版） */
export type UpdateChannel = "stable" | "prerelease" | "all";

/** 更新器设置（GET/PATCH /api/config 的 updater 段） */
export interface UpdaterConfig {
  check_on_startup: boolean;
  /** 是否启用自动检查更新（总开关；关闭后仅手动"立即检查"） */
  auto_check_enabled: boolean;
  /** 更新通道 */
  channel: UpdateChannel;
  release_source_url: string;
  check_interval_hours: number;
  /** 下载更新与仓库任务走显式代理（地址见 proxy_url） */
  use_proxy: boolean;
  /** 代理地址，如 http://127.0.0.1:7890（支持非本机代理） */
  proxy_url: string;
  /** 旧版"本地代理端口"字段：仅兼容保留，后端在 proxy_url 为空时用它派生 */
  proxy_port: number;
}

/** 上次更新检查状态（GET /api/update-state） */
export interface UpdateState {
  /** 上次检查时间（UTC RFC3339；空表示从未检查） */
  last_check_at: string;
  has_update: boolean;
  latest_version: string;
  /** 上次检查失败原因（成功时为空） */
  error: string;
  /** 远程发布是否缺少当前平台的安装包（区分"已是最新"与"无本平台包"） */
  platform_unavailable?: boolean;
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
  /** 活跃方案的登录渠道与直连参数（属 Profile 域，非全局设置） */
  login_channel: LoginChannel;
  http_method: HttpLoginMethod;
  http_url: string;
  http_headers: string;
  http_body: string;
  http_success_pattern: string;
  http_failure_pattern: string;
  http_crypto_script: string;
  password?: string;
}

/**
 * PATCH /api/config 请求体（凭据平铺）。
 * password 使用三态：null 保留已保存密码、空串清除、非空字符串加密更新。
 */
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
  /** 登录渠道与直连参数：后端写入活跃 Profile，非全局设置 */
  login_channel: LoginChannel;
  http_method: HttpLoginMethod;
  http_url: string;
  http_headers: string;
  http_body: string;
  http_success_pattern: string;
  http_failure_pattern: string;
  http_crypto_script: string;
  password: string | null;
}

/** 登录渠道与直连请求方法 */
export type LoginChannel = "browser" | "http";
export type HttpLoginMethod = "GET" | "POST";

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
  login_channel: LoginChannel;
  http_method: HttpLoginMethod;
  http_url: string;
  http_headers: string;
  http_body: string;
  http_success_pattern: string;
  http_failure_pattern: string;
  http_crypto_script: string;
  [key: string]: unknown;
}

/** 直连登录测试请求：使用编辑器内尚未保存的配置 */
export interface HttpLoginTestPayload {
  profile_id?: string;
  username: string;
  password: string;
  http_method: HttpLoginMethod;
  http_url: string;
  http_headers: string;
  http_body: string;
  http_success_pattern: string;
  http_failure_pattern: string;
  http_crypto_script: string;
  auth_url: string;
  fetch_page: boolean;
}

/** 直连登录测试结果（请求内容与响应片段均已由后端脱敏） */
export interface HttpLoginTestResult {
  rendered_url: string;
  rendered_headers: string;
  rendered_body: string;
  status: number | null;
  response_snippet: string;
  outcome:
    | "success"
    | "cancelled"
    | "navigation_timeout"
    | "selector_failed"
    | "assertion_failed"
    | "captcha_failed"
    | "invalid_credential"
    | "network_error"
    | "unknown_error";
  message: string;
  script_error: string | null;
  duration_ms: number;
}

/** 方案列表条目（后端 ProfileSummary：仅展示字段，不含密码与直连模板） */
export interface ProfileSummary {
  id: string;
  name: string;
  username: string;
  isp: string;
  active_task: string;
  /** 登录执行渠道：列表卡据此区分登录方式 */
  login_channel: LoginChannel;
  /** 网关 IP 匹配规则（空 = 未配置） */
  gateway_ip: string;
  /** WiFi SSID 匹配规则（空 = 未配置） */
  wifi_ssid: string;
}

/** 方案列表响应 */
export interface ProfileListResponse {
  profiles: Record<string, ProfileSummary>;
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

/** 认证门户检测结论（POST /api/monitor/detect-portal） */
export type PortalDetectStatus = "found" | "online" | "captive_no_redirect" | "offline";

/** 认证门户检测结果 */
export interface PortalDetectResult {
  status: PortalDetectStatus;
  portal_url: string | null;
  message: string;
  checked: string[];
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
  /** Worker 工程是否存在（即是否支持按需安装 OCR），不等于已安装 */
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
  /** 任务类型：browser / script（shell 已移除） */
  task_type: string;
}

/** 任务完整配置（对应后端 TaskKind，按 type 区分 browser/script） */
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
  /** 登录页截图（任务站 raw 地址；前端经 /api/repo/image 代理预览） */
  screenshot?: string;
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

/** 定时任务触发方式：cron 定时执行 / startup 启动后执行 */
export type ScheduledTaskTrigger = "cron" | "startup";

/** 启动触发的当日成功计数簿记（后端按本地日期窗口持久化，跨天自动归零） */
export interface ScheduledTaskDailySuccess {
  date: string;
  count: number;
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
  /** 触发方式（缺省 cron，兼容存量任务） */
  trigger?: ScheduledTaskTrigger;
  /** 启动触发：每日成功次数上限（缺省 1） */
  max_runs_per_day?: number | null;
  /** 启动触发：失败重试次数（缺省 2） */
  max_retries?: number | null;
  /** 启动触发：延迟执行秒数（缺省 30） */
  startup_delay_secs?: number | null;
  /** 启动触发：当日成功计数簿记 */
  startup_success?: ScheduledTaskDailySuccess | null;
  /** 启动触发：当日成功次数（列表接口回填，后端按本地日期计算） */
  startup_runs_today?: number;
  [key: string]: unknown;
}

/**
 * 创建/更新定时任务的请求载荷。
 *
 * id 仅创建时由前端生成（更新走路径参数，不带 id）、task_type 由后端按 target 推导，
 * 二者都不出现在载荷中，故不在此声明（故不用 `Omit<ScheduledTask, ...>`——本类型家族
 * 带 `[key: string]: unknown` 索引签名，`Omit`/`Pick` 会把字面量键并入索引签名而**不**
 * 剔除任何键，必填字段随之退化成「空类型」，缺 name 的载荷也能过检查）。
 */
export interface ScheduledTaskPayload {
  /** 仅创建时携带（前端生成 `sched_<ts>_<rand>`） */
  id?: string;
  name: string;
  description?: string;
  target_id: string;
  /** cron 表达式；trigger=startup 时后端落盘空串 */
  cron: string;
  enabled: boolean;
  timeout?: number | null;
  /** 触发方式（缺省 cron，兼容存量任务） */
  trigger?: ScheduledTaskTrigger;
  /** 启动触发字段：仅 trigger=startup 时发送 */
  max_runs_per_day?: number | null;
  max_retries?: number | null;
  startup_delay_secs?: number | null;
  [key: string]: unknown;
}

/** 定时任务执行历史条目（后端 job_history 扁平数组：{ run_at, success, message, duration }） */
export interface ScheduledTaskHistoryItem {
  run_at: string;
  success: boolean;
  message: string;
  duration?: number;
  /** 触发来源（cron/startup/manual；存量记录缺省为 null） */
  trigger?: string | null;
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
  /** 检查/更新流程的结果文案（如“更新已就绪”提示） */
  message?: string;
  /** 发布页/下载页链接（AboutView 展示“前往下载”按钮） */
  url?: string;
  /** 下载包预期 SHA256（点击“立即更新”时回传，固定本次确认的版本） */
  sha256?: string;
  /** 下载大小（字节） */
  size?: number;
  /** 更新说明（changelog） */
  notes?: string;
  /** 发布日期 */
  release_date?: string;
  /** 远程发布缺少当前平台安装包（此时 has_update=false） */
  platform_unavailable?: boolean;
  [key: string]: unknown;
}

/**
 * 更新固定版本快照
 *
 * 点击“立即更新”时把“检查更新”阶段已确认的 version/url/sha256 回传，
 * 避免服务端重查导致的版本漂移（展示 v5.0.1 却下到 v5.0.2）。
 */
export interface UpdatePin {
  version: string;
  url: string;
  sha256: string;
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
  worker_ready: boolean;
  manifest_current: boolean;
  playwright_ready: boolean;
  system_browser_ready: boolean;
  ocr_enabled: boolean;
  ocr_ready: boolean;
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
}

/** 危险步骤（保存任务前确认） */
export interface DangerStep {
  stepIndex: number;
  stepType: string;
  description: string;
  code: string;
}

/** LLM 服务配置（AI 任务生成，脱敏视图：key 只回是否已设置） */
export interface AiLlmConfig {
  provider: "opencode" | "glm" | "deepseek" | "custom";
  base_url: string;
  model: string;
  has_api_key: boolean;
  configured_providers: string[];
  max_tokens: number | null;
}

export interface AiStructureSummary {
  frames: number;
  forms: number;
  controls: number;
  captcha_candidates: number;
}

/** 登录页捕获结果（产物落盘服务端，响应只带元数据） */
export interface AiCaptureResult {
  final_url: string;
  title?: string;
  html_chars?: number;
  png_bytes?: number;
  resources_count?: number;
  structure_summary?: AiStructureSummary;
  note?: string | null;
  /** 捕获截图预览地址（GET 免鉴权，加时间戳防缓存） */
  screenshot_url?: string;
}

/** AI 生成任务结果 */
export interface AiGenerateResult {
  /** 生成的任务 JSON（未含 task_id，保存时由前端注入） */
  task: Record<string, unknown>;
  /** 实际生成轮数（1 = 首轮通过校验） */
  attempts: number;
  /** 非致命提示（截断/资源跳过/自动重试说明） */
  warnings: string[];
  model: string;
  base_url: string;
}
