//! RFC 4648 Base32（大写、无填充）——身份指纹编码用（`docs/03` §5.2）。
//!
//! 16 字节 → 128 bit → 26 个字符（128/5 向上取整）。

const ALPHABET: &[u8; 32] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";

/// 编码：任意字节 → Base32 大写无填充。
pub fn encode(data: &[u8]) -> String {
    let mut out = String::with_capacity((data.len() * 8 + 4) / 5);
    let mut acc: u64 = 0;
    let mut bits: u32 = 0;
    for &b in data {
        acc = (acc << 8) | u64::from(b);
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            out.push(ALPHABET[((acc >> bits) & 31) as usize] as char);
        }
    }
    if bits > 0 {
        out.push(ALPHABET[((acc << (5 - bits)) & 31) as usize] as char);
    }
    out
}

/// 解码：Base32 大写无填充 → 字节；非法字符返回 None。
pub fn decode(s: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity((s.len() * 5) / 8);
    let mut acc: u64 = 0;
    let mut bits: u32 = 0;
    for c in s.bytes() {
        let v = ALPHABET.iter().position(|&a| a == c)? as u64;
        acc = (acc << 5) | v;
        bits += 5;
        if bits >= 8 {
            bits -= 8;
            out.push(((acc >> bits) & 0xff) as u8);
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_empty_and_short() {
        assert_eq!(encode(b""), "");
        assert_eq!(encode(b"f"), "MY");
        assert_eq!(decode("MY").as_deref(), Some(b"f".as_ref()));
    }

    #[test]
    fn roundtrip_16_bytes_is_26_chars() {
        let data: Vec<u8> = (0..16u8).collect();
        let enc = encode(&data);
        assert_eq!(enc.len(), 26, "16 字节应编码为 26 字符");
        assert_eq!(decode(&enc), Some(data));
    }

    #[test]
    fn decode_rejects_invalid() {
        assert_eq!(decode("NOT VALID!"), None);
        assert_eq!(decode("01"), None); // 0/1 不在字母表
    }
}
