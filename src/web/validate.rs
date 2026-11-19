//! Web 层共享校验助手：http/https URL 校验、位图 magic 识别与受限流式读取
//!
//! 供 routes 各域复用（config / profiles 的 URL 校验、background / repo 的
//! 图片识别与响应体上限读取），消除多处近逐字重复；各调用点的既有测试钉住
//! 行为与错误文案，改动须保持输出不变。

use futures::StreamExt;

use crate::web::error::ApiError;

/// http/https URL 通用校验（认证地址 / 重定向触发地址等共用，G2 单点语义）
///
/// 仅做 scheme 与主机名两级判定；SSRF 防护（DNS 钉扎、私网拒收）由
/// `crate::web::ssrf` 在实际出站请求时另行执行，本函数只挡格式错误。
pub(crate) fn validate_http_url(label: &str, url: &str) -> Result<(), ApiError> {
    let parsed = url::Url::parse(url)
        .map_err(|_| ApiError::BadRequest(format!("{label}格式非法: {url}")))?;
    match parsed.scheme() {
        "http" | "https" => {}
        _ => {
            return Err(ApiError::BadRequest(format!(
                "{label}仅支持 http/https，当前为: {}",
                parsed.scheme()
            )));
        }
    }
    let host = parsed
        .host_str()
        .ok_or_else(|| ApiError::BadRequest(format!("{label}缺少主机名: {url}")))?;
    if host.is_empty() {
        return Err(ApiError::BadRequest(format!("{label}缺少主机名: {url}")));
    }
    Ok(())
}

/// 位图格式（按 magic bytes 识别，background / repo 两域共享同一判定）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ImageMagic {
    Png,
    Jpeg,
    Gif,
    Webp,
    Bmp,
    Ico,
}

impl ImageMagic {
    /// 按 magic bytes 识别六种位图格式，未知字节返回 `None`
    ///
    /// SVG 可在直接导航时以同源文档执行脚本，本程序只接受不可执行的位图
    /// 格式，文本 / SVG / 未知字节一律返回 `None` 交由调用方拒绝。
    /// 各格式签名互不为前缀，判定顺序不影响结果。
    pub(crate) fn detect(bytes: &[u8]) -> Option<Self> {
        if bytes.len() >= 8 && &bytes[0..8] == b"\x89PNG\r\n\x1a\n" {
            Some(Self::Png)
        } else if bytes.len() >= 3 && &bytes[0..3] == b"\xFF\xD8\xFF" {
            Some(Self::Jpeg)
        } else if bytes.len() >= 6 && (&bytes[0..6] == b"GIF87a" || &bytes[0..6] == b"GIF89a") {
            Some(Self::Gif)
        } else if bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
            Some(Self::Webp)
        } else if bytes.len() >= 2 && &bytes[0..2] == b"BM" {
            Some(Self::Bmp)
        } else if bytes.len() >= 4 && &bytes[0..4] == b"\x00\x00\x01\x00" {
            Some(Self::Ico)
        } else {
            None
        }
    }

    /// 下发 Content-Type 用的 MIME
    pub(crate) fn mime(self) -> &'static str {
        match self {
            Self::Png => "image/png",
            Self::Jpeg => "image/jpeg",
            Self::Gif => "image/gif",
            Self::Webp => "image/webp",
            Self::Bmp => "image/bmp",
            Self::Ico => "image/x-icon",
        }
    }

    /// 落盘文件扩展名（不含 `.`；JPEG 归一为 jpg）
    pub(crate) fn ext(self) -> &'static str {
        match self {
            Self::Png => "png",
            Self::Jpeg => "jpg",
            Self::Gif => "gif",
            Self::Webp => "webp",
            Self::Bmp => "bmp",
            Self::Ico => "ico",
        }
    }
}

/// 由 magic bytes 识别图片格式，返回待下发的 MIME
///
/// SVG 等可执行格式与未知字节返回 `None`，由调用方拒绝（repo 截图代理口径）。
pub(crate) fn detect_image_mime(bytes: &[u8]) -> Option<&'static str> {
    ImageMagic::detect(bytes).map(ImageMagic::mime)
}

/// 根据 magic bytes 识别图片格式（Content-Type 缺失或不可信时兜底），
/// 返回规范化扩展名（不含 `.`；background 上传 / 拉取口径）
pub(crate) fn ext_from_magic(bytes: &[u8]) -> Option<&'static str> {
    ImageMagic::detect(bytes).map(ImageMagic::ext)
}

/// 将一个 chunk 追加到缓冲区，超过上限返回 `None`（不追加任何字节）
///
/// 独立成纯函数以便单测覆盖边界判定逻辑。
fn append_within_limit(buf: &mut Vec<u8>, chunk: &[u8], limit: usize) -> Option<()> {
    // 先判后拼：超限 chunk 一个字节都不落入缓冲，避免无谓的内存增长
    if buf.len().checked_add(chunk.len()).is_none_or(|n| n > limit) {
        return None;
    }
    buf.extend_from_slice(chunk);
    Some(())
}

/// 流式读取响应体并施加字节上限，累计超过上限立即中止
///
/// 大小上限不能只信任可缺失或可伪造的 Content-Length，必须在流式读取中实测
/// （背景图远程拉取与仓库代理 JSON / 截图三处共用）；`on_overflow` 与
/// `on_read_err` 由调用方注入端点专属的错误文案与 debug 日志，保持各端点
/// 既有响应不变。
pub(crate) async fn read_stream_capped<Overflow, ReadErr>(
    response: reqwest::Response,
    limit: usize,
    on_overflow: Overflow,
    on_read_err: ReadErr,
) -> Result<Vec<u8>, ApiError>
where
    Overflow: Fn() -> ApiError,
    ReadErr: Fn(&reqwest::Error) -> ApiError,
{
    let mut body = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| on_read_err(&e))?;
        // 借用传入：闭包须在循环内可重复调用（&F 本身实现 Fn）
        append_within_limit(&mut body, &chunk, limit).ok_or_else(&on_overflow)?;
    }
    Ok(body)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ============ http/https URL 校验 ============

    #[test]
    fn validate_http_url_accepts_http_and_https_with_host() {
        assert!(validate_http_url("认证地址", "http://portal.example.edu.cn/").is_ok());
        assert!(validate_http_url("认证地址", "https://portal.example.edu.cn/login").is_ok());
    }

    #[test]
    fn validate_http_url_rejects_bad_scheme_and_parse_failure() {
        // 非 http/https scheme
        let err = validate_http_url("认证地址", "ftp://example.com/a").unwrap_err();
        assert!(err.to_string().contains("仅支持 http/https"));
        // 解析失败
        let err = validate_http_url("认证地址", "not-a-url").unwrap_err();
        assert!(err.to_string().contains("格式非法"));
        // 「缺少主机名」分支为防御性保留：http/https（特殊 scheme）解析成功后
        // host 必非空（如 `http:///path` 会被 url crate 归一为 `http://path/`）
    }

    // ============ 位图 magic 识别 ============

    #[test]
    fn image_magic_detect_covers_six_formats_and_ext_mime_consistent() {
        let samples: &[(ImageMagic, &[u8])] = &[
            (ImageMagic::Png, b"\x89PNG\r\n\x1a\nxxxx"),
            (ImageMagic::Jpeg, b"\xFF\xD8\xFF\xE0xxxx"),
            (ImageMagic::Gif, b"GIF89a"),
            (ImageMagic::Webp, b"RIFFxxxxWEBP"),
            (ImageMagic::Bmp, b"BMxxxx"),
            (ImageMagic::Ico, b"\x00\x00\x01\x00xxxx"),
        ];
        for (kind, bytes) in samples {
            assert_eq!(ImageMagic::detect(bytes), Some(*kind));
            // 两个便捷出口与枚举访问器一致
            assert_eq!(detect_image_mime(bytes), Some(kind.mime()));
            assert_eq!(ext_from_magic(bytes), Some(kind.ext()));
        }
        // SVG / 文本 / 空字节拒绝
        assert_eq!(ImageMagic::detect(b"<svg xmlns='x'></svg>"), None);
        assert_eq!(ImageMagic::detect(b"hello"), None);
        assert_eq!(ImageMagic::detect(b""), None);
        // JPEG 归一扩展名为 jpg
        assert_eq!(ImageMagic::Jpeg.ext(), "jpg");
    }

    // ============ 响应体上限判定（自 repo 域随 append_within_limit 上提） ============

    /// 追加不超过上限时成功，恰好达到上限仍允许（边界为“不超过”）
    #[test]
    fn test_append_within_limit_accepts_up_to_boundary() {
        let mut buf = Vec::new();
        assert!(append_within_limit(&mut buf, b"abc", 8).is_some());
        assert_eq!(buf, b"abc");
        // 恰好填满到上限：允许
        assert!(append_within_limit(&mut buf, b"defgh", 8).is_some());
        assert_eq!(buf.len(), 8);
    }

    /// 超过上限返回 None 且不追加任何字节（缓冲长度保持不变）
    #[test]
    fn test_append_within_limit_rejects_overflow_without_append() {
        let mut buf = Vec::new();
        buf.extend_from_slice(b"0123456789"); // 已有 10 字节
        assert!(append_within_limit(&mut buf, b"abc", 12).is_none());
        assert_eq!(buf.len(), 10, "超限 chunk 不应部分或全部落入缓冲");
        // 恰好等于上限（10 + 2 = 12）仍允许
        assert!(append_within_limit(&mut buf, b"ab", 12).is_some());
        assert_eq!(buf.len(), 12);
    }
}
