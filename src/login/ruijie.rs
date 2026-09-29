//! 上海大学锐捷 ePortal 的定制 RSA 密码变换，仅供对应门户模板使用。

use num_bigint::BigUint;

/// 参考实现中的固定公钥模数；其他学校的锐捷门户未必使用它。
const SHU_MODULUS: &str = "94dd2a8675fb779e6b9f7103698634cd400f27a154afa67af6166a43fc26417222a79506d34cacc7641946abda1785b7acf9910ad6a0978c91ec84d40b71d2891379af19ffb333e7517e390bd26ac312fe940c340466b4a5d4af1d65c3b5944078f96a1a51a5a53e4bc302818b7c9f63c4a1b07bd7d874cef1c3d4b2f5eb7871";

/// 按上海大学锐捷页面的算法加密 `password>mac`，不替其他部署猜公钥。
pub(crate) fn shu_password(password: &str, mac: &str) -> Result<String, String> {
    if password.len() + mac.len() > 4096 {
        return Err("锐捷密码输入超过大小限制".into());
    }
    let modulus = BigUint::parse_bytes(SHU_MODULUS.as_bytes(), 16).ok_or("锐捷公钥配置无效")?;
    let exponent = BigUint::from(65537u32);
    let chunk_size = 2 * (modulus.bits() as usize).div_ceil(8).saturating_sub(1);
    if chunk_size == 0 {
        return Err("锐捷公钥长度无效".into());
    }
    let plaintext = format!("{password}>{mac}");
    let mut reversed = plaintext.chars().rev().collect::<String>().into_bytes();
    reversed.resize(reversed.len().div_ceil(chunk_size) * chunk_size, 0);
    Ok(reversed
        .chunks(chunk_size)
        .map(|chunk| {
            BigUint::from_bytes_le(chunk)
                .modpow(&exponent, &modulus)
                .to_str_radix(16)
        })
        .collect::<Vec<_>>()
        .join(" "))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reference_vector_matches_source_project() {
        // 来源：BeiningWu/shu-net-keeper/src/encrypt.rs 的公开测试向量。
        assert_eq!(
            shu_password("123", "5ae915bf808f82732e98e01f704f00cd").unwrap(),
            "91a0e02175f6a0b22ad23dac0d7f599806bc091f9fee1bfdada0d24d011dcdaed418296b7c0ec560f988d92a7bb25dbf7ff51752d9bc6482a8180e56f7b772079ab59844abaae91e6d1c4660dc872717f9218f89acc9b70bb32891f28bf9d8f173d81b0e36c828deac919783e4e909ad1c22f953947b4a7ed7c90ac18fd95aa2"
        );
    }
}
