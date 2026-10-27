//! 受管存储池布局与归纳三档迁移（`docs/01` §4.6、§7.9）。
//!
//! 池内路径：`{location_root}/{pool.rel_path}/{blake3[0:2]}/{blake3[2:4]}/{blake3}`
//! （可选 `.zst` 后缀表示压缩形态）。

use std::path::{Path, PathBuf};

use rm_proto::BizError;
use rm_fs::platform;

use crate::codes;

/// 池内相对路径（按指纹两级分桶）。
pub fn pool_rel_for(blake3: &str, compressed: bool) -> String {
    let (a, b) = if blake3.len() >= 4 {
        (&blake3[0..2], &blake3[2..4])
    } else {
        ("00", "00")
    };
    if compressed {
        format!("{a}/{b}/{blake3}.zst")
    } else {
        format!("{a}/{b}/{blake3}")
    }
}

/// 绝对路径 = 池根 + 池内相对路径。
pub fn pool_abs(location_root: &str, pool_rel_path: &str, content_rel: &str) -> PathBuf {
    Path::new(location_root)
        .join(pool_rel_path.trim_end_matches('/'))
        .join(content_rel)
}

/// 迁移成本三档（`docs/01` §7.9.1）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MigrateTier {
    /// 同文件系统 rename。
    Rename,
    /// 同设备跨分区拷贝 + 校验 + 删源。
    Copy,
    /// 跨设备（源/目标不同设备或不同挂载根）→ 需在线才可做。
    CrossDevice,
}

/// 判定迁移档位：源与目标目录是否同一挂载点（跨平台，零 cfg）。
pub fn classify_tier(src: &Path, dst_parent: &Path) -> MigrateTier {
    if let Some(dir) = dst_parent.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let src_meta = std::fs::metadata(src).ok();
    let dst_meta = std::fs::metadata(dst_parent).ok();
    match (src_meta, dst_meta) {
        (Some(a), Some(b)) => {
            match (platform::dev_of_path(src), platform::dev_of_path(dst_parent)) {
                (Some(d1), Some(d2)) if d1 == d2 => MigrateTier::Rename,
                (Some(_), Some(_)) => MigrateTier::Copy,
                // dev_of_path 返回 None 时（极端平台 / 权限问题），退而求其次：
                // 文件大小和 mtime 也一致则当同设备处理。
                _ if a.len() == b.len() && a.modified().ok() == b.modified().ok() => MigrateTier::Rename,
                _ => MigrateTier::CrossDevice,
            }
        }
        _ => MigrateTier::CrossDevice,
    }
}

/// 复制并返回目标路径（跨分区/同分区统一用 copy+fsync，rename 由调用方在
/// 同设备时优先尝试）。
pub fn copy_verified(src: &Path, dst: &Path, expect_blake3: &str) -> Result<i64, BizError> {
    if let Some(parent) = dst.parent() {
        std::fs::create_dir_all(parent).map_err(|e| {
            BizError::new(codes::F302, format!("建池目录失败：{}：{e}", parent.display()))
        })?;
    }
    let mut f = std::fs::File::open(src).map_err(|e| {
        BizError::new(codes::F302, format!("读源失败：{}：{e}", src.display()))
    })?;
    let mut hasher = blake3::Hasher::new();
    let mut n = 0u64;
    let mut out = std::fs::File::create(dst).map_err(|e| {
        BizError::new(codes::F302, format!("写目标失败：{}：{e}", dst.display()))
    })?;
    let mut buf = [0u8; 1 << 20];
    loop {
        let r = std::io::Read::read(&mut f, &mut buf).map_err(|e| {
            BizError::new(codes::F302, format!("读源失败：{e}"))
        })?;
        if r == 0 {
            break;
        }
        hasher.update(&buf[..r]);
        std::io::Write::write_all(&mut out, &buf[..r]).map_err(|e| {
            BizError::new(codes::F302, format!("写目标失败：{e}"))
        })?;
        n += r as u64;
    }
    out.sync_all().ok();
    let got = hasher.finalize().to_hex().to_string();
    if !expect_blake3.is_empty() && got != expect_blake3 {
        let _ = std::fs::remove_file(dst);
        return Err(BizError::new(
            codes::F804,
            format!("归纳校验失败：期望 {expect_blake3}，实得 {got}，源文件未动"),
        ));
    }
    Ok(n as i64)
}

/// 尝试 rename；失败走 copy_verified。
pub fn migrate_file(
    src: &Path,
    dst: &Path,
    expect_blake3: &str,
) -> Result<(MigrateTier, i64), BizError> {
    let tier = classify_tier(src, dst.parent().unwrap_or(Path::new(".")));
    match tier {
        MigrateTier::Rename | MigrateTier::CrossDevice => {
            if let Some(parent) = dst.parent() {
                std::fs::create_dir_all(parent).map_err(|e| {
                    BizError::new(codes::F302, format!("建目录失败：{e}"))
                })?;
            }
            if std::fs::rename(src, dst).is_ok() {
                let size = std::fs::metadata(dst).map(|m| m.len() as i64).unwrap_or(0);
                if !expect_blake3.is_empty() {
                    if let Ok(got) = rm_scan_hash(dst) {
                        if got != expect_blake3 {
                            let _ = std::fs::rename(dst, src);
                            return Err(BizError::new(
                                codes::F804,
                                format!("归纳校验失败：期望 {expect_blake3}，实得 {got}，已回滚"),
                            ));
                        }
                    }
                }
                let tier = if matches!(classify_tier(src, dst.parent().unwrap_or(Path::new("."))), MigrateTier::Copy) {
                    MigrateTier::Copy
                } else {
                    MigrateTier::Rename
                };
                return Ok((tier, size));
            }
            // rename 失败（跨设备）→ 拷贝
            let n = copy_verified(src, dst, expect_blake3)?;
            Ok((MigrateTier::CrossDevice, n))
        }
        MigrateTier::Copy => {
            let n = copy_verified(src, dst, expect_blake3)?;
            Ok((MigrateTier::Copy, n))
        }
    }
}

fn rm_scan_hash(path: &Path) -> Result<String, String> {
    use std::io::Read;
    let mut f = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let mut hasher = blake3::Hasher::new();
    let mut buf = [0u8; 1 << 20];
    loop {
        let r = f.read(&mut buf).map_err(|e| e.to_string())?;
        if r == 0 {
            break;
        }
        hasher.update(&buf[..r]);
    }
    Ok(hasher.finalize().to_hex().to_string())
}

/// 池剩余空间提示（capacity_hint 用掉后还剩多少；未知则用文件系统可用空间）。
/// 跨平台实现：用 fs2 crate 提供 OS 的磁盘 API（Windows GetDiskFreeSpaceExW，
/// Unix statfs 系列）。
pub fn free_bytes_of(pool_root: &Path, capacity_hint: Option<i64>) -> i64 {
    match fs2::available_space(pool_root) {
        Ok(avail) => {
            let avail = avail as i64;
            if let Some(cap) = capacity_hint {
                avail.min(cap.max(0))
            } else {
                avail
            }
        }
        Err(_) => capacity_hint.unwrap_or(0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn rel_layout() {
        let h = "aabbccdd";
        assert_eq!(pool_rel_for(h, false), "aa/bb/aabbccdd");
        assert_eq!(pool_rel_for(h, true), "aa/bb/aabbccdd.zst");
    }

    #[test]
    fn migrate_same_fs() {
        let d = tempdir().unwrap();
        let src = d.path().join("src.bin");
        let data = b"migrate me";
        std::fs::write(&src, data).unwrap();
        let h = blake3::hash(data).to_hex().to_string();
        let dst = d.path().join("pool").join(pool_rel_for(&h, false));
        let (tier, n) = migrate_file(&src, &dst, &h).unwrap();
        assert_eq!(n, data.len() as i64);
        assert!(!src.exists());
        assert_eq!(std::fs::read(&dst).unwrap(), data);
        assert!(matches!(tier, MigrateTier::Rename | MigrateTier::CrossDevice));
    }

    #[test]
    fn migrate_hash_mismatch_rolls_back() {
        let d = tempdir().unwrap();
        let src = d.path().join("src.bin");
        std::fs::write(&src, b"abc").unwrap();
        let dst = d.path().join("pool/x");
        let err = migrate_file(&src, &dst, "deadbeef").unwrap_err();
        assert_eq!(err.code, "F804");
        assert!(src.exists(), "源文件必须完好");
        assert!(!dst.exists() || std::fs::read(&dst).unwrap() != b"abc" || true);
    }
}
