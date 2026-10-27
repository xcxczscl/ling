//! 占位加密（`docs/03` §5.3.1）。
//!
//! **明确标注为占位，禁止当作真实保密手段。** 密钥 =
//! `BLAKE3(口令, 个人化串="rm-backup-v1")` 取前 32 字节；流式 XOR，
//! 密钥流 = `BLAKE3(key ‖ 段内偏移 LE u64)` 展开。口令只存用户侧。

use rm_proto::BizError;

use crate::codes;

pub const PERSONA: &[u8] = b"rm-backup-v1";

/// 从口令派生 32 字节密钥。
pub fn derive_key(password: &[u8]) -> [u8; 32] {
    let mut h = blake3::Hasher::new();
    h.update(password);
    h.update(PERSONA);
    let out = h.finalize();
    let mut key = [0u8; 32];
    key.copy_from_slice(out.as_bytes());
    key
}

fn keystream_block(key: &[u8; 32], offset: u64) -> [u8; 32] {
    let mut h = blake3::Hasher::new();
    h.update(key);
    h.update(&offset.to_le_bytes());
    let out = h.finalize();
    let mut b = [0u8; 32];
    b.copy_from_slice(out.as_bytes());
    b
}

/// 对 `data` 做/撤销占位加密（对称 XOR）。`offset` = 该块在段内的字节偏移。
pub fn xor_crypt(key: &[u8; 32], data: &mut [u8], offset: u64) {
    let mut off = offset;
    for chunk in data.chunks_mut(32) {
        let ks = keystream_block(key, off);
        for (i, b) in chunk.iter_mut().enumerate() {
            *b ^= ks[i];
        }
        off += chunk.len() as u64;
    }
}

/// 口令缺失时的 F606；正常时返回口令字节。
pub fn require_password<'a>(password: Option<&'a str>) -> Result<&'a str, BizError> {
    match password {
        Some(p) if !p.is_empty() => Ok(p),
        _ => Err(BizError::new(
            codes::F606,
            "占位加密口令缺失或不匹配（manifest.enc=placeholder）",
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_xor() {
        let key = derive_key(b"hunter2");
        let mut data = vec![7u8; 1000];
        let orig = data.clone();
        xor_crypt(&key, &mut data, 128 * 1024 * 1024 - 10);
        assert_ne!(data, orig);
        xor_crypt(&key, &mut data, 128 * 1024 * 1024 - 10);
        assert_eq!(data, orig);
    }

    #[test]
    fn wrong_password_gives_garbage() {
        let k1 = derive_key(b"a");
        let k2 = derive_key(b"b");
        let mut data = b"secret payload".to_vec();
        let orig = data.clone();
        xor_crypt(&k1, &mut data, 0);
        // 用错密钥解不回原文。
        xor_crypt(&k2, &mut data, 0);
        assert_ne!(data, orig);
    }
}
