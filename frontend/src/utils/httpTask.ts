/**
 * HTTP 登录任务（`type: "http"`）的草稿 ⇄ 载荷互转与校验。
 *
 * 任务页编辑器用平铺草稿（一个字段一个输入控件），落盘与接口用 `HttpTaskConfig`；
 * 两种形态的互转集中在此，避免任务面板、仓库导入、方案编辑器各写一份而漂移。
 *
 * HTTP 登录任务**不含凭据**：账号、密码、认证地址属于方案（同一门户的不同账号共用
 * 一份任务），故此处没有也不应有对应字段。
 */

import type { HttpFailureAction, HttpFlowStep, HttpIgnoreHttpsErrors, HttpLoginMethod, HttpSuccessCheck, HttpTaskConfig } from "@/api/types";

/** 任务 ID 校验：与后端 `TASK_ID_PATTERN` 同口径（ASCII，允许 - 与 _） */
export const HTTP_TASK_ID_PATTERN = /^[a-zA-Z0-9_-]{1,64}$/;

/** 新建任务尚未填写请求地址时的界面占位值，不是可解析的模板变量。 */
export const HTTP_TASK_PLACEHOLDER_URL = "{gateway_host}";

/** 新建草稿的默认名称 */
export const HTTP_TASK_DEFAULT_NAME = "新建 HTTP 登录任务";

/** 对齐后端流程校验：模板 URL 先检查协议和主机形状，占位符在发送前再替换。 */
function hasHttpHost(raw: string): boolean {
  const url = raw.trim();
  const separator = url.indexOf("://");
  if (separator < 0) return false;
  const scheme = url.slice(0, separator).toLowerCase();
  const host = url.slice(separator + 3).split(/[/?#]/, 1)[0];
  return (scheme === "http" || scheme === "https") && !!host;
}

/** 创建一个不带凭据的 HTTP 请求步骤。 */
export function emptyHttpRequestStep(id = "login", name = "登录请求"): HttpFlowStep {
  return {
    id, name, kind: "request", method: "GET", url: "", headers: "", body: "",
    extract: "", extract_as: "", extracts: [], stop_on_redirect: false,
    script: "", on_error: "stop", wait_secs: 0,
  };
}

/** 计算步骤可选示例：把本机 IP 原样存为 ip，供后续请求引用。 */
export const HTTP_IP_EXAMPLE_SCRIPT = "function transform(ctx) {\n  return { ip: ctx.local_ip };\n}";

/** 新建任务从一个空白登录请求开始，填写真实门户地址前不落盘。 */
export function initialHttpFlowSteps(): HttpFlowStep[] {
  return [emptyHttpRequestStep()];
}

/** 旧式固定链在编辑器中的等价有序步骤；打开旧文件本身不会触发迁移保存。 */
function legacyHttpSteps(config: HttpTaskConfig): HttpFlowStep[] {
  const steps: HttpFlowStep[] = [];
  if (config.logout_request?.url) {
    steps.push({
      ...emptyHttpRequestStep("logout", "清理旧会话"),
      ...config.logout_request,
      on_error: "continue",
    });
  }
  if (config.crypto_script?.trim()) {
    steps.push({
      ...emptyHttpRequestStep("credentials", "计算凭据"),
      kind: "transform",
      script: config.crypto_script,
    });
  }
  if (config.pre_request?.url) {
    steps.push({
      ...emptyHttpRequestStep("prepare", "获取登录参数"),
      ...config.pre_request,
      name: "获取登录参数",
      extract_as: config.pre_request.name,
    });
  }
  steps.push({
    ...emptyHttpRequestStep(),
    method: config.method ?? "GET",
    url: config.url ?? "",
    headers: config.headers ?? "",
    body: config.body ?? "",
  });
  return steps;
}

/** HTTP 登录任务编辑草稿（平铺字段 + 新建标记） */
export interface HttpTaskDraft {
  /** 当前编辑器展示的有序步骤。 */
  steps: HttpFlowStep[];
  /** 最终判定采用哪一步的响应。 */
  result_step_id: string;
  /** 旧任务在首次调整步骤后才保存为新格式。 */
  _flowEnabled?: boolean;
  id: string;
  name: string;
  description: string;
  method: HttpLoginMethod;
  url: string;
  /** 认证地址 = 门户登录页地址；留空则回退用方案的 `auth_url` */
  auth_url: string;
  headers: string;
  body: string;
  success_pattern: string;
  failure_pattern: string;
  failure_action: HttpFailureAction;
  /** 成败判定方式（默认 "response" 响应关键字；"network" = 网络检测） */
  success_check: HttpSuccessCheck;
  crypto_script: string;
  /**
   * 前置请求（可选）：**地址留空 = 不需要**，不额外加一个开关。
   *
   * 开关 + 字段两份状态会各自漂移（关了开关但字段还在，用户以为没生效）；
   * 地址是前置请求存在的前提——后端 `HttpPreRequest::validate` 也是这么判的。
   */
  pre_request_method: HttpLoginMethod;
  pre_request_url: string;
  pre_request_headers: string;
  pre_request_body: string;
  /** 取值方式 `json:字段路径`；地址非空时必填 */
  pre_request_extract: string;
  /** 注册成哪个占位符（留空 = 取 `extract` 路径最后一段） */
  pre_request_name: string;
  /**
   * 退出登录请求（可选）：**地址留空 = 不需要**，与前置请求同一套"无开关"口径。
   *
   * 登录前先发一次下线动作（踢掉「IP 已在线」的旧会话），请求成败不判定、
   * 失败不拦登录。`wait_secs` 为下线后等待秒数（下线异步生效的门户用）。
   */
  logout_method: HttpLoginMethod;
  logout_url: string;
  logout_headers: string;
  logout_body: string;
  logout_wait_secs: number;
  ignore_https_errors: HttpIgnoreHttpsErrors;
  /** 新建（尚未落盘）：允许改 id，保存走新建语义 */
  _isNew?: boolean;
}

/** 新建空草稿 */
export function emptyHttpTaskDraft(): HttpTaskDraft {
  return {
    steps: [emptyHttpRequestStep()],
    result_step_id: "login",
    _flowEnabled: false,
    id: "",
    name: HTTP_TASK_DEFAULT_NAME,
    description: "",
    // 与后端 `HttpRequestMethod::default()` 一致：JSON 里 method 缺省即 GET，
    // 若此处默认 POST，缺字段的任务打开后会显示成另一个方法
    method: "GET",
    url: "",
    auth_url: "",
    headers: "",
    body: "",
    success_pattern: "",
    failure_pattern: "",
    failure_action: "credential",
    // 新任务默认网络检测：不看响应内容，以登录后能否上外网作为成功判据——
    // 响应格式拿不准的门户（大多数）开箱即用；响应关键字作为可选项保留
    success_check: "network",
    crypto_script: "",
    pre_request_method: "GET",
    pre_request_url: "",
    pre_request_headers: "",
    pre_request_body: "",
    pre_request_extract: "",
    pre_request_name: "",
    logout_method: "GET",
    logout_url: "",
    logout_headers: "",
    logout_body: "",
    logout_wait_secs: 0,
    ignore_https_errors: null,
    _isNew: true,
  };
}

/** 已保存任务配置 → 草稿（字段缺省由后端 `serde(default)` 补齐，此处逐项兜底） */
export function httpTaskDraftFromConfig(config: HttpTaskConfig): HttpTaskDraft {
  return {
    steps: config.steps?.length ? config.steps.map((step) => ({
      ...emptyHttpRequestStep(step.id, step.name), ...step,
      extracts: step.extracts?.length
        ? step.extracts.map((rule) => ({ ...rule }))
        : step.extract?.trim() ? [{ source: step.extract, name: step.extract_as || step.extract.split(".").at(-1) || "value" }] : [],
      extract: "", extract_as: "",
    })) : legacyHttpSteps(config).map((step) => ({
      ...step,
      extracts: step.extract?.trim() ? [{ source: step.extract, name: step.extract_as || step.extract.split(".").at(-1) || "value" }] : [],
      extract: "", extract_as: "",
    })),
    result_step_id: config.result_step_id || (config.steps?.length
      ? [...config.steps].reverse().find((step) => step.kind === "request")?.id || ""
      : "login"),
    _flowEnabled: (config.steps?.length ?? 0) > 0 || (config.schema_version ?? 1) > 1,
    id: config.task_id ?? "",
    name: config.name || HTTP_TASK_DEFAULT_NAME,
    description: config.description ?? "",
    method: config.method ?? "GET",
    url: config.url ?? "",
    auth_url: config.auth_url ?? "",
    headers: config.headers ?? "",
    body: config.body ?? "",
    success_pattern: config.success_pattern ?? "",
    failure_pattern: config.failure_pattern ?? "",
    failure_action: config.failure_action === "retry" || config.failure_action === "manual" ? config.failure_action : "credential",
    // 成败判定方式：老配置没有这个键，缺省按响应关键字（与后端 serde default 同口径）
    success_check: config.success_check === "network" ? "network" : "response",
    crypto_script: config.crypto_script ?? "",
    // 前置请求：老配置没有这个键（`undefined`）与显式 null 都按"不需要"处理
    pre_request_method: config.pre_request?.method ?? "GET",
    pre_request_url: config.pre_request?.url ?? "",
    pre_request_headers: config.pre_request?.headers ?? "",
    pre_request_body: config.pre_request?.body ?? "",
    pre_request_extract: config.pre_request?.extract ?? "",
    pre_request_name: config.pre_request?.name ?? "",
    // 退出登录请求：老配置没有这个键（`undefined`）与显式 null 都按"不需要"处理；
    // wait_secs 为 0/缺失时归 0（下线后不等待）
    logout_method: config.logout_request?.method ?? "GET",
    logout_url: config.logout_request?.url ?? "",
    logout_headers: config.logout_request?.headers ?? "",
    logout_body: config.logout_request?.body ?? "",
    logout_wait_secs: typeof config.logout_request?.wait_secs === "number" && config.logout_request.wait_secs > 0
      ? config.logout_request.wait_secs
      : 0,
    ignore_https_errors: config.ignore_https_errors ?? null,
    _isNew: false,
  };
}

/** 草稿 → 落盘载荷（`PUT /api/tasks/{id}` 需要完整的 TaskKind JSON） */
export function httpTaskPayload(draft: HttpTaskDraft): HttpTaskConfig & { type: "http" } {
  if (draft._flowEnabled) {
    return {
      type: "http",
      schema_version: 2,
      task_id: draft.id,
      name: draft.name.trim() || HTTP_TASK_DEFAULT_NAME,
      description: draft.description.trim(),
      steps: draft.steps.map((step) => ({
        ...step, url: step.url.trim(), extract: "", extract_as: "",
        extracts: step.extracts.map((rule) => ({ source: rule.source.trim(), name: rule.name.trim() })),
      })),
      result_step_id: draft.result_step_id,
      method: "GET",
      url: "",
      auth_url: draft.auth_url.trim(),
      headers: "",
      body: "",
      success_pattern: draft.success_pattern,
      failure_pattern: draft.failure_pattern,
      failure_action: draft.failure_action,
      success_check: draft.success_check,
      crypto_script: "",
      pre_request: null,
      logout_request: null,
      ignore_https_errors: draft.ignore_https_errors,
    };
  }
  return {
    type: "http",
    task_id: draft.id,
    name: draft.name.trim() || HTTP_TASK_DEFAULT_NAME,
    description: draft.description.trim(),
    method: draft.method,
    url: draft.url.trim(),
    auth_url: draft.auth_url.trim(),
    headers: draft.headers,
    body: draft.body,
    success_pattern: draft.success_pattern,
    failure_pattern: draft.failure_pattern,
    failure_action: draft.failure_action,
    success_check: draft.success_check,
    crypto_script: draft.crypto_script,
    // 地址留空 = 不需要前置请求（null 与缺省同义；后端 Option<HttpPreRequest> 收 null）
    pre_request: draft.pre_request_url.trim()
      ? {
          method: draft.pre_request_method,
          url: draft.pre_request_url.trim(),
          headers: draft.pre_request_headers,
          body: draft.pre_request_body,
          extract: draft.pre_request_extract.trim(),
          name: draft.pre_request_name.trim(),
        }
      : null,
    // 地址留空 = 不需要退出登录（与前置请求同口径；后端 Option<HttpActionRequest> 收 null）
    logout_request: draft.logout_url.trim()
      ? {
          method: draft.logout_method,
          url: draft.logout_url.trim(),
          headers: draft.logout_headers,
          body: draft.logout_body,
          // 执行侧钳制 [0, 30]，此处只把非法输入归 0（负数/NaN 不该落盘）
          wait_secs: Number.isFinite(draft.logout_wait_secs) && draft.logout_wait_secs > 0
            ? draft.logout_wait_secs
            : 0,
        }
      : null,
    ignore_https_errors: draft.ignore_https_errors,
  };
}

/**
 * 保存前的缺口清单（空数组 = 可保存）。
 *
 * 只校验"缺了必然登不上"的项：id 形态、请求地址。判定关键字刻意不强制——
 * 不少门户 HTTP 200 即成功，强制填写会逼用户编一个关键字出来。
 *
 * 前置请求按"地址非空即启用"判断：填了地址却没说取哪个字段，取不到值就发不出正确的
 * 登录请求（后端也拒绝保存），属于同一类"缺了必然登不上"。
 */
export function httpTaskDraftGaps(draft: HttpTaskDraft): string[] {
  const gaps: string[] = [];
  if (draft._isNew && !HTTP_TASK_ID_PATTERN.test(draft.id.trim())) {
    gaps.push("任务 ID（1~64 位字母、数字、下划线或连字符）");
  }
  if (draft.auth_url.trim() && !hasHttpHost(draft.auth_url)) {
    gaps.push("认证页面地址（需为带主机名的 http/https URL）");
  }
  if (draft._flowEnabled) {
    if (!draft.steps.length || draft.steps.length > 16) {
      gaps.push("请求步骤（1～16 个）");
    }
    const ids = new Set<string>();
    draft.steps.forEach((step, index) => {
      const label = `步骤 ${index + 1}`;
      if (!HTTP_TASK_ID_PATTERN.test(step.id) || ids.has(step.id)) gaps.push(`${label} ID（需唯一）`);
      ids.add(step.id);
      if (step.kind === "request") {
        if (!step.url.trim() || step.url.trim() === HTTP_TASK_PLACEHOLDER_URL) gaps.push(`${label} 请求地址`);
        else if (!hasHttpHost(step.url)) gaps.push(`${label} 请求地址（需为带主机名的 http/https URL）`);
        if (step.extracts.length > 12) gaps.push(`${label} 取值规则（最多 12 条）`);
        const names = new Set<string>();
        step.extracts.forEach((rule, ruleIndex) => {
          if (!/^(json|header|url|redirect|html|regex):.+/.test(rule.source.trim())) gaps.push(`${label} 第 ${ruleIndex + 1} 条取值来源`);
          if (!/^[a-zA-Z0-9_]{1,64}$/.test(rule.name.trim()) || names.has(rule.name.trim())) gaps.push(`${label} 第 ${ruleIndex + 1} 条变量名（需唯一）`);
          names.add(rule.name.trim());
        });
        if (!Number.isFinite(step.wait_secs) || step.wait_secs < 0 || step.wait_secs > 30) gaps.push(`${label} 等待秒数（0～30）`);
      } else if (!step.script.trim()) {
        gaps.push(`${label} 计算脚本`);
      }
    });
    const resultStep = draft.steps.find((step) => step.kind === "request" && step.id === draft.result_step_id);
    if (!resultStep) {
      gaps.push("结果判断引用的请求步骤");
    } else if (resultStep.on_error === "continue") {
      gaps.push("结果来源步骤不能忽略失败");
    }
    return gaps;
  }
  if (!draft.url.trim() || draft.url.trim() === HTTP_TASK_PLACEHOLDER_URL) {
    gaps.push("请求地址");
  } else if (!hasHttpHost(draft.url)) {
    gaps.push("请求地址（需为带主机名的 http/https URL）");
  }
  if (draft.pre_request_url.trim()) {
    if (!hasHttpHost(draft.pre_request_url)) {
      gaps.push("前置请求地址（需为带主机名的 http/https URL）");
    }
    if (!draft.pre_request_extract.trim()) {
      gaps.push("前置请求的取值方式（如 json:csrf_token）");
    } else if (!draft.pre_request_extract.trim().startsWith("json:")) {
      gaps.push("前置请求的取值方式需以 json: 开头（如 json:data.token）");
    }
  } else if (draft.pre_request_extract.trim()) {
    // 只填了取值方式没填地址：不报就等着用户困惑"为什么没生效"
    gaps.push("前置请求的请求地址（填了取值方式就必须有地址）");
  }
  if (draft.logout_url.trim()) {
    if (!hasHttpHost(draft.logout_url)) {
      gaps.push("退出登录地址（需为带主机名的 http/https URL）");
    }
    // 与后端 `HttpActionRequest::validate` 的等待钳制同口径（0~30 秒）
    if (!(draft.logout_wait_secs >= 0) || draft.logout_wait_secs > 30) {
      gaps.push("退出登录的等待秒数（0~30）");
    }
  }
  return gaps;
}
