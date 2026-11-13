//! 发布清单 ed25519 离线签名：公钥嵌入策略、规范化字节与签名校验
//!
//! ## 信任模型
//!
//! SHA256 摘要校验保证「下载到的包 == 清单声明的包」，但清单本身（自定
//! `latest.json` / GitHub Release API 合成）只受 HTTPS 传输保护：清单源被劫持
//! （仓库被入侵、自建镜像被篡改、用户误配恶意镜像源）时，攻击者可以换包并补
//! 一份匹配的假摘要。ed25519 签名在清单完整性之上加一道与传输通道和发布源
//! 无关的信任锚——清单由发布方私钥签名，客户端用编译期嵌入的公钥校验。
//! 各平台包的 `sha256` 字段语义不变，签名只是清单之上的信任背书。
//!
//! ## 验证策略（按构建形态二选一）
//!
//! - **嵌公钥构建**：release 流程在签名私钥（GitHub Secret
//!   `MANIFEST_SIGNING_KEY`，绝不入仓）存在时以 `CAMPUS_MANIFEST_SIGNING=1`
//!   调用构建（见 `build.rs`），公钥取自仓内 `resources/manifest-pubkey` 经
//!   `option_env!` 编译进二进制。清单缺失 `signature` 字段或校验失败一律拒绝
//!   （[`UpdaterError::ManifestSignatureInvalid`]，fail-closed，不降级）；
//! - **未嵌公钥构建**（默认开发/CI 构建）：不做签名校验，行为与本功能引入前
//!   完全一致（历史未签名清单照常可用）。
//!
//! **同源同批**是本机制的安全语义核心：公钥嵌入与清单签名必须来自同一批发布
//! （release 流程要么同时「嵌公钥 + 为清单签名」，要么两者都不做）。绝不允许
//! 出现嵌了公钥却发出未签名清单的版本——那会让该版本之后的所有客户端拒绝一切
//! 更新（客户端无法升级修复，只能整包重装）。
//!
//! ## 签名对象与规范化选择
//!
//! 签名对象是**清单文档剥离 `signature` 字段后的确定性规范化 JSON 字节**
//! （对象键递归按字典序排序、无空白、serde_json 标准转义），不是文件原文。
//! 选择理由：
//!
//! 1. `signature` 字段内嵌在清单文档自身，原文签名是循环定义，必须先剥离
//!    签名字段再对剩余内容签名；
//! 2. 镜像站/代理可能重排键序或改变空白后转发清单，规范化让「内容相同、
//!    序列化不同」的清单验证一致通过，镜像无需逐字节保真；
//! 3. 清单只含字符串/整数/布尔/嵌套对象（无浮点数），该规范化规则完全确定且
//!    跨版本一致——验证端可能是任意历史版本的客户端，规范化规则一经发布
//!    即不可变更，故刻意不追求 RFC 8785 全集（数字转义、Unicode 归一化），
//!    只钉死清单实际用到的子集。
//!
//! ## 资源版本兼容语义
//!
//! - 旧清单（无 `signature` 字段）在未嵌公钥构建下照常接受（同现状）；
//! - 旧清单在嵌公钥构建下被拒绝：嵌公钥只发生在签名启用后的发布上，其客户端
//!   检查的发布必然带签名（「同源同批」）；仅镜像源未同步 latest.json 签名
//!   文件时会遇到，拒绝是预期安全行为；
//! - GitHub Release API 路径：优先取发布附带的 `latest.json` 资产（发布方对
//!   完整清单的整体签名，见 `check.rs`）。嵌公钥构建下资产**缺失或不可用**
//!   （下载失败 / 解析失败 / 签名校验失败）一律直接拒绝，绝不回退逐资产合成
//!   清单——合成清单无签名字段，属纯 TLS 信任，与嵌钥构建的签名信任锚不可
//!   共存（资产缺失报 [`UpdaterError::SignedManifestMissing`]，其余错误原样
//!   透传）；未嵌公钥构建不做校验，资产不可用时照旧回退合成清单（与现状
//!   一致）。
//! - 升级窗口：签名启用（Secret 注入）后的第一个发布 tag 起按「同源同批」恒
//!   同时产出「公钥嵌入 + 已签名清单资产」。Stable 路径每次检查取当前
//!   releases/latest，旧 release 自然不再被命中；通道枚举路径在最高候选缺当前
//!   平台资产时会尝试更低候选，签名启用前的旧发布在该异常窗口内会被拒绝（见
//!   上条），待下一个正常发布出现即恢复。镜像源未同步 latest.json 资产时的
//!   拒绝同理，是预期安全行为。
//!
//! ## 签名工具链
//!
//! 签名/复核由 `campus-auth-helper --sign-manifest` / `--verify-manifest`
//! 完成（见 `helper_main.rs`），密钥生成用 `--gen-signing-key`；三者都调用本
//! 模块的函数，与客户端校验共用同一套规范化实现，不存在两份算法漂移。

use base64::Engine as _;
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};

use crate::updater::error::UpdaterError;

/// 编译期嵌入的清单签名公钥（base64 标准字母表，32 字节）
///
/// 由 `build.rs` 在 `CAMPUS_MANIFEST_SIGNING=1` 时从 `resources/manifest-pubkey`
/// 注入；未设置时为 `None`，更新器对清单不做签名校验。
fn embedded_pubkey_b64() -> Option<&'static str> {
    option_env!("CAMPUS_MANIFEST_PUBKEY_B64")
}

/// 解析编译期嵌入的公钥
///
/// `build.rs` 已在构建期校验格式，此处解析失败属防御分支：按未启用签名校验
/// 处理（保可用性）并显著告警，不让一次构建事故静默摧毁更新可用性。
/// `pub(crate)` 供 `check.rs` GitHub Release 路径取钥并按构建形态分流回退
/// 口径（见 [`verify_manifest_value`] 与 `check.rs` 的对应注释）。
pub(crate) fn embedded_verifying_key() -> Option<VerifyingKey> {
    let b64 = embedded_pubkey_b64()?;
    match decode_pubkey(b64) {
        Ok(key) => Some(key),
        Err(e) => {
            tracing::warn!("编译期嵌入的清单签名公钥无法解析，按未启用签名校验处理: {e}");
            None
        }
    }
}

/// 解析 base64 编码的 ed25519 公钥（`pub(crate)` 供 `check.rs` 测试注入密钥）
pub(crate) fn decode_pubkey(pubkey_b64: &str) -> Result<VerifyingKey, String> {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(pubkey_b64.trim())
        .map_err(|e| format!("公钥 base64 解码失败: {e}"))?;
    let arr: [u8; 32] = bytes
        .try_into()
        .map_err(|v: Vec<u8>| format!("公钥长度应为 32 字节，实际 {} 字节", v.len()))?;
    VerifyingKey::from_bytes(&arr).map_err(|e| format!("公钥不合法: {e}"))
}

/// 确定性规范化序列化（对象键递归按字典序排序、无空白）
///
/// 仅覆盖清单用到的 JSON 子集：null / bool / 整数 / 字符串 / 数组 / 对象。
/// 字符串转义交由 serde_json（其转义规则总产出合法 JSON，序列化字符串值不会失败）。
fn canonical_json(value: &serde_json::Value, out: &mut String) {
    match value {
        serde_json::Value::Null => out.push_str("null"),
        serde_json::Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        serde_json::Value::Number(n) => out.push_str(&n.to_string()),
        serde_json::Value::String(s) => {
            out.push_str(&serde_json::to_string(s).unwrap_or_else(|_| format!("{s:?}")))
        }
        serde_json::Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                canonical_json(item, out);
            }
            out.push(']');
        }
        serde_json::Value::Object(map) => {
            // 显式排序而非依赖 serde_json::Map 的内部实现（BTreeMap）：防止未来
            // 有人开启 preserve_order 特性后键序随插入序漂移，破坏规范化稳定性
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            out.push('{');
            for (i, key) in keys.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(&serde_json::to_string(key).unwrap_or_else(|_| format!("{key:?}")));
                out.push(':');
                canonical_json(&map[*key], out);
            }
            out.push('}');
        }
    }
}

/// 清单文档的待签名字节：剥离 `signature` 字段后做确定性规范化
fn manifest_signing_bytes(manifest: &serde_json::Value) -> Vec<u8> {
    let mut doc = manifest.clone();
    if let serde_json::Value::Object(map) = &mut doc {
        map.remove("signature");
    }
    let mut out = String::new();
    canonical_json(&doc, &mut out);
    out.into_bytes()
}

/// 清单接受前的签名严格校验（`check.rs` 自定清单路径的统一入口）
///
/// 嵌公钥构建：签名缺失或校验失败 → [`UpdaterError::ManifestSignatureInvalid`]；
/// 未嵌公钥构建：不校验直接放行，与现状一致（兼容语义见模块 doc）。
/// GitHub Release 路径不直接走本函数：该路径需按构建形态分流回退口径，且
/// 测试要注入显式公钥，故由 `check.rs` 经 [`embedded_verifying_key`] 取钥后
/// 调 [`verify_manifest_with_key`]（校验语义与本函数一致）。
pub(crate) fn verify_manifest_value(manifest: &serde_json::Value) -> Result<(), UpdaterError> {
    match embedded_verifying_key() {
        Some(key) => verify_manifest_with_key(manifest, Some(&key)),
        None => Ok(()),
    }
}

/// 用给定公钥校验清单签名（核心纯函数，便于单测覆盖各组合）
///
/// `pubkey` 为 `None` 表示未启用签名校验（未嵌公钥构建），直接放行；
/// 清单缺失 `signature` 字段 / base64 解码失败 / 签名与规范化字节不匹配 → 拒绝。
pub(crate) fn verify_manifest_with_key(
    manifest: &serde_json::Value,
    pubkey: Option<&VerifyingKey>,
) -> Result<(), UpdaterError> {
    let Some(key) = pubkey else {
        return Ok(());
    };
    let sig_b64 = manifest
        .get("signature")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if sig_b64.is_empty() {
        return Err(UpdaterError::ManifestSignatureInvalid);
    }
    let sig_bytes = base64::engine::general_purpose::STANDARD
        .decode(sig_b64)
        .map_err(|_| UpdaterError::ManifestSignatureInvalid)?;
    if sig_bytes.len() != 64 {
        return Err(UpdaterError::ManifestSignatureInvalid);
    }
    let mut arr = [0u8; 64];
    arr.copy_from_slice(&sig_bytes);
    let sig = Signature::from_bytes(&arr);
    let message = manifest_signing_bytes(manifest);
    key.verify(&message, &sig)
        .map_err(|_| UpdaterError::ManifestSignatureInvalid)
}

/// 生成 ed25519 签名密钥对（发布方一次性操作）
///
/// 返回 `(私钥 base64, 公钥 base64)`：私钥即 GitHub Secret `MANIFEST_SIGNING_KEY`
/// 的值（绝不入仓），公钥提交入仓 `resources/manifest-pubkey`。种子经系统熵源
/// 生成，编码后立即清零。
pub fn generate_signing_keypair() -> (String, String) {
    use rand::RngCore;
    use zeroize::Zeroize;

    let mut seed = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut seed);
    let secret_b64 = base64::engine::general_purpose::STANDARD.encode(seed);
    let key = SigningKey::from_bytes(&seed);
    seed.zeroize();
    (
        secret_b64,
        base64::engine::general_purpose::STANDARD.encode(key.verifying_key().as_bytes()),
    )
}

/// 为清单 JSON 注入 `signature` 字段（`--sign-manifest` 的实现）
///
/// 输入为清单 JSON 文本（容忍键序与空白差异）；剥离既有 `signature` 字段后对
/// 规范化字节签名，返回注入了新 `signature` 的紧凑 JSON 文本（末尾带换行）。
/// 返回 `Err` 时载荷为中文描述，CLI 场景可直接透出。
pub fn sign_manifest_document(document: &str, signing_key_b64: &str) -> Result<String, String> {
    let key = decode_signing_key(signing_key_b64)?;
    let value: serde_json::Value =
        serde_json::from_str(document).map_err(|e| format!("清单不是合法 JSON: {e}"))?;
    let message = manifest_signing_bytes(&value);
    let sig = key.sign(&message);
    let sig_b64 = base64::engine::general_purpose::STANDARD.encode(sig.to_bytes());
    let mut value = value;
    match &mut value {
        serde_json::Value::Object(map) => {
            map.insert("signature".to_owned(), serde_json::Value::String(sig_b64));
        }
        _ => return Err("清单顶层必须是 JSON 对象".to_owned()),
    }
    let mut out = serde_json::to_string(&value).map_err(|e| format!("签名结果序列化失败: {e}"))?;
    out.push('\n');
    Ok(out)
}

/// 用指定公钥（base64）校验清单文档（`--verify-manifest` 的实现 / 测试辅助）
///
/// 供发布流水线做一致性复核：私钥（Secret）与提交入仓的公钥不匹配时失败，
/// 防止带病发布。返回 `Err` 时载荷为中文描述。
pub fn verify_manifest_document(document: &str, pubkey_b64: &str) -> Result<(), String> {
    let key = decode_pubkey(pubkey_b64)?;
    let value: serde_json::Value =
        serde_json::from_str(document).map_err(|e| format!("清单不是合法 JSON: {e}"))?;
    verify_manifest_with_key(&value, Some(&key)).map_err(|e| e.to_string())
}

/// 解析 base64 编码的签名私钥（32 字节种子），解码后立即清零中间缓冲
fn decode_signing_key(signing_key_b64: &str) -> Result<SigningKey, String> {
    use zeroize::Zeroize;

    let mut bytes = base64::engine::general_purpose::STANDARD
        .decode(signing_key_b64.trim())
        .map_err(|e| format!("私钥 base64 解码失败: {e}"))?;
    if bytes.len() != 32 {
        let len = bytes.len();
        bytes.zeroize();
        return Err(format!("私钥长度应为 32 字节，实际 {len} 字节"));
    }
    let mut seed = [0u8; 32];
    seed.copy_from_slice(&bytes);
    bytes.zeroize();
    Ok(SigningKey::from_bytes(&seed))
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::engine::general_purpose::STANDARD;

    /// 生成一对测试密钥（私钥/公钥 base64）
    fn test_keypair() -> (SigningKey, VerifyingKey) {
        let (secret_b64, pubkey_b64) = generate_signing_keypair();
        let key = decode_signing_key(&secret_b64).expect("测试私钥应可解析");
        let pubkey = decode_pubkey(&pubkey_b64).expect("测试公钥应可解析");
        (key, pubkey)
    }

    /// 签名往返：签 → 验成功，且换一种键序/空白序列化仍通过（规范化收益）
    #[test]
    fn sign_and_verify_roundtrip_across_serializations() {
        let (key, pubkey) = test_keypair();
        let document = r#"{
            "version": "5.3.0",
            "platforms": { "windows-x64": { "url": "https://example.com/pkg.zip", "sha256": "a0", "size": 1 } },
            "changelog": "修复若干问题（含引号\"与\\反斜杠）",
            "release_date": "2026-11-13"
        }"#;
        let signed =
            sign_manifest_document(document, &STANDARD.encode(key.to_bytes())).expect("签名应成功");
        assert!(signed.contains("\"signature\""));

        // 原样校验通过
        verify_manifest_document(&signed, &STANDARD.encode(pubkey.as_bytes()))
            .expect("正确签名应通过校验");

        // 键序重排 + 空白变化（镜像转发常见）后仍通过：内容相同即验证一致
        let value: serde_json::Value = serde_json::from_str(&signed).unwrap();
        let reordered = serde_json::json!({
            "platforms": value["platforms"],
            "signature": value["signature"],
            "release_date": value["release_date"],
            "version": value["version"],
            "changelog": value["changelog"],
        });
        verify_manifest_with_key(&reordered, Some(&pubkey)).expect("重排序列化应通过校验");
    }

    /// 篡改清单内容（改版本号）→ 拒绝
    #[test]
    fn tampered_manifest_is_rejected() {
        let (key, pubkey) = test_keypair();
        let document = r#"{"version":"5.3.0","platforms":{}}"#;
        let signed = sign_manifest_document(document, &STANDARD.encode(key.to_bytes())).unwrap();
        let mut value: serde_json::Value = serde_json::from_str(&signed).unwrap();
        value["version"] = serde_json::Value::String("9.9.9".into());
        assert!(matches!(
            verify_manifest_with_key(&value, Some(&pubkey)),
            Err(UpdaterError::ManifestSignatureInvalid)
        ));
    }

    /// 错误公钥 → 拒绝
    #[test]
    fn wrong_pubkey_is_rejected() {
        let (key, _) = test_keypair();
        let (_, other_pubkey) = test_keypair();
        let signed = sign_manifest_document(
            r#"{"version":"1.0.0","platforms":{}}"#,
            &STANDARD.encode(key.to_bytes()),
        )
        .unwrap();
        let value: serde_json::Value = serde_json::from_str(&signed).unwrap();
        assert!(matches!(
            verify_manifest_with_key(&value, Some(&other_pubkey)),
            Err(UpdaterError::ManifestSignatureInvalid)
        ));
    }

    /// 无签名 + 有公钥 → 拒绝；无签名 + 无公钥 → 放行（兼容语义）
    #[test]
    fn missing_signature_gated_by_pubkey_presence() {
        let (_, pubkey) = test_keypair();
        let value: serde_json::Value =
            serde_json::from_str(r#"{"version":"1.0.0","platforms":{}}"#).unwrap();
        assert!(matches!(
            verify_manifest_with_key(&value, Some(&pubkey)),
            Err(UpdaterError::ManifestSignatureInvalid)
        ));
        verify_manifest_with_key(&value, None).expect("未嵌公钥构建应放行未签名清单");
    }

    /// 畸形签名字段（非 base64 / 长度不符 / 非字符串）→ 拒绝
    #[test]
    fn malformed_signature_is_rejected() {
        let (_, pubkey) = test_keypair();
        let short = STANDARD.encode([0u8; 8]);
        for sig in ["not-base64!!", short.as_str(), ""] {
            let value = serde_json::json!({ "version": "1.0.0", "signature": sig });
            assert!(
                matches!(
                    verify_manifest_with_key(&value, Some(&pubkey)),
                    Err(UpdaterError::ManifestSignatureInvalid)
                ),
                "畸形签名 {sig:?} 应被拒绝"
            );
        }
        // signature 缺失字段
        let value = serde_json::json!({ "version": "1.0.0" });
        assert!(matches!(
            verify_manifest_with_key(&value, Some(&pubkey)),
            Err(UpdaterError::ManifestSignatureInvalid)
        ));
    }

    /// 规范化稳定性：键序/空白不同、内容相同的文档产出同一规范化字节
    #[test]
    fn canonical_bytes_are_order_and_whitespace_independent() {
        let a: serde_json::Value =
            serde_json::from_str(r#"{"b":1,"a":{"y":[1,2],"x":null},"c":"s"}"#).unwrap();
        let b: serde_json::Value = serde_json::from_str(
            "{\n  \"c\" : \"s\",\n  \"a\" : { \"x\" : null, \"y\" : [ 1, 2 ] },\n  \"b\" : 1\n}",
        )
        .unwrap();
        let (ba, bb) = (manifest_signing_bytes(&a), manifest_signing_bytes(&b));
        assert_eq!(ba, bb);
        // 键按字典序排列、无空白
        assert_eq!(
            String::from_utf8(ba).unwrap(),
            r#"{"a":{"x":null,"y":[1,2]},"b":1,"c":"s"}"#
        );
    }

    /// 待签名字节剥离 signature 字段：带旧签名的文档重签后自洽
    #[test]
    fn signing_bytes_strip_signature_field() {
        let (key, pubkey) = test_keypair();
        let document = r#"{"version":"2.0.0","platforms":{},"signature":"stale-old-signature"}"#;
        // 对携带旧签名字段的文档重签：剥离后按剩余内容签名，新签名应替换旧值并通过
        let signed = sign_manifest_document(document, &STANDARD.encode(key.to_bytes())).unwrap();
        verify_manifest_document(&signed, &STANDARD.encode(pubkey.as_bytes()))
            .expect("重签后的文档应通过校验");
        // 且被签名字节确实不含 signature 字段（与剥离行为一致）
        let value: serde_json::Value = serde_json::from_str(&signed).unwrap();
        let stripped = manifest_signing_bytes(&value);
        assert!(!String::from_utf8(stripped).unwrap().contains("signature"));
    }

    /// 密钥生成：私钥/公钥配对且与再生成结果不同（熵源随机性）
    #[test]
    fn generated_keypair_is_self_consistent() {
        let (secret, pubkey) = generate_signing_keypair();
        assert_eq!(secret.len(), 44, "base64 编码的 32 字节种子应为 44 字符");
        assert_eq!(pubkey.len(), 44);
        let signed =
            sign_manifest_document(r#"{"version":"1.0.0","platforms":{}}"#, &secret).unwrap();
        verify_manifest_document(&signed, &pubkey).expect("自生成密钥对应自洽");

        let (secret2, pubkey2) = generate_signing_keypair();
        assert_ne!(secret, secret2, "不同次生成的密钥对应不同");
        assert_ne!(pubkey, pubkey2);
    }

    /// 非法私钥（长度不符 / 非 base64）与非法公钥给出中文错误
    #[test]
    fn invalid_keys_fail_with_readable_errors() {
        let err = sign_manifest_document("{}", &STANDARD.encode([0u8; 8])).unwrap_err();
        assert!(err.contains("32 字节"), "{err}");
        let err = sign_manifest_document("{}", "not-base64!!").unwrap_err();
        assert!(err.contains("base64"), "{err}");
        let err = verify_manifest_document("{}", &STANDARD.encode([0u8; 64])).unwrap_err();
        assert!(err.contains("32 字节"), "{err}");
    }
}
