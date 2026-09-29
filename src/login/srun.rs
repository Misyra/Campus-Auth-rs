//! 深澜 SRUN 登录所需的纯计算算法；请求流程与门户参数由 HTTP 任务定义。

use base64::Engine as _;

/// 深澜使用的非标准 Base64 字母表。
const SRUN_ALPHABET: &str = "LVoJPiCN2R8G90yg+hmFHuacZ1OWMnrsSTXkYpUq/3dlbfKwv6xztjI7DeBE45QA";

/// 将登录信息 JSON 按 challenge 做 xEncode，再按深澜字母表编码。
pub(crate) fn encode_info(info_json: &str, challenge: &str) -> Result<String, String> {
    if info_json.len() > 64 * 1024 || challenge.len() > 8 * 1024 {
        return Err("深澜计算输入过大".into());
    }
    if challenge.is_empty() {
        return Err("深澜 challenge 不能为空".into());
    }
    let alphabet = base64::alphabet::Alphabet::new(SRUN_ALPHABET)
        .map_err(|_| "深澜 Base64 字母表无效".to_string())?;
    let engine = base64::engine::general_purpose::GeneralPurpose::new(
        &alphabet,
        base64::engine::general_purpose::GeneralPurposeConfig::new(),
    );
    Ok(format!(
        "{{SRBX1}}{}",
        engine.encode(xencode(info_json.as_bytes(), challenge.as_bytes()))
    ))
}

/// 把字节按小端序切成 32 位字；信息末尾附带原始字节长度。
fn words(input: &[u8], append_len: bool) -> Vec<u32> {
    let mut result = input
        .chunks(4)
        .map(|chunk| {
            let mut bytes = [0; 4];
            bytes[..chunk.len()].copy_from_slice(chunk);
            u32::from_le_bytes(bytes)
        })
        .collect::<Vec<_>>();
    if append_len {
        result.push(input.len() as u32);
    }
    result
}

/// XXTEA 派生的深澜 xEncode；所有加法与移位都按 32 位环绕计算。
fn xencode(info: &[u8], challenge: &[u8]) -> Vec<u8> {
    if info.is_empty() {
        return Vec::new();
    }
    let mut data = words(info, true);
    let mut key = words(challenge, false);
    key.resize(key.len().max(4), 0);
    let count = 6 + 52 / data.len();
    let mut sum = 0_u32;
    let mut previous = data[data.len() - 1];
    for _ in 0..count {
        sum = sum.wrapping_add(0x9e37_79b9);
        let selector = (sum >> 2 & 3) as usize;
        for index in 0..data.len() {
            let next = data[(index + 1) % data.len()];
            let mixed = ((previous >> 5) ^ (next << 2))
                .wrapping_add((next >> 3) ^ (previous << 4) ^ (sum ^ next));
            let mixed = mixed.wrapping_add(key[(index & 3) ^ selector] ^ previous);
            data[index] = data[index].wrapping_add(mixed);
            previous = data[index];
        }
    }
    data.into_iter().flat_map(u32::to_le_bytes).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn challenge_and_size_are_checked() {
        assert!(encode_info("{}", "").is_err());
        assert!(encode_info(&"x".repeat(65 * 1024), "token").is_err());
    }

    #[test]
    fn encode_matches_reference_implementation() {
        let info =
            r#"{"username":"u","password":"p","ip":"1.2.3.4","acid":1,"enc_ver":"srun_bx1"}"#;
        assert_eq!(
            encode_info(info, "abc").unwrap(),
            "{SRBX1}RszD8J8tEj+rl30RUjO0eE4LKu+T5BnmQhiPndo5wofC0U/EGeOyVeofh6PYn0dHpXPVmdVb1iUdjlHJYKfXg6dQzFC3zYYVrB4LpqDZ6HZ="
        );
    }
}
