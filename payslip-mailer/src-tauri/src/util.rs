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

/// 全角金额字符 → 半角（ch.13 必测边界用例「中文全角数字」）。
///
/// 中文输入法直接打出的数字是「１８５００」这类全角形式，`parse::<f64>` 不认识它们，
/// 但 HR 的 Excel 与逐格编辑里都会出现 —— 它们不该被当成「填错了」。
///
/// 全角 ASCII 区（U+FF01–U+FF5E）与半角正好差 0xFEE0。这里**只转金额里可能出现的
/// 五类**（数字 / 小数点 / 加号 / 减号 / 百分号），其余字符原样留下 ——
/// 让它们照旧走到各自的报错，而不是被悄悄转成别的东西（比如把「（」转成「(」）。
pub fn normalize_fullwidth(text: &str) -> String {
    text.chars()
        .map(|c| {
            if matches!(c, '０'..='９' | '．' | '＋' | '－' | '％') {
                char::from_u32(c as u32 - 0xFEE0).unwrap_or(c)
            } else {
                c
            }
        })
        .collect()
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

    #[test]
    fn fullwidth_amount_chars_become_halfwidth_and_nothing_else() {
        assert_eq!(normalize_fullwidth("１８５００"), "18500");
        assert_eq!(normalize_fullwidth("１８，０００．５０"), "18，000.50");
        assert_eq!(normalize_fullwidth("－２１００"), "-2100");
        assert_eq!(normalize_fullwidth("＋１８０００"), "+18000");
        assert_eq!(normalize_fullwidth("１２．５％"), "12.5%");
        // 不属于金额的字符原样留下，不悄悄转
        assert_eq!(normalize_fullwidth("ＡＢ（１）"), "ＡＢ（1）");
    }
}
