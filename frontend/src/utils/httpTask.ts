/**
 * 直连任务（`type: "http"`）的草稿 ⇄ 载荷互转与校验。
 *
 * 任务页编辑器用平铺草稿（一个字段一个输入控件），落盘与接口用 `HttpTaskConfig`；
 * 两种形态的互转集中在此，避免任务面板、仓库导入、方案编辑器各写一份而漂移。
 *
 * 直连任务**不含凭据**：账号、密码、认证地址属于方案（同一门户的不同账号共用
 * 一份任务），故此处没有也不应有对应字段。
 */

import type { HttpIgnoreHttpsErrors, HttpLoginMethod, HttpTaskConfig } from "@/api/types";

/** 任务 ID 校验：与后端 `TASK_ID_PATTERN` 同口径（ASCII，允许 - 与 _） */
export const HTTP_TASK_ID_PATTERN = /^[a-zA-Z0-9_-]{1,64}$/;

/** 新建草稿的默认名称 */
export const HTTP_TASK_DEFAULT_NAME = "新直连任务";

/** 直连任务编辑草稿（平铺字段 + 新建标记） */
export interface HttpTaskDraft {
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
  ignore_https_errors: HttpIgnoreHttpsErrors;
  /** 新建（尚未落盘）：允许改 id，保存走新建语义 */
  _isNew?: boolean;
}

/** 新建空草稿 */
export function emptyHttpTaskDraft(): HttpTaskDraft {
  return {
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
    crypto_script: "",
    pre_request_method: "GET",
    pre_request_url: "",
    pre_request_headers: "",
    pre_request_body: "",
    pre_request_extract: "",
    pre_request_name: "",
    ignore_https_errors: null,
    _isNew: true,
  };
}

/** 已保存任务配置 → 草稿（字段缺省由后端 `serde(default)` 补齐，此处逐项兜底） */
export function httpTaskDraftFromConfig(config: HttpTaskConfig): HttpTaskDraft {
  return {
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
    crypto_script: config.crypto_script ?? "",
    // 前置请求：老配置没有这个键（`undefined`）与显式 null 都按"不需要"处理
    pre_request_method: config.pre_request?.method ?? "GET",
    pre_request_url: config.pre_request?.url ?? "",
    pre_request_headers: config.pre_request?.headers ?? "",
    pre_request_body: config.pre_request?.body ?? "",
    pre_request_extract: config.pre_request?.extract ?? "",
    pre_request_name: config.pre_request?.name ?? "",
    ignore_https_errors: config.ignore_https_errors ?? null,
    _isNew: false,
  };
}

/** 草稿 → 落盘载荷（`PUT /api/tasks/{id}` 需要完整的 TaskKind JSON） */
export function httpTaskPayload(draft: HttpTaskDraft): HttpTaskConfig & { type: "http" } {
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
  if (!draft.url.trim()) {
    gaps.push("请求地址");
  }
  if (draft.pre_request_url.trim()) {
    if (!draft.pre_request_extract.trim()) {
      gaps.push("前置请求的取值方式（如 json:csrf_token）");
    } else if (!draft.pre_request_extract.trim().startsWith("json:")) {
      gaps.push("前置请求的取值方式需以 json: 开头（如 json:data.token）");
    }
  } else if (draft.pre_request_extract.trim()) {
    // 只填了取值方式没填地址：不报就等着用户困惑"为什么没生效"
    gaps.push("前置请求的请求地址（填了取值方式就必须有地址）");
  }
  return gaps;
}
