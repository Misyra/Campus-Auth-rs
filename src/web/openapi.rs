//! OpenAPI 契约文档骨架与生成管线（阶段 5 契约战役 T5-0）
//!
//! 三层职责：
//! 1. [`schemas`]：跨域共享的 ToSchema 形状（错误信封、data 包装），后续域
//!    批次一律引用本模块，不得在各 route 文件内重复定义信封；
//! 2. [`ApiDoc`]：utoipa 文档骨架——根元数据（openapi/info/servers）由生成侧
//!    接管，版本号随 `CARGO_PKG_VERSION` 自动同步（三端同版少一处手工项），
//!    paths 逐批挂载已注解域（当前为空骨架，system 样板切片随后接入）；
//! 3. [`merged_openapi_json`]：生成管线——utoipa 生成 spec 与手写 baseline
//!    （根目录 `openapi.json` 的未注解域条目）按路径合并，产出最终文档。
//!
//! 过渡期共存策略：**已注解域的路径条目以生成源为准**（整体替换手写条目），
//! 其余域维持手写条目；后续批次逐域补 `#[utoipa::path]` 注解，全量切换后
//! 手写 baseline 退役、本函数退化为纯生成。根目录 `openapi.json` 的重写由
//! 生成测试在 `UPDATE_OPENAPI=1` 时完成（见本模块 tests）。
//!
//! 确定性保障：utoipa 的 paths/components 为 BTreeMap（键序稳定），合并走
//! serde_json 的 BTreeMap（键恒排序），序列化固定 1 空格缩进 + 文件尾换行，
//! 与既有 baseline 版式一致——重复生成零 diff，避免无意义抖动。

use utoipa::OpenApi;

/// 跨域共享的契约形状（schema 模块）
///
/// 命名与引用规范（给后续域批次）：
/// - `Error*` 为错误信封：任何 `Result<_, ApiError>` 处理器的非 2xx 响应
///   一律 `body = ErrorResponse`（wire 形状 `{ "error": { code, message, details? } }`，
///   对齐 [`crate::web::error::ApiError::into_response`]）；
/// - `DataEnvelope<T>` 为成功响应包装：2xx 响应一律
///   `body = DataEnvelope<XxxData>`（wire 形状 `{ "data": <T> }`），
///   业务载荷类型以 `XxxData` 命名并定义在各自 route 文件内；
/// - 本模块类型同时派生 `Serialize`：供样板域强类型 handler 直接返回
///   （如 `health_check` 的 `Json<DataEnvelope<HealthData>>`）。
pub mod schemas {
    use serde::Serialize;
    use utoipa::ToSchema;

    /// 错误详情（`error.details`，因错误码而异，可省略）
    #[derive(Debug, Clone, Serialize, ToSchema)]
    pub struct ErrorDetail {
        /// 稳定的机器可读错误码（如 `BAD_REQUEST`，全表见
        /// [`crate::web::error::ApiError::code`]）
        pub code: String,
        /// 人类可读消息
        pub message: String,
        /// 补充详情（如 VALIDATION_ERROR 的字段级错误列表），可省略
        #[schema(value_type = Object, nullable = true)]
        pub details: Option<serde_json::Value>,
    }

    /// 统一错误响应信封 `{ "error": { ... } }`
    #[derive(Debug, Clone, Serialize, ToSchema)]
    pub struct ErrorResponse {
        /// 错误对象
        pub error: ErrorDetail,
    }

    /// 统一成功响应包装 `{ "data": T }`（禁止 `success` 字段）
    #[derive(Debug, Clone, Serialize, ToSchema)]
    pub struct DataEnvelope<T> {
        /// 响应数据
        pub data: T,
    }
}

/// OpenAPI 文档骨架（utoipa 生成源）
///
/// `info`/`servers` 由本骨架权威持有；`paths` 随域批次逐个挂载
/// `#[utoipa::path]` 注解处理器（当前为空骨架，首个域样板随后接入）。
#[derive(OpenApi)]
#[openapi(
    info(
        title = "Campus-Auth Web API",
        version = env!("CARGO_PKG_VERSION"),
        description = "认证喵本地管理界面 API。字段级契约以前端 frontend/src/api/types.ts 为准；\
            手写 baseline 中未注解域的响应 schema 仍为 `{}` 占位，已注解域（生成源条目）\
            携带真实 schema——两者按路径合并产出最终文档，全量切换后本说明随 baseline 退役。",
        contact(name = "Misyra", url = "https://github.com/Misyra/Campus-Auth-rs"),
    ),
    servers(
        (url = "http://127.0.0.1:50721", description = "本机管理界面默认端口"),
    ),
)]
pub struct ApiDoc;

/// 生成并合并最终 OpenAPI 文档：utoipa 生成 spec ⊕ 手写 baseline
///
/// 合并规则：
/// - 根字段（`openapi`/`info`/`servers`/`components`）整体由生成侧接管；
/// - `paths` 逐路径合并：生成侧条目（已注解域）**整体替换**手写条目，
///   未注解域保留手写条目原样。
pub fn merged_openapi_json() -> serde_json::Value {
    let generated = serde_json::to_value(ApiDoc::openapi()).expect("utoipa 文档应可序列化为 JSON");
    let mut merged: serde_json::Value = serde_json::from_str(include_str!("../../openapi.json"))
        .expect("根目录 openapi.json 应为合法 JSON");
    let root = merged
        .as_object_mut()
        .expect("openapi.json 顶层应为 JSON 对象");

    // 根字段：生成侧接管（生成侧缺省的键不强插，保留 baseline 原值兜底）
    for key in ["openapi", "info", "servers", "components"] {
        if let Some(value) = generated.get(key) {
            root.insert(key.to_string(), value.clone());
        }
    }
    // paths：逐路径覆盖，未注解域条目保留手写原文
    if let Some(generated_paths) = generated.get("paths").and_then(|p| p.as_object()) {
        let paths = root
            .entry("paths")
            .or_insert_with(|| serde_json::json!({}))
            .as_object_mut()
            .expect("paths 应为 JSON 对象");
        for (path, item) in generated_paths {
            paths.insert(path.clone(), item.clone());
        }
    }
    merged
}

/// 以与 baseline 一致的版式渲染快照：1 空格缩进 + 文件尾换行
///
/// serde_json 默认 pretty 是 2 空格；用自定义 formatter 对齐历史版式，
/// 避免重生成时产生全文件缩进噪音 diff。当前仅生成测试使用（快照落盘
/// 由 `UPDATE_OPENAPI=1` 驱动），故随测试编译。
#[cfg(test)]
fn render_snapshot(spec: &serde_json::Value) -> String {
    use serde::Serialize;
    let mut buf = Vec::new();
    let formatter = serde_json::ser::PrettyFormatter::with_indent(b" ");
    let mut serializer = serde_json::Serializer::with_formatter(&mut buf, formatter);
    spec.serialize(&mut serializer).expect("Value 序列化不失败");
    buf.push(b'\n');
    String::from_utf8(buf).expect("快照应为 UTF-8")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 生成 spec（合并后）必须覆盖全部路由表 method+path
    ///
    /// 与 `web::mod` 的 `openapi_json_matches_route_table`（文件级）互补：
    /// 本测试校验**生成管线**的不变量——即便后续批次把更多域改为生成源，
    /// 合并结果也必须完整覆盖路由表；文件只在其被重写（UPDATE_OPENAPI=1）
    /// 时才反映最新生成结果。
    #[test]
    fn merged_spec_covers_route_table() {
        let spec = merged_openapi_json();
        const METHODS: [&str; 5] = ["get", "post", "put", "patch", "delete"];

        let mut spec_set = std::collections::BTreeSet::new();
        let paths = spec
            .get("paths")
            .and_then(|p| p.as_object())
            .expect("合并 spec 应含 paths");
        for (path, ops) in paths {
            if !path.starts_with("/api/") && path != "/api" {
                continue;
            }
            for method in ops
                .as_object()
                .into_iter()
                .flat_map(|m| m.keys())
                .filter(|k| METHODS.contains(&k.as_str()))
            {
                spec_set.insert(format!("{} {}", method.to_uppercase(), path));
            }
        }

        for (method, path, _) in super::super::route_table() {
            assert!(
                spec_set.contains(&format!("{method} {path}")),
                "路由表条目 {method} {path} 未被合并 spec 覆盖"
            );
        }
    }

    /// 生成结果确定性：同版本代码重复生成必须逐字节一致（防抖动 diff）
    #[test]
    fn generation_is_deterministic() {
        let first = merged_openapi_json();
        let second = merged_openapi_json();
        assert_eq!(
            render_snapshot(&first),
            render_snapshot(&second),
            "两次生成的快照必须逐字节一致"
        );
    }
}
