//! 分层哈希（`docs/01` §7.2）。

use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

/// 第二级窗口大小：首尾各 64 KB。
pub const TIER2_WINDOW: u64 = 64 * 1024;

/// 第二级快速指纹（首尾各 64 KB 的 BLAKE3）。读不满时取实际可用字节。
pub fn quick_fingerprint(path: &Path) -> std::io::Result<String> {
    let mut f = std::fs::File::open(path)?;
    let len = f.metadata()?.len();
    let mut buf = Vec::with_capacity((TIER2_WINDOW * 2) as usize);
    let head = TIER2_WINDOW.min(len);
    buf.resize(head as usize, 0);
    f.read_exact(&mut buf)?;
    if len > TIER2_WINDOW {
        let tail = TIER2_WINDOW.min(len - TIER2_WINDOW);
        f.seek(SeekFrom::End(-(tail as i64)))?;
        let start = buf.len();
        buf.resize(start + tail as usize, 0);
        f.read_exact(&mut buf[start..])?;
    }
    Ok(blake3::hash(&buf).to_hex().to_string())
}

/// 第三级全文 BLAKE3（64 位十六进制）。
pub fn full_hash(path: &Path) -> std::io::Result<String> {
    let mut f = std::fs::File::open(path)?;
    let mut hasher = blake3::Hasher::new();
    let mut buf = vec![0u8; 1024 * 1024];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher.finalize().to_hex().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn tier2_and_full_hash_stable() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.bin");
        let mut f = std::fs::File::create(&p).unwrap();
        f.write_all(&[7u8; 200_000]).unwrap();
        let q1 = quick_fingerprint(&p).unwrap();
        let q2 = quick_fingerprint(&p).unwrap();
        assert_eq!(q1, q2);
        let h = full_hash(&p).unwrap();
        assert_eq!(h.len(), 64);
        // 改一个字节 → 全文哈希必变
        let mut f = std::fs::File::create(&p).unwrap();
        f.write_all(&[8u8; 200_000]).unwrap();
        assert_ne!(h, full_hash(&p).unwrap());
    }

    #[test]
    fn tier2_short_file() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("s.bin");
        std::fs::write(&p, b"hello").unwrap();
        assert_eq!(quick_fingerprint(&p).unwrap().len(), 64);
    }
}
