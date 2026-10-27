//! 线上协议原语（`docs/03` §5.2 冻结细节的实现）。

use sha2::{Digest, Sha256};

/// 默认 LAN 监听端口。
pub const DEFAULT_PORT: u16 = 7912;
/// 传输块大小：4 MB。
pub const BLOCK_SIZE: usize = 4 * 1024 * 1024;
/// 并行连接数。
pub const PARALLEL: usize = 8;
/// 签名时间戳允许偏差（秒）。
pub const TS_SKEW_SECS: i64 = 300;
/// 配对码有效期（秒）。
pub const PAIRING_TTL_SECS: u64 = 600;
/// 协议版本。
pub const PROTO: u32 = 1;

/// `/blob` 块帧头长度：u32 块长 + 32 字节 BLAKE3。
pub const BLOCK_HEADER_LEN: usize = 4 + 32;

/// 拼 `/blob` 签名串。
pub fn blob_sign_payload(hash: &str, offset: u64, len: u64, ts: i64) -> String {
    format!("rm-blob|{hash}|{offset}|{len}|{ts}")
}

/// 拼 `/op/*` 签名串（body 先 SHA-256）。
pub fn op_sign_payload(method: &str, body: &[u8], ts: i64) -> String {
    let digest = Sha256::digest(body);
    let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
    format!("rm-op|{method}|{hex}|{ts}")
}

/// 拼 `/pair` 发起端签名串。
pub fn pair_client_payload(code: &str, device_id: &str, pubkey_hex: &str, ts: i64) -> String {
    format!("rm-pair|{code}|{device_id}|{pubkey_hex}|{ts}")
}

/// 拼 `/pair` 响应端签名串。
pub fn pair_server_payload(device_id: &str, pubkey_hex: &str, ts: i64) -> String {
    format!("rm-pair|{device_id}|{pubkey_hex}|{ts}")
}

/// 十六进制编码（公钥/指纹比对用）。
pub fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// 从十六进制解码；非法返回 None。
pub fn from_hex(s: &str) -> Option<Vec<u8>> {
    if s.len() % 2 != 0 {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).ok())
        .collect()
}

/// 当前 Unix 秒。
pub fn now_ts() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// 时间戳偏差检查。
pub fn ts_ok(ts: i64) -> bool {
    (now_ts() - ts).abs() <= TS_SKEW_SECS
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_roundtrip() {
        let b = [0u8, 1, 0xab, 0xff];
        assert_eq!(from_hex(&to_hex(&b)), Some(b.to_vec()));
        assert_eq!(from_hex("abc"), None);
        assert_eq!(from_hex("zz"), None);
    }

    #[test]
    fn payload_shapes() {
        assert_eq!(
            blob_sign_payload("h", 0, 4, 5),
            "rm-blob|h|0|4|5"
        );
        assert_eq!(pair_client_payload("12345678", "dv_a", "ab", 1), "rm-pair|12345678|dv_a|ab|1");
        assert!(ts_ok(now_ts()));
        assert!(!ts_ok(now_ts() + 10_000));
    }
}
