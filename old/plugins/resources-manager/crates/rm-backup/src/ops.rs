//! 备份三操作：run / verify / restore（`docs/01` §7.7、`docs/03` §5.3）。

use std::path::{Path, PathBuf};

use rm_proto::BizError;

use crate::codes;
use crate::crypt;
use crate::format::{
    new_snapshot_id, read_entry, read_index, read_manifest, seg_path, write_meta, IndexEntry,
    Manifest, SegmentWriter, SEG_SIZE,
};

/// 备份输入：一条待写入的内容（已有绝对路径）。
pub struct BackupItem {
    pub blake3: String,
    /// 本机可读绝对路径（缺失则内容不可达 → F601/F605 由调用方决定排队）。
    pub path: PathBuf,
    pub size: u64,
}

/// run 的结果统计（对应 `snapshot.stats_json` 固定字段）。
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct BackupStats {
    pub files: u64,
    pub bytes: u64,
    pub new_contents: u64,
    pub segs: u64,
    pub skipped: u64,
    pub snapshot_id: String,
    pub manifest_hash: String,
}

/// 把一批内容备份到 `target`（段式；已存在指纹跳过）。
pub fn backup_run(
    target: &Path,
    device_id: &str,
    location_id: &str,
    items: &[BackupItem],
    password: Option<&str>,
    now: i64,
    on_progress: &mut dyn FnMut(u64, u64),
) -> Result<BackupStats, BizError> {
    std::fs::create_dir_all(target).map_err(|e| {
        BizError::new(
            codes::F601,
            format!("备份目标不可达或不可写：{}：{e}", target.display()),
        )
    })?;
    if !target.is_dir() {
        return Err(BizError::new(
            codes::F601,
            format!("备份目标不是目录：{}", target.display()),
        ));
    }

    let key = match password {
        Some(p) if !p.is_empty() => Some(crypt::derive_key(p.as_bytes())),
        Some(_) | None => None,
    };
    let enc = if key.is_some() { "placeholder" } else { "none" };

    // 增量：读旧索引，已存在跳过。
    let old: Vec<IndexEntry> = if read_manifest(target).is_ok() {
        read_index(target).unwrap_or_default()
    } else {
        Vec::new()
    };
    let mut existing: std::collections::HashMap<String, IndexEntry> = old
        .into_iter()
        .map(|e| (e.blake3.clone(), e))
        .collect();

    // 按指纹升序写，保证索引有序。
    let mut to_write: Vec<&BackupItem> = items.iter().collect();
    to_write.sort_by(|a, b| a.blake3.cmp(&b.blake3));

    let mut w = SegmentWriter::new(target, SEG_SIZE, key)?;
    let mut entries: Vec<IndexEntry> = Vec::new();
    let mut files = 0u64;
    let mut bytes = 0u64;
    let mut new_contents = 0u64;
    let mut skipped = 0u64;
    let total = to_write.len() as u64;
    let mut done = 0u64;

    for item in to_write {
        done += 1;
        on_progress(done, total);
        if existing.contains_key(&item.blake3) {
            skipped += 1;
            files += 1;
            bytes += item.size;
            continue;
        }
        if !item.path.is_file() {
            return Err(BizError::new(
                codes::F605,
                format!("内容不可达（源不在本机在线路径）：{}", item.path.display()),
            ));
        }
        let plain = std::fs::read(&item.path).map_err(|e| {
            BizError::new(
                codes::F601,
                format!("读源失败：{}：{e}", item.path.display()),
            )
        })?;
        let got = blake3::hash(&plain).to_hex().to_string();
        if got != item.blake3 {
            return Err(BizError::new(
                codes::F603,
                format!(
                    "源文件与指纹不符：期望 {}，实得 {got}：{}",
                    item.blake3,
                    item.path.display()
                ),
            ));
        }
        // 试压：值得才 zstd（与 compress.sample 同阈值）。
        let zstd = {
            let sample_len = plain.len().min(1_048_576);
            if sample_len == 0 {
                false
            } else {
                match crate::compress::compress_bytes(&plain[..sample_len]) {
                    Ok(c) => (c.len() as f64) < (sample_len as f64) * 0.95,
                    Err(_) => false,
                }
            }
        };
        let e = w.append(&item.blake3, &plain, zstd)?;
        existing.insert(item.blake3.clone(), e.clone());
        entries.push(e);
        files += 1;
        bytes += item.size;
        new_contents += 1;
    }

    // 合并旧条目 + 新条目，按指纹升序。
    let mut all: Vec<IndexEntry> = existing.into_values().collect();
    all.sort_by(|a, b| a.blake3.cmp(&b.blake3));

    let snapshot_id = new_snapshot_id(now);
    let manifest = Manifest {
        snapshot_id: snapshot_id.clone(),
        created: now,
        device_id: device_id.to_string(),
        location_id: location_id.to_string(),
        entry_count: all.len() as u64,
        total_bytes: all.iter().map(|e| e.len).sum(),
        seg_size: SEG_SIZE,
        enc: enc.to_string(),
    };
    write_meta(target, &manifest, &all)?;
    let man_raw = std::fs::read(crate::format::manifest_path(target)).map_err(|e| {
        BizError::new(codes::F601, format!("回读清单失败：{e}"))
    })?;
    let manifest_hash = blake3::hash(&man_raw).to_hex().to_string();
    let segs = all.iter().map(|e| e.seg).max().unwrap_or(0) as u64;
    Ok(BackupStats {
        files,
        bytes,
        new_contents,
        segs,
        skipped,
        snapshot_id,
        manifest_hash,
    })
}

/// 校验结果状态（`docs/02` §7.8）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum VerifyStatus {
    Ok,
    Mismatch,
    Missing,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct VerifyRow {
    pub blake3: String,
    pub status: VerifyStatus,
    pub detail: String,
}

#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct VerifyReport {
    pub checked: u64,
    pub ok: u64,
    pub mismatch: u64,
    pub missing: u64,
    pub rows: Vec<VerifyRow>,
}

/// 校验备份：先 CHECKSUM，再抽样/全量核对段内数据。
pub fn backup_verify(
    target: &Path,
    full: bool,
    password: Option<&str>,
    on_progress: &mut dyn FnMut(u64, u64),
) -> Result<VerifyReport, BizError> {
    crate::format::verify_checksum(target)?;
    let man = read_manifest(target)?;
    let mut entries = read_index(target)?;
    let key = if man.enc == "placeholder" {
        let pw = crypt::require_password(password)?;
        Some(crypt::derive_key(pw.as_bytes()))
    } else {
        None
    };

    if !full {
        let step = (entries.len() / 100).max(1);
        let sampled: Vec<IndexEntry> = entries.iter().step_by(step).cloned().collect();
        entries = sampled;
    }
    let total = entries.len() as u64;
    let mut report = VerifyReport::default();
    for (i, e) in entries.iter().enumerate() {
        on_progress(i as u64 + 1, total);
        if !seg_path(target, e.seg).is_file() {
            report.missing += 1;
            report.rows.push(VerifyRow {
                blake3: e.blake3.clone(),
                status: VerifyStatus::Missing,
                detail: format!("段 {} 缺失", e.seg),
            });
            continue;
        }
        match read_entry(target, e, key.as_ref()) {
            Ok(_) => {
                report.ok += 1;
                report.rows.push(VerifyRow {
                    blake3: e.blake3.clone(),
                    status: VerifyStatus::Ok,
                    detail: String::new(),
                });
            }
            Err(err) => {
                // 读不出段 → missing；读得出但指纹不对 → mismatch（read_entry 内已分）。
                let status = if err.code == codes::F604 {
                    VerifyStatus::Missing
                } else {
                    VerifyStatus::Mismatch
                };
                match status {
                    VerifyStatus::Missing => report.missing += 1,
                    _ => report.mismatch += 1,
                }
                report.rows.push(VerifyRow {
                    blake3: e.blake3.clone(),
                    status,
                    detail: err.message,
                });
            }
        }
        report.checked += 1;
    }
    if report.mismatch > 0 {
        let rows = serde_json::to_string(&report.rows).unwrap_or_default();
        return Err(BizError::new(
            codes::F603,
            format!("备份校验不一致：{} 条；rows={rows}", report.mismatch),
        ));
    }
    if report.missing > 0 {
        let rows = serde_json::to_string(&report.rows).unwrap_or_default();
        return Err(BizError::new(
            codes::F604,
            format!("备份内容缺失：{} 条；rows={rows}", report.missing),
        ));
    }
    Ok(report)
}

/// 恢复：把选中（或全部）内容写到 `out_dir`，文件名 = blake3。
pub fn backup_restore(
    target: &Path,
    out_dir: &Path,
    only: Option<&[String]>,
    password: Option<&str>,
    on_progress: &mut dyn FnMut(u64, u64),
) -> Result<BackupStats, BizError> {
    let man = read_manifest(target)?;
    let entries = read_index(target)?;
    let key = if man.enc == "placeholder" {
        let pw = crypt::require_password(password)?;
        Some(crypt::derive_key(pw.as_bytes()))
    } else {
        None
    };
    std::fs::create_dir_all(out_dir).map_err(|e| {
        BizError::new(codes::F601, format!("建恢复目录失败：{e}"))
    })?;
    let selected: Vec<&IndexEntry> = match only {
        Some(list) if !list.is_empty() => entries
            .iter()
            .filter(|e| list.iter().any(|b| b == &e.blake3))
            .collect(),
        _ => entries.iter().collect(),
    };
    let total = selected.len() as u64;
    let mut stats = BackupStats {
        snapshot_id: man.snapshot_id.clone(),
        ..Default::default()
    };
    for (i, e) in selected.iter().enumerate() {
        on_progress(i as u64 + 1, total);
        let plain = read_entry(target, e, key.as_ref())?;
        let out = out_dir.join(&e.blake3);
        std::fs::write(&out, &plain).map_err(|err| {
            BizError::new(
                codes::F601,
                format!("写恢复文件失败：{}：{err}", out.display()),
            )
        })?;
        stats.files += 1;
        stats.bytes += plain.len() as u64;
    }
    stats.segs = selected.iter().map(|e| e.seg).max().unwrap_or(0) as u64;
    Ok(stats)
}

/// 列出目标上当前清单里的全部指纹（backup.list 用）。
pub fn list_contents(target: &Path) -> Result<(Manifest, Vec<String>), BizError> {
    let man = read_manifest(target)?;
    let entries = read_index(target)?;
    Ok((man, entries.into_iter().map(|e| e.blake3).collect()))
}

// BizError 扩展：带 detail（校验报告）。若 proto 已有则复用。

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn item(dir: &Path, data: &[u8]) -> (BackupItem, String) {
        let h = blake3::hash(data).to_hex().to_string();
        let p = dir.join(&h);
        std::fs::write(&p, data).unwrap();
        (
            BackupItem {
                blake3: h.clone(),
                path: p,
                size: data.len() as u64,
            },
            h,
        )
    }

    #[test]
    fn backup_verify_restore_roundtrip() {
        let d = tempdir().unwrap();
        let src = d.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        let a = vec![b'x'; 50_000];
        let b = b"tiny".to_vec();
        let (ia, ha) = item(&src, &a);
        let (ib, hb) = item(&src, &b);
        let target = d.path().join("bk");
        let st = backup_run(&target, "dv_a", "lc_h", &[ia, ib], None, 1_760_000_000, &mut |_, _| {})
            .unwrap();
        assert_eq!(st.files, 2);
        assert_eq!(st.new_contents, 2);
        assert!(st.snapshot_id.starts_with("sn_"));

        // 增量再跑：全部跳过。
        let (ia2, _) = item(&src, &a);
        let (ib2, _) = item(&src, &b);
        let st2 = backup_run(&target, "dv_a", "lc_h", &[ia2, ib2], None, 1_760_000_100, &mut |_, _| {})
            .unwrap();
        assert_eq!(st2.new_contents, 0);
        assert_eq!(st2.skipped, 2);

        let rep = backup_verify(&target, true, None, &mut |_, _| {}).unwrap();
        assert_eq!(rep.ok, 2);
        assert_eq!(rep.mismatch, 0);
        assert_eq!(rep.missing, 0);

        let out = d.path().join("restore");
        let rs = backup_restore(&target, &out, None, None, &mut |_, _| {}).unwrap();
        assert_eq!(rs.files, 2);
        assert_eq!(std::fs::read(out.join(&ha)).unwrap(), a);
        assert_eq!(std::fs::read(out.join(&hb)).unwrap(), b);
    }

    #[test]
    fn placeholder_encryption_roundtrip() {
        let d = tempdir().unwrap();
        let src = d.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        let payload = vec![0u8; 300_000];
        let (item, _) = item(&src, &payload);
        let target = d.path().join("bk");
        backup_run(&target, "dv", "lc", &[item], Some("pw"), 2, &mut |_, _| {}).unwrap();
        // 无口令校验 → F606。
        let err = backup_verify(&target, true, None, &mut |_, _| {}).unwrap_err();
        assert_eq!(err.code, codes::F606);
        let rep = backup_verify(&target, true, Some("pw"), &mut |_, _| {}).unwrap();
        assert_eq!(rep.ok, 1);
        let out = d.path().join("out");
        backup_restore(&target, &out, None, Some("pw"), &mut |_, _| {}).unwrap();
        assert_eq!(
            std::fs::read(out.join(blake3::hash(&payload).to_hex().to_string())).unwrap(),
            payload
        );
    }

    #[test]
    fn detect_tamper() {
        let d = tempdir().unwrap();
        let src = d.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        let (item, h) = item(&src, b"payload");
        let target = d.path().join("bk");
        backup_run(&target, "dv", "lc", &[item], None, 3, &mut |_, _| {}).unwrap();
        // 篡改 CHECKSUM → F603。
        std::fs::write(
            crate::format::checksum_path(&target),
            "manifest dead\nindex    beef\n",
        )
        .unwrap();
        let err = backup_verify(&target, true, None, &mut |_, _| {}).unwrap_err();
        assert_eq!(err.code, codes::F603);
        let _ = h;
    }
}
