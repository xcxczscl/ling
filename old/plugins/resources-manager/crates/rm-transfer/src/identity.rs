//! 设备身份：Ed25519 密钥 + 自签证书 + 指纹（`docs/03` §5.2、`docs/01` §7.6）。
//!
//! 种子存 `data_dir/identity.key`（32 字节，0600）；指纹 = 公钥 SHA-256 前 16 字节
//! RFC 4648 Base32，前缀 `sha256:`。

use std::path::Path;

use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use sha2::{Digest, Sha256};

use crate::error::TransferError;

/// 指纹前缀。
pub const FP_PREFIX: &str = "sha256:";

/// 本机设备身份：可签名、可自报指纹。
pub struct Identity {
    signing: SigningKey,
    fingerprint: String,
}

impl Identity {
    /// 从 32 字节种子构造。
    pub fn from_seed(seed: [u8; 32]) -> Self {
        let signing = SigningKey::from_bytes(&seed);
        Self {
            fingerprint: fingerprint_of(signing.verifying_key().as_bytes()),
            signing,
        }
    }

    /// 加载或首启生成：`identity.key` 不存在则生成并以 0600 写入。
    pub fn load_or_generate(dir: &Path) -> Result<Self, TransferError> {
        let path = dir.join("identity.key");
        if path.exists() {
            let bytes = std::fs::read(&path)
                .map_err(|e| TransferError::io(format!("读取身份密钥 {path:?}：{e}")))?;
            if bytes.len() != 32 {
                return Err(TransferError::proto(format!(
                    "身份密钥 {path:?} 长度 {} ≠ 32 字节",
                    bytes.len()
                )));
            }
            let mut seed = [0u8; 32];
            seed.copy_from_slice(&bytes);
            return Ok(Self::from_seed(seed));
        }
        let mut seed = [0u8; 32];
        getrandom::getrandom(&mut seed)
            .map_err(|e| TransferError::io(format!("生成身份密钥随机数失败：{e}")))?;
        std::fs::create_dir_all(dir)
            .map_err(|e| TransferError::io(format!("创建数据目录 {dir:?}：{e}")))?;
        std::fs::write(&path, seed)
            .map_err(|e| TransferError::io(format!("写入身份密钥 {path:?}：{e}")))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
        }
        Ok(Self::from_seed(seed))
    }

    /// 本机指纹（`sha256:` + 26 字符 Base32）。
    pub fn fingerprint(&self) -> &str {
        &self.fingerprint
    }

    /// 本机公钥（32 字节）。
    pub fn public_key(&self) -> [u8; 32] {
        self.signing.verifying_key().to_bytes()
    }

    /// 签名任意消息。
    pub fn sign(&self, msg: &[u8]) -> [u8; 64] {
        self.signing.sign(msg).to_bytes()
    }
}

/// 由公钥算指纹。
pub fn fingerprint_of(pubkey: &[u8]) -> String {
    let digest = Sha256::digest(pubkey);
    format!("{}{}", FP_PREFIX, crate::base32::encode(&digest[..16]))
}

/// 验签：`msg` 的签名是否由 `pubkey` 作出。
pub fn verify(pubkey: &[u8], msg: &[u8], sig: &[u8]) -> bool {
    let Ok(vk) = VerifyingKey::from_bytes(pubkey.try_into().unwrap_or(&[0u8; 32])) else {
        return false;
    };
    let Ok(signature) = Signature::from_slice(sig) else {
        return false;
    };
    vk.verify(msg, &signature).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprint_format() {
        let id = Identity::from_seed([7u8; 32]);
        let fp = id.fingerprint();
        assert!(fp.starts_with(FP_PREFIX));
        assert_eq!(fp.len(), FP_PREFIX.len() + 26);
        // 稳定：同种子同指纹。
        assert_eq!(Identity::from_seed([7u8; 32]).fingerprint(), fp);
        assert_ne!(Identity::from_seed([8u8; 32]).fingerprint(), fp);
    }

    #[test]
    fn sign_verify_roundtrip() {
        let a = Identity::from_seed([1u8; 32]);
        let b = Identity::from_seed([2u8; 32]);
        let msg = b"rm-blob|abc|0|4194304|1760500000";
        let sig = a.sign(msg);
        assert!(verify(&a.public_key(), msg, &sig));
        assert!(!verify(&b.public_key(), msg, &sig));
        assert!(!verify(&a.public_key(), b"other", &sig));
    }

    #[test]
    fn load_or_generate_persists() {
        let dir = tempfile::tempdir().expect("tempdir");
        let id1 = Identity::load_or_generate(dir.path()).expect("gen");
        let id2 = Identity::load_or_generate(dir.path()).expect("load");
        assert_eq!(id1.fingerprint(), id2.fingerprint());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(dir.path().join("identity.key"))
                .expect("meta")
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600, "identity.key 应为 0600");
        }
    }
}
