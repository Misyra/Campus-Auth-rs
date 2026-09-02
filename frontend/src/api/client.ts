/**
 * HTTP 客户端封装：fetch + 自动解包 + 统一错误处理。
 * 所有 API 调用通过此模块暴露的端点函数，禁止在业务代码中直接 fetch。
 *
 * 契约（详见根目录 openapi.json 与后端实现）：
 * - 成功响应统一为 `{ "data": <业务负载> }`，列表端点 data 直接是数组；
 * - 错误响应统一为 `{ "error": { "code": "...", "message": "...", "details": {...} } }`；
 * - 无 success 字段，HTTP 状态码非 2xx 即为错误。
 */

import { frontendLogger } from "../utils/logger";

/** 统一 API 错误，携带解析出的 code / 用户友好消息 / details */
export class ApiError extends Error {
  status?: number;
  code?: string;
  detail?: unknown;
  /**
   * 请求因调用方 AbortSignal 被主动取消（非超时）。
   * G19：AbortError 在此被转为 ApiError（name 固定为 "ApiError"），
   * 调用方不能再用 `err.name === "AbortError"` 判断，应检查本标记；
   * 被 abort 的旧请求属预期行为，应静默处理而非当作失败提示。
   */
  aborted?: boolean;
  constructor(message: string, status?: number, detail?: unknown, code?: string, aborted?: boolean) {
    super(message);
    this.name = "ApiError";
    this.status = status;
    this.detail = detail;
    this.code = code;
    this.aborted = aborted;
  }
}

const BASE = "";

/**
 * 本地 API 鉴权 token：首次请求时从 /api/auth/token 懒加载并缓存。
 *
 * 后端对所有 /api/* 与 /ws/* 请求校验 X-Auth-Token（防本地恶意网页 CSRF），
 * token 端点本身豁免鉴权，其响应受 CORS 读保护（仅 localhost Origin 可读）。
 */
let authToken: string | null = null;
let tokenPromise: Promise<string | null> | null = null;

/** 获取（并缓存）鉴权 token；失败返回 null（后端将拒绝后续请求并返回 401） */
export function ensureAuthToken(): Promise<string | null> {
  if (authToken) return Promise.resolve(authToken);
  if (!tokenPromise) {
    // 注意：此处必须用裸 fetch，经 request() 会因 ensureAuthToken 递归
    tokenPromise = fetch(`${BASE}/api/auth/token`)
      .then((res) => (res.ok ? res.json() : null))
      .then((json) => {
        const token = (json as { data?: { token?: string } } | null)?.data?.token;
        authToken = typeof token === "string" && token ? token : null;
        tokenPromise = null;
        return authToken;
      })
      .catch((e) => {
        frontendLogger.debug("auth", "获取 token 失败，将以匿名请求继续", e);
        tokenPromise = null;
        return null;
      });
  }
  return tokenPromise;
}

/** 重置 token 缓存（401 时调用，后端重启会换发新 token） */
function resetAuthToken(): void {
  authToken = null;
  tokenPromise = null;
}

export interface RequestOptions {
  signal?: AbortSignal;
  timeout?: number;
  /** multipart/form-data 上传，禁止手动设置 Content-Type（浏览器自动加 boundary） */
  rawBody?: BodyInit;
  headers?: Record<string, string>;
}

/**
 * 发起请求并解包响应。
 *
 * 契约说明（见 data-models.md §7.3）：
 * - 成功响应统一为 `{ "data": <业务负载> }`，本函数自动解包返回 data 内容；
 * - 错误响应统一为 `{ "error": { code, message, details } }`，抛出携带 code/message/details 的 ApiError；
 * - HTTP 非 2xx 一律按错误处理；无 success 字段判断。
 */
async function request<T>(method: string, path: string, opts: RequestOptions = {}, retried = false): Promise<T> {
  const controller = opts.timeout || opts.signal ? new AbortController() : null;
  let timeoutId: ReturnType<typeof setTimeout> | undefined;
  let timedOut = false;
  const abortFromCaller = () => controller?.abort(opts.signal?.reason);
  if (controller && opts.signal) {
    if (opts.signal.aborted) abortFromCaller();
    else opts.signal.addEventListener("abort", abortFromCaller, { once: true });
  }
  if (controller) {
    if (opts.timeout) {
      timeoutId = setTimeout(() => {
        timedOut = true;
        controller.abort();
      }, opts.timeout);
    }
  }
  const signal = controller?.signal;
  const token = await ensureAuthToken();

  const headers: Record<string, string> = {
    ...(token ? { "X-Auth-Token": token } : {}),
    ...(opts.headers || {}),
  };
  // FormData 的 boundary 必须由浏览器生成；手工设置 Content-Type 会导致后端无法解析 multipart。
  if (opts.rawBody !== undefined && !(opts.rawBody instanceof FormData)) {
    headers["Content-Type"] ??= "application/json";
  }

  let res: Response;
  try {
    res = await fetch(`${BASE}${path}`, {
      method,
      headers,
      body: opts.rawBody !== undefined ? opts.rawBody : undefined,
      signal,
    });
  } catch (e) {
    if ((e as Error).name === "AbortError") {
      // timedOut 区分超时与调用方主动取消；后者置 aborted 标记供调用方静默处理
      throw new ApiError(timedOut ? "请求超时" : "请求已取消", undefined, e, undefined, !timedOut);
    }
    throw new ApiError("网络连接失败，请检查后端是否已启动", undefined, e);
  } finally {
    if (timeoutId) clearTimeout(timeoutId);
    opts.signal?.removeEventListener("abort", abortFromCaller);
  }

  // 401：token 可能因后端重启而轮换，重取一次后重试（仅一次，避免循环）
  if (res.status === 401 && !retried) {
    resetAuthToken();
    return request<T>(method, path, opts, true);
  }

  const contentType = res.headers.get("content-type") || "";
  if (!res.ok) {
    let message = `请求失败 (${res.status})`;
    let code: string | undefined;
    let detail: unknown;
    if (contentType.includes("application/json")) {
      try {
        const errJson = (await res.json()) as Record<string, unknown>;
        // spec 错误信封：{ error: { code, message, details } }
        const errObj = errJson.error as { code?: string; message?: string; details?: unknown } | undefined;
        if (errObj && typeof errObj === "object") {
          if (typeof errObj.message === "string") message = errObj.message;
          if (typeof errObj.code === "string") code = errObj.code;
          detail = errObj.details;
        } else {
          // 兼容旧 FastAPI detail 数组格式
          detail = errJson.detail;
          const detailMsg = extractDetailMessage(detail);
          if (detailMsg) message = detailMsg;
        }
      } catch {
        /* ignore */
      }
    }
    throw new ApiError(message, res.status, detail, code);
  }

  if (contentType.includes("application/json")) {
    const json = (await res.json()) as Record<string, unknown>;
    // spec 成功信封：{ data: <业务负载> }，解包返回 data 内容
    if (json && typeof json === "object" && "data" in json) {
      return json.data as T;
    }
    // 兜底：无信封时原样返回
    return json as unknown as T;
  }

  // 非 JSON 成功响应（如文件流），原样返回文本
  return (await res.text()) as unknown as T;
}

/** 从 FastAPI 422 detail 数组提取可读消息 */
function extractDetailMessage(detail: unknown): string | null {
  if (Array.isArray(detail)) {
    return detail
      .map((d) => {
        if (typeof d === "string") return d;
        const loc = d?.loc ? `[${d.loc[d.loc.length - 1]}] ` : "";
        return loc + (d?.msg || d?.detail || String(d));
      })
      .join("; ") || null;
  }
  if (typeof detail === "string") return detail;
  return null;
}

/** 从任意异常提取用户友好消息 */
export function extractApiError(error: unknown, fallback = "操作失败"): string {
  if (error instanceof ApiError) return error.message;
  const message = (error as { message?: string })?.message;
  return message || fallback;
}

export const http = {
  get: <T>(path: string, opts?: RequestOptions) => request<T>("GET", path, opts),
  // 写方法对缺失 body 统一发送 "{}" 并携带 application/json：后端 Json 提取器
  // 要求该 Content-Type 与非空 body，缺一即 415（曾导致"手动登录"按钮不可用）
  post: <T>(path: string, body?: unknown, opts?: RequestOptions) =>
    request<T>("POST", path, { ...opts, rawBody: body instanceof FormData ? body : JSON.stringify(body ?? {}) }),
  put: <T>(path: string, body?: unknown, opts?: RequestOptions) =>
    request<T>("PUT", path, { ...opts, rawBody: body instanceof FormData ? body : JSON.stringify(body ?? {}) }),
  patch: <T>(path: string, body?: unknown, opts?: RequestOptions) =>
    request<T>("PATCH", path, { ...opts, rawBody: body instanceof FormData ? body : JSON.stringify(body ?? {}) }),
  delete: <T>(path: string, opts?: RequestOptions) => request<T>("DELETE", path, opts),
};
