/**
 * 直连任务草稿 ⇄ 载荷互转与缺口校验的单元测试。
 *
 * 锁定三类易错点：
 * 1. 证书策略是**三态**（`null` 跟随全局 / `true` / `false`），不能把 `null` 与 `false`
 *    混成同一个值——那会让"跟随全局设置"的用户被静默改成"严格校验"，自签门户直连失败；
 * 2. 缺字段兜底：后端 `serde(default)` 之后仍可能缺 name/method（老文件、手工编辑的
 *    JSON），编辑器打开时必须是可用的草稿而不是一片 undefined；
 * 3. 缺口只在**新建**时校验 ID 形态：已保存任务的 ID 来自服务端，重开编辑器不该报缺。
 */
import { describe, expect, it } from "vitest";
import type { HttpTaskConfig } from "@/api/types";
import {
  HTTP_TASK_DEFAULT_NAME,
  emptyHttpTaskDraft,
  httpTaskDraftFromConfig,
  httpTaskDraftGaps,
  httpTaskPayload,
} from "./httpTask";

function makeConfig(overrides: Partial<HttpTaskConfig> = {}): HttpTaskConfig {
  return {
    task_id: "dorm",
    name: "宿舍直连",
    description: "",
    method: "GET",
    url: "http://10.0.0.1/login?username={username}&password={password}",
    auth_url: "",
    headers: "",
    body: "",
    success_pattern: "",
    failure_pattern: "",
    crypto_script: "",
    pre_request: null,
    ignore_https_errors: null,
    ...overrides,
  };
}

describe("emptyHttpTaskDraft", () => {
  it("新建草稿：空 ID、默认名称、GET、证书跟随全局、标记为新建", () => {
    const draft = emptyHttpTaskDraft();
    expect(draft.id).toBe("");
    expect(draft.name).toBe(HTTP_TASK_DEFAULT_NAME);
    // method 缺省必须是 GET：后端 HttpRequestMethod::default() 也是 GET，
    // 若这里默认 POST，缺字段的任务打开后会显示成另一个方法
    expect(draft.method).toBe("GET");
    // null = 跟随全局 browser.ignore_https_errors，不是"忽略证书"也不是"严格校验"
    expect(draft.ignore_https_errors).toBeNull();
    expect(draft._isNew).toBe(true);
    // 文本字段一律为空串（编辑器直接 v-model，undefined 会让输入框变非受控）
    for (const key of [
      "description",
      "url",
      "auth_url",
      "headers",
      "body",
      "success_pattern",
      "failure_pattern",
      "crypto_script",
      "pre_request_url",
      "pre_request_headers",
      "pre_request_body",
      "pre_request_extract",
      "pre_request_name",
    ] as const) {
      expect(draft[key]).toBe("");
    }
    // 前置请求的方法与主请求同口径（后端 HttpRequestMethod::default() = GET）
    expect(draft.pre_request_method).toBe("GET");
  });
});

describe("httpTaskDraftFromConfig", () => {
  it("逐字段搬运并标记为已保存（_isNew = false）", () => {
    const draft = httpTaskDraftFromConfig(
      makeConfig({
        description: "门户直连",
        method: "POST",
        auth_url: "http://10.0.0.1/",
        headers: "Content-Type: application/x-www-form-urlencoded",
        body: "username={username}&password={password}",
        success_pattern: "登录成功",
        failure_pattern: "密码错误",
        crypto_script: "function transform(ctx) { return ctx; }",
      }),
    );
    expect(draft.id).toBe("dorm");
    expect(draft.name).toBe("宿舍直连");
    expect(draft.description).toBe("门户直连");
    expect(draft.method).toBe("POST");
    expect(draft.auth_url).toBe("http://10.0.0.1/");
    expect(draft.body).toBe("username={username}&password={password}");
    expect(draft.success_pattern).toBe("登录成功");
    expect(draft.failure_pattern).toBe("密码错误");
    expect(draft.crypto_script).toBe("function transform(ctx) { return ctx; }");
    expect(draft._isNew).toBe(false);
  });

  it("证书策略三态各自保留：null 跟随全局、true 忽略、false 严格", () => {
    expect(httpTaskDraftFromConfig(makeConfig({ ignore_https_errors: null })).ignore_https_errors).toBeNull();
    expect(httpTaskDraftFromConfig(makeConfig({ ignore_https_errors: true })).ignore_https_errors).toBe(true);
    // false 必须原样保留：塌成 null 会把"严格校验"改回"跟随全局"
    expect(httpTaskDraftFromConfig(makeConfig({ ignore_https_errors: false })).ignore_https_errors).toBe(false);
  });

  it("字段缺失时逐项兜底（老文件/手工编辑的 JSON 也要能打开）", () => {
    const draft = httpTaskDraftFromConfig({} as HttpTaskConfig);
    expect(draft.id).toBe("");
    expect(draft.name).toBe(HTTP_TASK_DEFAULT_NAME);
    expect(draft.method).toBe("GET");
    expect(draft.url).toBe("");
    expect(draft.ignore_https_errors).toBeNull();
    expect(draft._isNew).toBe(false);
  });

  it("名称为空串时回落默认名（下拉/列表里不能出现无名条目）", () => {
    expect(httpTaskDraftFromConfig(makeConfig({ name: "" })).name).toBe(HTTP_TASK_DEFAULT_NAME);
  });
});

describe("httpTaskPayload", () => {
  it("带上 type 标记并去掉首尾空白（task_id 不 trim：保存路径用请求 id 覆盖它）", () => {
    const payload = httpTaskPayload({
      ...emptyHttpTaskDraft(),
      id: "dorm",
      name: "  宿舍直连  ",
      description: "  门户直连  ",
      url: "  http://10.0.0.1/login  ",
      auth_url: "  http://10.0.0.1/  ",
    });
    expect(payload.type).toBe("http");
    expect(payload.task_id).toBe("dorm");
    expect(payload.name).toBe("宿舍直连");
    expect(payload.description).toBe("门户直连");
    expect(payload.url).toBe("http://10.0.0.1/login");
    expect(payload.auth_url).toBe("http://10.0.0.1/");
  });

  it("名称全空白时回落默认名（保存产物里不允许空名称）", () => {
    const payload = httpTaskPayload({ ...emptyHttpTaskDraft(), id: "dorm", name: "   ", url: "http://x/" });
    expect(payload.name).toBe(HTTP_TASK_DEFAULT_NAME);
  });

  it("请求头与请求内容原样保留（多行文本的首尾换行可能有意，不做 trim）", () => {
    const headers = "Content-Type: text/plain\nReferer: http://10.0.0.1/\n";
    const payload = httpTaskPayload({ ...emptyHttpTaskDraft(), headers, body: " a=1 " });
    expect(payload.headers).toBe(headers);
    expect(payload.body).toBe(" a=1 ");
  });
});

describe("httpTaskDraftGaps", () => {
  it("合法草稿没有缺口", () => {
    expect(httpTaskDraftGaps({ ...emptyHttpTaskDraft(), id: "dorm", url: "http://10.0.0.1/login" })).toEqual([]);
  });

  it("新建时校验 ID 形态（1~64 位字母、数字、下划线或连字符）", () => {
    const gaps = httpTaskDraftGaps({ ...emptyHttpTaskDraft(), id: "宿舍 直连", url: "http://10.0.0.1/login" });
    expect(gaps).toHaveLength(1);
    expect(gaps[0]).toContain("任务 ID");
  });

  it("已保存任务的 ID 不参与校验（ID 来自服务端，重开编辑器不该报缺）", () => {
    const gaps = httpTaskDraftGaps({
      ...emptyHttpTaskDraft(),
      id: "非 ASCII 的历史 ID",
      _isNew: false,
      url: "http://10.0.0.1/login",
    });
    expect(gaps).toEqual([]);
  });

  it("请求地址必填（缺了必然登不上）", () => {
    const gaps = httpTaskDraftGaps({ ...emptyHttpTaskDraft(), id: "dorm", url: "   " });
    expect(gaps).toEqual(["请求地址"]);
  });

  it("ID 与地址同时缺失时两条都报（一次说清要补什么）", () => {
    expect(httpTaskDraftGaps(emptyHttpTaskDraft())).toHaveLength(2);
  });

  it("判定关键字留空不算缺口（不少门户 HTTP 200 即成功，强制填写会逼用户编关键字）", () => {
    const gaps = httpTaskDraftGaps({
      ...emptyHttpTaskDraft(),
      id: "dorm",
      url: "http://10.0.0.1/login",
      success_pattern: "",
      failure_pattern: "",
    });
    expect(gaps).toEqual([]);
  });

  it("前置请求：地址非空但没说取哪个字段 → 报缺口（取不到值就发不出正确请求）", () => {
    const gaps = httpTaskDraftGaps({
      ...emptyHttpTaskDraft(),
      id: "dorm",
      url: "http://10.0.0.1/login",
      pre_request_url: "http://10.0.0.1/api/csrf-token",
    });
    expect(gaps).toHaveLength(1);
    expect(gaps[0]).toContain("取值方式");
  });

  it("前置请求：取值方式前缀不对要当场纠正（后端只认 json:）", () => {
    const gaps = httpTaskDraftGaps({
      ...emptyHttpTaskDraft(),
      id: "dorm",
      url: "http://10.0.0.1/login",
      pre_request_url: "http://10.0.0.1/api/csrf-token",
      pre_request_extract: 'regex:name="(.*)"',
    });
    expect(gaps).toHaveLength(1);
    expect(gaps[0]).toContain("json:");
  });

  it("前置请求：只填了取值方式没填地址 → 也报缺口（否则静默不生效）", () => {
    const gaps = httpTaskDraftGaps({
      ...emptyHttpTaskDraft(),
      id: "dorm",
      url: "http://10.0.0.1/login",
      pre_request_extract: "json:csrf_token",
    });
    expect(gaps).toHaveLength(1);
    expect(gaps[0]).toContain("请求地址");
  });
});

describe("前置请求的草稿 ⇄ 载荷", () => {
  const pre = {
    method: "GET" as const,
    url: "http://10.0.0.1/api/csrf-token",
    headers: "X-Requested-With: XMLHttpRequest",
    body: "",
    extract: "json:csrf_token",
    name: "csrf",
  };

  it("已保存任务的前置请求逐字段搬进草稿", () => {
    const draft = httpTaskDraftFromConfig(makeConfig({ pre_request: pre }));
    expect(draft.pre_request_method).toBe("GET");
    expect(draft.pre_request_url).toBe(pre.url);
    expect(draft.pre_request_headers).toBe(pre.headers);
    expect(draft.pre_request_body).toBe("");
    expect(draft.pre_request_extract).toBe("json:csrf_token");
    expect(draft.pre_request_name).toBe("csrf");
  });

  it("老配置（无 pre_request 键）与显式 null 都当「不需要」，草稿字段仍为空串", () => {
    for (const config of [makeConfig(), makeConfig({ pre_request: null })]) {
      const draft = httpTaskDraftFromConfig(config);
      expect(draft.pre_request_url).toBe("");
      expect(draft.pre_request_extract).toBe("");
      expect(draft.pre_request_method).toBe("GET");
    }
  });

  it("地址留空 → 载荷里是 null（后端 Option<HttpPreRequest>，null 与缺省同义）", () => {
    const payload = httpTaskPayload({
      ...emptyHttpTaskDraft(),
      id: "dorm",
      url: "http://10.0.0.1/login",
      // 只填了取值方式：不能因为这一栏有字就发出半个前置请求
      pre_request_extract: "json:csrf_token",
    });
    expect(payload.pre_request).toBeNull();
  });

  it("地址非空 → 载荷里是完整对象，且首尾空白被清掉", () => {
    const payload = httpTaskPayload({
      ...emptyHttpTaskDraft(),
      id: "dorm",
      url: "http://10.0.0.1/login",
      pre_request_url: "  http://10.0.0.1/api/csrf-token  ",
      pre_request_extract: "  json:csrf_token  ",
      pre_request_name: "  csrf  ",
    });
    expect(payload.pre_request).toEqual({
      method: "GET",
      url: "http://10.0.0.1/api/csrf-token",
      headers: "",
      body: "",
      extract: "json:csrf_token",
      name: "csrf",
    });
  });
});
