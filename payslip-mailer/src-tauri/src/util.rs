//! 小工具：十六进制编解码。
//!
//! **不引 `hex` crate**：它在 Cargo.lock 里只挂在 proc-macro 侧，
//! 运行时依赖里加它等于凭空多一条供应链（ADR-0010 第 7 条「少一个依赖就是少一条供应链」）。
//! 十六进制只有几十行，自己写没有维护负担。

/// 字节 → 小写十六进制。
///
/// 喂给 SQLCipher 的 `PRAGMA key = "x'…'"`（原始密钥），
/// 也用来把盐写进备份清单、把 SHA-256 写成可读串。
pub fn to_hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(DIGITS[(byte >> 4) as usize] as char);
        out.push(DIGITS[(byte & 0x0f) as usize] as char);
    }
    out
}

/// 十六进制 → 字节。奇数长度或非法字符一律报错，不猜、不补位。
///
/// 读备份清单里的盐要用它：**盐决定能不能解开备份**，
/// 解析错了必须是明确的失败，不能悄悄变成另一把钥匙。
pub fn from_hex(text: &str) -> Option<Vec<u8>> {
    let bytes = text.as_bytes();
    if !bytes.len().is_multiple_of(2) {
        return None;
    }
    let mut out = Vec::with_capacity(bytes.len() / 2);
    for pair in bytes.chunks(2) {
        let hi = (pair[0] as char).to_digit(16)?;
        let lo = (pair[1] as char).to_digit(16)?;
        out.push((hi * 16 + lo) as u8);
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_round_trips() {
        let bytes = [0u8, 1, 15, 16, 127, 128, 255];
        let hex = to_hex(&bytes);
        assert_eq!(hex, "00010f107f80ff");
        assert_eq!(from_hex(&hex).unwrap(), bytes.to_vec());
    }

    #[test]
    fn bad_hex_is_rejected_rather_than_guessed() {
        assert!(from_hex("abc").is_none(), "奇数长度必须拒绝");
        assert!(from_hex("zz").is_none(), "非法字符必须拒绝");
        assert!(from_hex("").unwrap().is_empty());
    }
}
