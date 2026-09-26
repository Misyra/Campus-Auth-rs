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
  /** 后端检查到可用更新（周期检查命中时置真；前端据此自动弹出更新弹窗） */
  update_available?: boolean;
}

export type NetworkState = "online" | "captive_portal" | "offline" | "unknown";
export type AssessmentConfidence = "high" | "medium" | "low";
export type AssessmentReason =
  | "not_checked"
  | "internet_verified"
  | "captive_detected"
  | "external_failed_auth_reachable"
  | "link_up_login_assumed"
  | "redirect_login_assumed"
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

/**
 * 卸载清单项：程序目录 / 用户数据目录。
 *
 * `exists` 参与渲染（不存在的目录不该被列成"将删除"），但**不参与是否删除的判定**
 * ——判定在助手侧按目录名做（`crate::uninstall`），存在性只是给人看的。
 */
export interface UninstallTarget {
  /** 数据目录名（`program` 项无此字段） */
  key?: string;
  label: string;
  path: string;
  exists: boolean;
}

/** 卸载检测响应（GET /api/uninstall/detect） */
export interface UninstallDetectResult {
  /** 程序目录与用户数据目录之外的系统残留（用户数据目录 / Playwright 缓存 / 自启动） */
  items: UninstallDetectItem[];
  /** 程序目录：卸载时整体删除（含 resources/ docs/ python_worker/ 与随包源码副本） */
  program: UninstallTarget;
  /**
   * 卸载助手（`campus-auth-helper`）是否在位。
   *
   * 它不在位时 `purge` 一定失败（spawn 不出来），而那时"清理系统残留"可能已经跑过一遍
   * ——界面据此先把按钮拦下，别让用户白删一轮。老后端不返回该字段，故可选。
   */
  helper?: UninstallTarget;
  /** 用户数据目录：勾选「保留配置与任务」时整棵保留 */
  data: UninstallTarget[];
  /** 非 null = 拒绝卸载及原因（如该目录是源码仓库而非安装目录），界面据此禁用卸载 */
  blocked: string | null;
}

/** 真卸载响应（POST /api/uninstall/purge） */
export interface UninstallPurgeResponse {
  message: string;
  kept_user_data: boolean;
  install_dir: string;
  /**
   * 本次实际删除的用户数据目录。
   *
   * 与 `UninstallTarget` 同形但**不含 `exists`**（后端返回 `{key,label,path}`）：这里是
   * "已经确定要删的东西"，存在性由 `detect` 报过。故类型上放宽，免得 UI 误以为有该字段。
   */
  data_dirs: Array<Omit<UninstallTarget, "exists">>;
  /** 是否顺带取消了待应用更新（不取消会在退出时被更新助手装回来） */
  cancelled_pending_update: boolean;
  /**
   * 取消待应用更新**失败**，退出后仍可能被更新助手重新安装。
   *
   * 只在取消失败（`pending.json` / staging 被占用）时为真：这时必须出声，否则用户以为
   * 卸载完成了、下次开机却看到程序还在。老后端不返回该字段，故可选。
   */
  pending_update_left?: boolean;
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

/**
 * 凭据字段集合（已不属于 `Config`）。
 *
 * 这些字段都是 `ProfileData` 的成员，仅在「配置方案」页编辑（直连任务的请求参数
 * 已独立成 `HttpTaskConfig`，不在此列）。`Config` 里曾有一份 `credentials` 投影
 * （由 `GET /api/config` 的扁平响应填充），使同一份数据有了两个可写入口，现已移除。
 */
export interface CredentialsConfig {
  username: string;
  password: string;
  auth_url: string;
  trigger_url: string;
  isp: string;
  /**
   * 本方案自动登录使用的浏览器任务 ID（空 = 未绑定，回退内置默认任务）。
   * 按方案绑定：切方案即切任务。
   */
  active_task: string;
  /** 登录执行渠道 */
  login_channel: LoginChannel;
  /**
   * 直连渠道绑定的直连任务 ID（空 = 未绑定）。
   *
   * 直连**没有内置兜底任务**（门户地址无法内置），故空值意味着直连登录会直接失败。
   */
  active_http_task: string;
  /**
   * 脚本渠道绑定的脚本任务 ID（空 = 未绑定）。
   *
   * 与 `active_http_task` 同理：脚本渠道也没有内置兜底任务（登录逻辑只能自己写），
   * 故空值意味着脚本登录会直接失败。
   */
  active_script_task: string;
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

/**
 * 完整全局配置（前端内部表示）。
 *
 * 只含 `GlobalConfig` 域字段：账号与登录方式属于 Profile，由「配置方案」页
 * 经 `/api/profiles/*` 读写，不在本结构内。
 */
export interface Config {
  browser: BrowserConfig;
  worker: WorkerConfig;
  monitor: MonitorConfig;
  pause: PauseConfig;
  logging: LoggingConfig;
  retry: RetryConfig;
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
  /** 活跃方案的登录渠道与直连任务绑定（属 Profile 域，非全局设置） */
  login_channel: LoginChannel;
  /** 直连渠道绑定的直连任务 ID（空 = 未绑定；请求参数在任务里，不在本响应里） */
  active_http_task: string;
  /** 脚本渠道绑定的脚本任务 ID（空 = 未绑定；脚本正文在任务里，不在本响应里） */
  active_script_task: string;
}

/**
 * PATCH /api/config 请求体：只含全局设置。
 *
 * 后端仍接受扁平的凭据/直连键并把它们写回活跃方案（兼容既有客户端），但前端
 * 不再提交这些字段——它们是方案数据，只能在「配置方案」页显式保存，否则任意
 * 全局保存都会顺带改写活跃方案的凭据。
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
}

/** 登录渠道与直连请求方法 */
export type LoginChannel = "browser" | "http" | "script";
export type HttpLoginMethod = "GET" | "POST";

/**
 * 直连 HTTPS 证书策略（三态）。
 *
 * `null` = 未设置，登录时跟随全局 `browser.ignore_https_errors`（默认 true，
 * 与浏览器渠道同口径）；`true`/`false` = 本直连任务显式覆盖。
 */
export type HttpIgnoreHttpsErrors = boolean | null;

/**
 * 直连登录的成败判定方式。
 *
 * `"response"`（默认）= 响应关键字：命中成功关键字即成功（为空时退回 HTTP 2xx）；
 * `"network"` = 网络检测：响应体与状态码都不参与成功判定，登录请求发出且未命中
 * 失败关键字即交给登录后的网络检测判定（公网可达才算真成功）。
 */
export type HttpSuccessCheck = "response" | "network";

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
  /**
   * 直连渠道绑定的直连任务 ID（空 = 未绑定）。
   *
   * 与 `active_task` 同语义，但**没有内置兜底任务**：门户地址无法内置，故未绑定时
   * 直连登录直接以明确原因失败（浏览器渠道则会回退到内置默认任务）。
   */
  active_http_task: string;
  /**
   * 脚本渠道绑定的脚本任务 ID（空 = 未绑定）。
   *
   * 登录动作整个由这个脚本任务承担：程序起本地子进程跑它，凭据经环境变量注入
   * （`CAMPUS_USERNAME` / `CAMPUS_PASSWORD` / `CAMPUS_ISP` / `CAMPUS_AUTH_URL`），
   * 脚本退出码 0 视为本次尝试成功（真终态仍由登录后网络验证确认）。
   */
  active_script_task: string;
  [key: string]: unknown;
}

/**
 * 直连登录的前置请求（`HttpTaskConfig.pre_request`）。
 *
 * 先发一次请求、取出一个值（如 CSRF token），再发登录请求；登录请求的地址、请求头
 * 与请求体里用 `{name}` 引用这个值。取值方式目前只支持 `json:字段路径`。
 */
export interface HttpPreRequest {
  method: HttpLoginMethod;
  /** 请求地址模板（与登录请求同一套占位符） */
  url: string;
  /** 请求头模板（每行 `名称: 值`） */
  headers: string;
  /** 请求体模板（POST 使用） */
  body: string;
  /** 取值方式：`json:<字段>[.<字段>...]`，如 `json:csrf_token`、`json:data.token` */
  extract: string;
  /** 把取到的值注册成哪个占位符（留空时取 `extract` 路径的最后一段） */
  name: string;
}

/**
 * 直连任务的动作请求（`HttpTaskConfig.logout_request`）：发了不判成败的附加请求。
 *
 * 与 `HttpPreRequest` 的分工：前置请求**取值**（取不到即登录流程终态失败），
 * 动作请求**触达**（门户收没收到都照常走主流程）。退出登录正是这一类——强制下线
 * 通常只为把「IP 已在线，拒绝重复登录」的旧会话踢掉，下线请求失败不拦登录。
 */
export interface HttpActionRequest {
  method: HttpLoginMethod;
  /** 请求地址模板（与登录请求同一套占位符） */
  url: string;
  /** 请求头模板（每行 `名称: 值`） */
  headers: string;
  /** 请求体模板（POST 使用） */
  body: string;
  /** 请求发出后等待秒数（0 = 不等待；下线异步生效的门户等 1~3 秒再登录） */
  wait_secs: number;
}

/**
 * 直连任务配置（`tasks/http/<id>.json`，`type: "http"`）。
 *
 * 只描述**请求形状**：账号、密码与认证地址仍属方案——同一门户的不同账号共用一份
 * 直连任务，这正是把它从方案里独立出来的意义。仓库分享的也是这个对象。
 */
export interface HttpTaskConfig {
  task_id: string;
  name: string;
  description: string;
  method: HttpLoginMethod;
  /** 请求地址模板（支持 {username} 等占位符） */
  url: string;
  /**
   * 认证地址 = 门户登录页地址（脚本 `ctx.auth_url` 与「抓取登录页原文」的来源）。
   *
   * 不是登录请求地址（那是 `url`）。留空时回退用方案的 `auth_url`，因此老配置不填
   * 也照旧工作；填了则本任务自带认证页地址——分享给别人的任务通常需要它，
   * 否则对方还得自己摸出认证页地址。
   */
  auth_url: string;
  /** 请求头模板（每行 `名称: 值`） */
  headers: string;
  /** 请求体模板（POST 使用） */
  body: string;
  success_pattern: string;
  failure_pattern: string;
  /** 凭据变换脚本（JS `transform(ctx)`；空 = 不变换） */
  crypto_script: string;
  /**
   * 前置请求（可选）：登录前先发一次请求、从响应里取出一个值供登录请求引用。
   *
   * 面向「令牌绑连接」的门户：部分门户的 CSRF token 必须与登录请求走**同一条 TCP
   * 连接**（换连接服务器回 `CSRF token mismatch`），且 token 只能从另一个接口取到。
   * 两次请求由后端用同一个 HTTP 客户端顺序发出，连接因此被复用。
   *
   * `null` / 缺省 = 不需要前置请求。
   */
  pre_request?: HttpPreRequest | null;
  /**
   * 退出登录请求（可选）：登录前先发一次下线动作（踢掉「IP 已在线」的旧会话）。
   *
   * 排在整个流程**最前**（先于读取登录页与凭据变换脚本），与登录请求走同一条 keep-alive
   * 连接；请求成败不判定——失败只记日志，登录照常进行。`null` / 缺省 = 不需要。
   */
  logout_request?: HttpActionRequest | null;
  /** null = 跟随全局证书策略（`browser.ignore_https_errors`） */
  ignore_https_errors: HttpIgnoreHttpsErrors;
  /**
   * 成败判定方式（默认 `"response"` 响应关键字）。
   *
   * `"network"` = 网络检测：响应体与状态码都不参与成功判定，登录请求发出且未命中
   * 失败关键字即交给登录后的网络检测一锤定音（公网可达才算真成功）。适用于响应体
   * 不可靠的门户（如 dr1003 的 JSONP 回调名恒等于 callback）。失败关键字两种模式下
   * 都生效（门户明确报错时快速失败，不必等探测）。
   */
  success_check?: HttpSuccessCheck;
  /** 任务元数据（执行器不用；仓库来源等标注可放这里） */
  metadata?: Record<string, unknown>;
}

/** 方案分享载荷（导出产物 / 导入输入）
 *
 * 后端导出时剔除 username 与 password（密码是跨机器不可解的 ENC: 密文，原样带出
 * 会被接收方当明文再加密一次），并清空 active_task（接收方通常没有该任务）。
 * 两个凭据字段仍保留在类型里：据其是否为空在前端提示"需自行填写"。
 */
export interface ProfileSharePayload {
  campus_auth_profile: number;
  exported_at?: string;
  app_version?: string;
  /** 导入时的命名建议（源方案 id，恒为 ASCII slug）；缺失时由后端按名称推导 */
  suggested_id?: string;
  profile: Omit<Profile, "id" | "active_task" | "active_http_task"> & {
    active_task?: string;
    active_http_task?: string;
  };
}

/** 方案导入结果：导入成功后的实际方案 ID（冲突时已自动改名） */
export interface ProfileImportResult {
  id: string;
  /**
   * 分享文件里带着旧版「方案内联直连配置」（v9 及以前的 `http_*` 字段）时为 true。
   *
   * v10 起直连参数只存在于直连任务里，后端会忽略这些残留字段——必须让用户知道，
   * 否则会以为导入后直连开箱可用。
   */
  legacy_http_config_dropped?: boolean;
}

/**
 * GET /api/profiles/{id} 响应。
 *
 * `has_password` 是独立于 `settings` 的布尔：后端出于安全不回传密码（`settings.password`
 * 恒为空串），仅凭它前端无法区分「没设密码」与「有密码但被抹掉」，占位文案只能猜。
 * 口径与 `GET /api/config` 的 `has_password` 一致（反映「密码可解密」而非「字段非空」）。
 */
export interface ProfileDetailResponse {
  settings: Profile;
  has_password: boolean;
}

/** PUT /api/profiles/{id} 请求体：字段全可选，仅覆盖出现的字段 */
export interface ProfileUpdatePayload extends Partial<Profile> {
  /**
   * 显式清除已保存密码。
   *
   * 不能用 `password: ""` 表达清除——该接口的空串契约是「未修改，保留原密码」，
   * 两者等价。清除只能经本字段完成。
   */
  clear_password?: boolean;
}

/**
 * 直连测试请求（`POST /api/http-tasks/test`）。
 *
 * 两种用法：任务编辑器里传未保存的 `task`（任务页没有方案上下文，账号密码要手填）；
 * 方案编辑器里传已保存任务的 `task_id` + `profile_id`（凭据留空时由后端回退该方案
 * 已保存的密码）。
 *
 * 认证地址的解析顺序：任务的 `auth_url` → 方案的 `auth_url`（给了 `profile_id` 时），
 * 与正式登录同口径，避免「测试能过、自动登录用了另一个地址」的错位。
 */
export interface HttpTaskTestPayload {
  /** 已保存的直连任务 ID；与 `task` 二选一，`task` 优先 */
  task_id?: string;
  /** 编辑器内尚未保存的直连任务草稿 */
  task?: HttpTaskConfig;
  /** 凭据（以及认证地址回退）的来源方案（可省） */
  profile_id?: string;
  username: string;
  password: string;
  /** 是否在运行脚本前抓取认证页原文 */
  fetch_page: boolean;
}

/** 直连登录测试结果（请求内容与响应片段均已由后端脱敏） */
export interface HttpLoginTestResult {
  rendered_url: string;
  rendered_headers: string;
  rendered_body: string;
  status: number | null;
  /** 响应头逐行文本（已脱敏；排查 Content-Type/charset/跳转问题） */
  response_headers: string;
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
  /** 直连渠道绑定的直连任务 ID（空 = 未绑定）；任务页据此标出"这条任务被谁在用" */
  active_http_task: string;
  /** 脚本渠道绑定的脚本任务 ID（空 = 未绑定）；用途同 `active_http_task` */
  active_script_task: string;
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

/** 可见浏览器重定向检测结论（POST /api/monitor/test-redirect） */
export type RedirectTestStatus = "detected" | "online" | "not_detected";

/** 重定向检测结果；检测只判断能力，不返回或保存门户临时地址。 */
export interface RedirectTestResult {
  status: RedirectTestStatus;
  message: string;
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
  /**
   * Worker 存活时上报的运行时 OCR 能力；`null` 表示认证核心当前未运行
   * （按需懒加载），不代表依赖缺失。详见 `GET /api/ocr/status`。
   */
  runtime_ocr?: boolean | null;
}

/** 任务（浏览器任务 / 脚本 / 直连任务的列表项） */
export interface TaskItem {
  id: string;
  name: string;
  description?: string;
  type?: string;
  task_type?: string;
  /** 任务地址：浏览器=登录页，直连=请求地址；摘要自带，任务列表行直接显示 */
  url?: string;
  /** 直连任务的请求方法（GET/POST）；非直连任务缺省 */
  http_method?: string;
  /** 任务文件最近修改时间（UTC RFC3339）；读不到时缺省——列表「最近修改」列数据源 */
  modified_at?: string;
  [key: string]: unknown;
}

/** 任务摘要（列表/概览用，对应后端 TaskSummary） */
export interface TaskSummary {
  id: string;
  name: string;
  description: string;
  /** 任务类型：browser / script / http（http = 直连任务） */
  task_type: string;
}

/** 任务完整配置（对应后端 TaskKind，按 type 区分 browser/script/http） */
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
  /**
   * 条目类型（`browser` / `script` / `http`）。
   *
   * 缺省视为 `browser`：直连任务加入索引之前发布的老条目没有这个字段，
   * 把缺省当 browser 才能让同一个仓库同时承载两类条目而不破坏既有条目。
   */
  type?: string;
  /** 登录页截图（任务站 raw 地址；前端经 /api/repo/image 代理预览） */
  screenshot?: string;
  /**
   * 来源仓库地址（任务改编自他人脚本时标注出处）。
   *
   * 来自远端索引，渲染前须经 `repoSourceUrl` 只放行 http(s)。
   */
  source?: string;
  url: string;
}

/** 脚本 */
export interface Script {
  id: string;
  name: string;
  description?: string;
  content?: string;
  binary_path?: string;
  /** 脚本文件最近修改时间（UTC RFC3339）；读不到时缺省——列表「最近修改」列数据源 */
  modified_at?: string;
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

/** 定时任务执行历史条目（后端 map_history_records 输出：{ run_at, success, message, duration, trigger }） */
export interface ScheduledTaskHistoryItem {
  run_at: string;
  success: boolean;
  message: string;
  /** 执行耗时秒数；存量历史记录缺 duration 时为 null（同 trigger 口径） */
  duration?: number | null;
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
  /**
   * 已命中 `update/` 目录下摘要与远程清单一致的安装包（后端
   * `updater::LocalPackage`）：此时点击「立即更新」会跳过下载。
   *
   * 仅作展示用。应用阶段服务端会重新扫描并复制校验，请求体里不接受本地文件信息。
   */
  local_package?: LocalPackage | null;
  [key: string]: unknown;
}

/** 本地安装包（后端 updater::LocalPackage，来自 `<base_path>/update/` 目录） */
export interface LocalPackage {
  /** 文件名（不含目录） */
  file_name: string;
  /** 文件大小（字节） */
  size: number;
  /** 文件 SHA256（hex 小写，与远程清单声明值一致） */
  sha256: string;
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

/** 系统信息（GET /api/system/info） */
export interface SystemInfo {
  version: string;
  /** 程序数据目录（运行时 base_path）；配置、任务、日志与 update/ 都相对它 */
  base_path: string;
  port: number;
  active_profile_id: string;
  platform: string;
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
  /** 服务商标识：`custom` 覆盖「自定义服务商」（含指向 opencode.ai 的历史配置） */
  provider: "glm" | "deepseek" | "custom";
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
