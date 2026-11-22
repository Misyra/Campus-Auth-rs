/**
 * HTTP 客户端封装：fetch + 自动解包 + 统一错误处理。
 * 所有 API 调用通过此模块暴露的端点函数，禁止在业务代码中直接 fetch。
 *
 * 契约（详见根目录 openapi.json 与后端实现）：
 * - 成功响应统一为 `{ "data": <业务负载> }`，列表端点 data 直接是数组；
 * - 错误响应统一为 `{ "error": { "code": "...", "message": "...", "details": {...} } }`；
 * - 无 success 字段，HTTP 状态码非 2xx 即为错误。
 *
 * 鉴权信任模型（bootstrap 握手）：免鉴权的 GET /api/auth/token 已移除，token 只能经
 * `POST /api/auth/exchange` 换取——凭据为 URL fragment 的引导秘钥（托盘打开界面时
 * 携带，换取成功即剥除）或管理密码（远程访问引导页，仅服务端非回环监听接受）。
 * token 服务端持久化，前端缓存于内存 + 本地存储（回环 localStorage / 远程
 * sessionStorage，见下方 ensureAuthToken 注释）；
 * 无凭据可用即「引导态」，由 App.vue 挂载 AuthBootstrapGuide 接管。
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
 * 鉴权信任模型（回环本地信任 + bootstrap 握手）：
 *
 * - **回环访问（本机桌面）**：后端 `GET /api/auth/local-token` 直接下发 token
 *   （非回环绑定 404），前端在无 fragment、无缓存时自动取用——纯本地使用
 *   「打开即用」，不依赖托盘链接。
 * - **远程访问（Docker/LAN）**：token 只能经 `POST /api/auth/exchange` 用
 *   管理密码换取；该分支仅服务端非回环监听时接受。
 * - 托盘/launcher 打开界面时 URL 仍携带 `#auth=<bootstrap>` fragment，前端
 *   用它换取 token 并立即 `history.replaceState` 剥除（fragment 不发往服务器、
 *   不进日志）；与 local-token 等价，保留给托盘入口与跨浏览器打开。
 *
 * token 由后端持久化（重启不变），换取成功后按访问来源分存：回环访问（本机桌面）
 * 同时写 localStorage——同浏览器任意标签页、浏览器重启后都能直接复用，本地项目
 * 「打开即用」；远程访问（Docker/LAN）只写 sessionStorage，避免共享电脑上的浏览器
 * 长期保留凭据。引导态（无 token 也无可用秘钥/密码）由 `App.vue` 挂载
 * `AuthBootstrapGuide` 接管。
 */
const TOKEN_STORAGE_KEY = "campus_auth_token";
const AUTH_TOKEN_TIMEOUT_MS = 10000;
/** 引导秘钥形如 64 位 hex；放宽到 8~128 以容纳未来的长度调整 */
const BOOTSTRAP_SECRET_PATTERN = /^[0-9a-fA-F]{8,128}$/;

let authToken: string | null = null;
let tokenPromise: Promise<string | null> | null = null;

/**
 * 「鉴权就绪」事件订阅（P2-7）：token 经**真实引导**（exchange 交换 / 回环
 * local-token 下发）成功取得时通知订阅方，供 WS 日志通道与状态轮询把启动/重连
 * 排在鉴权就绪之后——引导完成前不发起匿名请求风暴。
 *
 * 事件只在引导路径成功时触发，**缓存读取路径不发**（内存/本地存储命中）：WS 重连
 * 的常规流程每轮都经 `refreshAuthToken` 从缓存恢复同一 token，若也发事件会把重连
 * 退避反复重置成紧密循环（连接风暴的另一种形态）。
 */
type AuthReadyListener = (token: string) => void;
const authReadyListeners: AuthReadyListener[] = [];

/** 订阅鉴权就绪事件；返回退订函数 */
export function onAuthReady(listener: AuthReadyListener): () => void {
  authReadyListeners.push(listener);
  return () => {
    const i = authReadyListeners.indexOf(listener);
    if (i !== -1) authReadyListeners.splice(i, 1);
  };
}

function notifyAuthReady(token: string): void {
  for (const listener of [...authReadyListeners]) {
    try {
      listener(token);
    } catch (e) {
      frontendLogger.warn("auth", "鉴权就绪回调执行失败", e);
    }
  }
}

/**
 * 读取 token 缓存：localStorage 优先（回环跨标签/重启），回退 sessionStorage
 * （远程与旧版本缓存）。存储被禁用（隐私模式等）时静默降级。
 */
function readStoredToken(): string | null {
  try {
    const persisted = localStorage.getItem(TOKEN_STORAGE_KEY);
    if (persisted) return persisted;
  } catch {
    /* 隐私模式等场景跳过 localStorage */
  }
  try {
    return sessionStorage.getItem(TOKEN_STORAGE_KEY);
  } catch {
    return null;
  }
}

/** 写入 token 缓存；失败不影响内存缓存可用性 */
function writeStoredToken(token: string): void {
  try {
    sessionStorage.setItem(TOKEN_STORAGE_KEY, token);
  } catch {
    /* 隐私模式等场景降级为仅内存缓存 */
  }
  // 回环访问额外落 localStorage：新标签页与浏览器重启后无需 fragment 即可打开
  if (!isLoopbackAccess()) return;
  try {
    localStorage.setItem(TOKEN_STORAGE_KEY, token);
  } catch {
    /* localStorage 被禁用时仍保留 sessionStorage 与内存缓存 */
  }
}

/** 清除 token 缓存（仅经 resetAuthToken：401 证实的失效才内存与两处存储同步废弃） */
function clearStoredToken(): void {
  try {
    localStorage.removeItem(TOKEN_STORAGE_KEY);
  } catch {
    /* ignore */
  }
  try {
    sessionStorage.removeItem(TOKEN_STORAGE_KEY);
  } catch {
    /* ignore */
  }
}

/** 当前是否为回环访问（127.0.0.1 / localhost / [::1]）；与后端 Host 头校验同口径 */
export function isLoopbackAccess(): boolean {
  if (typeof location === "undefined") return true;
  // URL 规范里 IPv6 hostname 带方括号，两种写法都接受
  return ["127.0.0.1", "localhost", "::1", "[::1]"].includes(location.hostname);
}

/** 当前是否为远程（非回环）访问：决定引导页展示密码表单还是托盘指引 */
export function isRemoteAccess(): boolean {
  return !isLoopbackAccess();
}

/** 是否已有可用 token（内存或本地存储）：为 false 且首检完成时展示引导页 */
export function hasUsableAuthToken(): boolean {
  return Boolean(authToken) || Boolean(readStoredToken());
}

/**
 * 从任意输入解析引导秘钥：接受完整带 `#auth=<hex>` 的 URL 或裸秘钥（引导页粘贴框用）。
 * 无法识别时返回 null（调用方给出可纠正的提示，而不是盲目发起必败的请求）。
 */
export function parseBootstrapSecret(raw: string): string | null {
  const text = raw.trim();
  if (!text) return null;
  const idx = text.indexOf("#auth=");
  if (idx !== -1) {
    const secret = text.slice(idx + "#auth=".length).trim();
    return BOOTSTRAP_SECRET_PATTERN.test(secret) ? secret : null;
  }
  return BOOTSTRAP_SECRET_PATTERN.test(text) ? text : null;
}

/** 读取当前 URL fragment 中的引导秘钥（形如 `#auth=<hex>`）；无或不合法返回 null */
function readHashBootstrapSecret(): string | null {
  if (typeof location === "undefined") return null;
  return location.hash.startsWith("#auth=") ? parseBootstrapSecret(location.hash) : null;
}

/** 剥除 URL 中的 auth fragment：token 已入缓存，地址栏残留秘钥只会带来泄露面 */
function stripAuthHash(): void {
  try {
    history.replaceState(null, "", location.pathname + location.search);
  } catch {
    /* replaceState 失败不影响功能，仅地址栏残留 */
  }
}

/** 引导交换的结果：成功带 token；失败带稳定错误码与中文用户可读信息 */
export type BootstrapExchangeOutcome =
  | { ok: true; token: string }
  | { ok: false; code: "AUTH_NOT_CONFIGURED" | "AUTH_REJECTED" | "NETWORK_ERROR"; message: string };

/**
 * POST /api/auth/exchange 换取 token（bootstrap 秘钥或管理密码恰含其一）。
 *
 * 注意：此处必须用裸 fetch——经 request() 会因 ensureAuthToken 递归；同时 axum 的
 * Json 提取器要求显式 `Content-Type: application/json`（缺一即 415）。成功即写
 * 内存 + 本地存储缓存并剥除 URL 中的 auth fragment。
 */
async function exchangeBootstrapAuth(
  payload: { bootstrap?: string; password?: string },
  rejectMessage: string,
): Promise<BootstrapExchangeOutcome> {
  const controller = new AbortController();
  const timeoutId = setTimeout(() => controller.abort(), AUTH_TOKEN_TIMEOUT_MS);
  try {
    const res = await fetch(`${BASE}/api/auth/exchange`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(payload),
      signal: controller.signal,
    });
    if (res.ok) {
      const json = (await res.json().catch(() => null)) as { data?: { token?: string } } | null;
      const token = json?.data?.token;
      if (typeof token === "string" && token) {
        authToken = token;
        writeStoredToken(token);
        stripAuthHash();
        // 引导交换成功（fragment / 管理密码 / 粘贴秘钥）：通知鉴权就绪（P2-7）
        notifyAuthReady(token);
        return { ok: true, token };
      }
      return { ok: false, code: "AUTH_REJECTED", message: "服务返回的凭据无效，请重试" };
    }
    // 401 错误信封：{ error: { code, message } }；AUTH_NOT_CONFIGURED 需要引导页
    // 展示服务端配置指引，必须原样透传错误码
    if (res.status === 401) {
      const errJson = (await res.json().catch(() => null)) as
        | { error?: { code?: string; message?: string } }
        | null;
      if (errJson?.error?.code === "AUTH_NOT_CONFIGURED") {
        return {
          ok: false,
          code: "AUTH_NOT_CONFIGURED",
          // 消息原样透传（引导页会以自己的固定指引文案展示该分支）
          message: errJson.error.message || "服务端尚未配置管理密码",
        };
      }
      return {
        ok: false,
        code: "AUTH_REJECTED",
        message: errJson?.error?.message || rejectMessage,
      };
    }
    return { ok: false, code: "AUTH_REJECTED", message: rejectMessage };
  } catch {
    // 超时与网络失败同语义：引导页据此后退回「稍后重试」路径
    return { ok: false, code: "NETWORK_ERROR", message: "无法连接服务，请确认程序已启动后重试" };
  } finally {
    clearTimeout(timeoutId);
  }
}

/**
 * 用引导秘钥换取 token（引导页粘贴框入口；URL fragment 路径在 ensureAuthToken 内部）。
 */
export function submitBootstrapSecret(secret: string): Promise<BootstrapExchangeOutcome> {
  return exchangeBootstrapAuth(
    { bootstrap: secret },
    "引导秘钥无效或已过期，请从系统托盘重新获取界面链接",
  );
}

/**
 * 用管理密码换取 token（远程访问引导页入口）。
 * 401 + `AUTH_NOT_CONFIGURED` 原样透传：服务端未配置 management_password（或
 * 环境变量 CAMPUS_AUTH_MANAGEMENT_PASSWORD）时，密码再正确也换不到 token。
 */
export function submitManagementPassword(password: string): Promise<BootstrapExchangeOutcome> {
  return exchangeBootstrapAuth({ password }, "管理密码不正确");
}

/**
 * GET /api/auth/local-token：回环监听（或显式豁免无凭据绑定的 Docker 默认部署）
 * 下后端直接下发 token（本地免引导）。其余非回环绑定返回 404，远程访问不走该
 * 路径；网络失败/超时一律返回 null，由 `ensureAuthToken` 按引导态处理。成功即
 * 写入内存与本地存储，后续请求与 WS 复用同一 token。
 */
async function fetchLocalToken(): Promise<string | null> {
  const controller = new AbortController();
  const timeoutId = setTimeout(() => controller.abort(), AUTH_TOKEN_TIMEOUT_MS);
  try {
    const res = await fetch(`${BASE}/api/auth/local-token`, {
      headers: { Accept: "application/json" },
      signal: controller.signal,
    });
    if (!res.ok) return null;
    const json = (await res.json().catch(() => null)) as { data?: { token?: string } } | null;
    const token = json?.data?.token;
    if (typeof token !== "string" || !token) return null;
    authToken = token;
    writeStoredToken(token);
    // 回环免引导 token 下发成功：通知鉴权就绪（P2-7）。REST 401 触发的重新引导
    // （resetAuthToken 清缓存后重走本端点）也经此把新 token 通知到 WS 重连方
    notifyAuthReady(token);
    return token;
  } catch {
    // 后端未起 / 非回环 404 / 超时：保持引导态判定不变
    return null;
  } finally {
    clearTimeout(timeoutId);
  }
}

/**
 * 获取（并缓存）鉴权 token；无可用凭据时返回 null（后端将拒绝后续请求并返回 401，
 * 引导态由 `App.vue` 的 `AuthBootstrapGuide` 接管）。
 *
 * 取用顺序：内存缓存 → URL fragment 引导秘钥（换取成功即剥除）→ 本地存储缓存
 * （回环 localStorage 优先，回退 sessionStorage）→ 回环 local-token 免引导。
 * fragment 优先于缓存：托盘带来的秘钥必然新鲜，且换取后剥除 fragment，避免
 * 失效秘钥残留在地址栏。
 */
export function ensureAuthToken(): Promise<string | null> {
  if (authToken) return Promise.resolve(authToken);
  if (!tokenPromise) {
    const pending = (async () => {
      const secret = readHashBootstrapSecret();
      if (secret) {
        const outcome = await submitBootstrapSecret(secret);
        if (outcome.ok) return outcome.token;
        frontendLogger.warn("auth", "引导秘钥换取 token 失败", outcome.message);
        // 交换失败不立即进入引导态：本地存储可能仍有有效 token
        //（如同浏览器先前引导过、之后又贴入一条过期链接）
      }
      const cached = readStoredToken();
      if (cached) {
        authToken = cached;
        return cached;
      }
      // 回环监听的本地免引导：后端直接下发 token（非回环 404，静默跳过）
      const local = await fetchLocalToken();
      if (local) return local;
      return null;
    })();
    tokenPromise = pending;
    void pending.finally(() => {
      // resetAuthToken 后可能已经开始新一轮引导，旧请求不得清空新请求的共享状态。
      if (tokenPromise === pending) tokenPromise = null;
    });
  }
  return tokenPromise;
}

/**
 * 重置 token 缓存（401 时调用，token 已被证实失效）：内存与本地存储同步失效。
 * 后端 token 已持久化（重启不变），触发 401 说明凭据被吊销/轮换，旧缓存必须丢弃；
 * 此后无 fragment 也无缓存时 `ensureAuthToken` 返回 null，交给引导态。
 */
function resetAuthToken(): void {
  authToken = null;
  tokenPromise = null;
  clearStoredToken();
}

/**
 * 强制刷新 token 并返回新值：WS 重连前调用。
 *
 * 语义分界（token 由后端持久化，正常重启不变）：
 * - **常规路径——仅清内存缓存**：本地存储保留，ensureAuthToken 的读取链
 *   （内存 → hash 交换 → 本地存储）会从中恢复出原 token，WS 重连仍带同一
 *   token。后端重启场景下旧 token 依然有效，页面不再被迫回到引导页重新引导。
 * - **轮换路径——内存与存储全清**：仅当后端秘钥文件损坏成对重建（token 真被轮换）
 *   时，恢复出的旧 token 会在后续 HTTP 请求收到 401，由 request() 调用
 *   resetAuthToken 把内存与两处存储一并清空 → 无凭据可用时交给引导页兜底。
 *
 * 失败返回 null，调用方以匿名建连（后端 401 关掉后走正常退避，不比旧行为差）。
 */
export function refreshAuthToken(): Promise<string | null> {
  authToken = null;
  tokenPromise = null;
  return ensureAuthToken();
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
/** 未显式传 timeout 的请求默认 30s 超时：后端卡死/连接假死时快速报错而非永久转圈。
 *  长操作（登录/环境引导/OCR/AI）各自显式传入更大超时，不受影响。 */
const DEFAULT_TIMEOUT_MS = 30000;

async function request<T>(method: string, path: string, opts: RequestOptions = {}, retried = false): Promise<T> {
  const controller = new AbortController();
  const timeoutMs = opts.timeout ?? DEFAULT_TIMEOUT_MS;
  let timedOut = false;
  const abortFromCaller = () => controller?.abort(opts.signal?.reason);
  if (controller && opts.signal) {
    if (opts.signal.aborted) abortFromCaller();
    else opts.signal.addEventListener("abort", abortFromCaller, { once: true });
  }
  const timeoutId: ReturnType<typeof setTimeout> = setTimeout(() => {
    timedOut = true;
    controller.abort();
  }, timeoutMs);
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
  }

  // 注意：超时定时器与 abort 监听在**响应体消费完成后**才清理（见下层 finally），
  // 不能只覆盖到「收到响应头」——若头已回但 body 卡住/半途断流，`res.json()`
  // 将无超时、不可取消而永久 pending（表现为永久转圈、busy 标志永不复位）。

  try {
    // 401：token 可能因后端重启而轮换，重取一次后重试（仅一次，避免循环）
    if (res.status === 401 && !retried) {
      resetAuthToken();
      return await request<T>(method, path, opts, true);
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
        } catch (e) {
          // body 消费期间被超时/取消打断：如实报出，不可静默忽略
          if ((e as Error).name === "AbortError") {
            throw new ApiError(timedOut ? "请求超时" : "请求已取消", undefined, e, undefined, !timedOut);
          }
          /* 其余解析失败忽略：保留上面的通用消息 */
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
  } catch (e) {
    // 响应体读取超时/被取消：与 header 阶段同语义地归一为 ApiError
    if ((e as Error).name === "AbortError") {
      throw new ApiError(timedOut ? "请求超时" : "请求已取消", undefined, e, undefined, !timedOut);
    }
    throw e;
  } finally {
    // 超时定时器与 abort 监听覆盖到响应体解析结束（上面两处 try 的所有出口）
    if (timeoutId) clearTimeout(timeoutId);
    opts.signal?.removeEventListener("abort", abortFromCaller);
  }
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

/**
 * 是否为"无可用浏览器"失败（后端 `browser::NO_BROWSER_MESSAGE` 及其兜底后缀）。
 * 匹配子串固定为"无可用浏览器"：改动后端文案时必须同步此处，否则手动登录
 * 的弹窗引导会静默退化成普通通知（见 `useUi.manualLogin` 与 `client.test.ts`）。
 */
export function isNoBrowserMessage(message: unknown): boolean {
  return typeof message === "string" && message.includes("无可用浏览器");
}

/**
 * 是否为"资源已存在"冲突（后端 `ApiError::Conflict` → HTTP 409 + code `CONFLICT`）。
 *
 * 存在的理由：定时任务的"新建"是 `POST /api/scheduler/jobs`，而该端点对已存在 id 明确
 * 返回 409。自动保存场景下"两发重叠"是正常时序（首发与补发都以为自己是第一次），
 * 这时 409 的真实含义是"另一发已经把它建好了"——调用方据此降级为 PUT 继续，
 * 而不是弹一句"定时任务 X 已存在"的红字（任务其实建成功了）。
 */
export function isConflictError(error: unknown): boolean {
  return error instanceof ApiError && (error.status === 409 || error.code === "CONFLICT");
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
