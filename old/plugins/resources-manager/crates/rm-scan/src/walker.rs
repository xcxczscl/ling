//! 扫描编排：遍历 → 增量判断 → 分层哈希 → 批量入库 → 进度回调。

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use rm_catalog::repo::{content, placement};
use rm_proto::asset_type_for_ext;

use crate::hash_tier;

/// 进度快照（每批提交后回调一次，`docs/01` §7.1 步骤 5）。
#[derive(Debug, Clone, Default)]
pub struct ScanProgress {
    pub files_seen: u64,
    pub files_hashed: u64,
    pub files_skipped: u64,
    pub bytes_hashed: u64,
    pub errors: u64,
}

/// 扫描结果统计。
#[derive(Debug, Clone, Default)]
pub struct ScanOutcome {
    pub progress: ScanProgress,
    pub contents_new: u64,
    pub placements_new: u64,
    pub placements_updated: u64,
    pub error_list: Vec<String>,
}

/// 扫描参数。
pub struct ScanOptions<'a> {
    pub location_id: &'a str,
    pub root: &'a Path,
    /// `full=true` 强制重算哈希。
    pub full: bool,
    /// 每批提交行数（`docs/01` §7.1 步骤 4：1–2 万）。
    pub batch_size: usize,
    pub ignores: &'a [String],
}

type BatchRow = (String, i64, i64, Option<i64>, String, String);

fn biz(e: impl std::fmt::Display) -> rm_proto::BizError {
    rm_proto::BizError::new(rm_proto::codes::F301, e.to_string())
}

/// 执行一次扫描。`cancel` 置真则在批边界安全停止（永不半写——已提交批次完整）。
///
/// `on_progress` 每批提交后调用。
pub fn scan_location(
    conn: &mut rusqlite::Connection,
    opts: ScanOptions<'_>,
    cancel: &AtomicBool,
    on_progress: &mut dyn FnMut(&ScanProgress),
) -> Result<ScanOutcome, rm_proto::BizError> {
    let now = rm_catalog::now();
    let mut files_seen = 0u64;
    let mut files_hashed = 0u64;
    let mut files_skipped = 0u64;
    let mut bytes_hashed = 0u64;
    let mut contents_new = 0u64;
    let mut placements_new = 0u64;
    let mut placements_updated = 0u64;
    let mut error_list: Vec<String> = Vec::new();
    let mut batch: Vec<BatchRow> = Vec::new();
    let batch_size = opts.batch_size.max(1);

    let flush = |conn: &mut rusqlite::Connection,
                 batch: &mut Vec<BatchRow>,
                 contents_new: &mut u64,
                 placements_new: &mut u64,
                 placements_updated: &mut u64|
     -> Result<(), rm_proto::BizError> {
        if batch.is_empty() {
            return Ok(());
        }
        let tx = conn.transaction().map_err(biz)?;
        for (rel, size, mtime, inode, hash, asset) in batch.iter() {
            if content::insert_if_absent(&tx, hash, *size, asset, now).map_err(biz)? {
                *contents_new += 1;
            }
            match placement::placement_find(&tx, opts.location_id, rel).map_err(biz)? {
                Some((pid, _cid, _s, _m, _i)) => {
                    placement::placement_refresh(&tx, pid, hash, *size, *mtime, *inode)
                        .map_err(biz)?;
                    *placements_updated += 1;
                }
                None => {
                    placement::placement_add(
                        &tx,
                        opts.location_id,
                        rel,
                        hash,
                        *size,
                        *mtime,
                        *inode,
                        "online",
                        now,
                    )
                    .map_err(biz)?;
                    *placements_new += 1;
                }
            }
        }
        tx.commit().map_err(biz)?;
        batch.clear();
        Ok(())
    };

    let root = opts.root.to_path_buf();
    let ignores: Vec<String> = opts.ignores.to_vec();
    let full = opts.full;
    let location_id = opts.location_id.to_string();

    let walk_result = rm_fs::walk::walk(&root, &ignores, &mut |fm| {
        files_seen += 1;
        if cancel.load(Ordering::SeqCst) {
            return false;
        }
        if !full {
            if let Ok(Some((_, _cid, old_size, old_mtime, _inode))) =
                placement::placement_find(conn, &location_id, &fm.rel_path)
            {
                if old_size == fm.size && old_mtime == fm.mtime {
                    files_skipped += 1;
                    return true;
                }
            }
        }
        match hash_tier::full_hash(&root.join(&fm.rel_path)) {
            Ok(hash) => {
                files_hashed += 1;
                bytes_hashed += fm.size as u64;
                let ext = Path::new(&fm.rel_path)
                    .extension()
                    .map(|e| e.to_string_lossy().to_string())
                    .unwrap_or_default();
                let asset = asset_type_for_ext(&ext).to_string();
                batch.push((
                    fm.rel_path.clone(),
                    fm.size,
                    fm.mtime,
                    fm.inode,
                    hash,
                    asset,
                ));
                if batch.len() >= batch_size {
                    if let Err(e) = flush(
                        conn,
                        &mut batch,
                        &mut contents_new,
                        &mut placements_new,
                        &mut placements_updated,
                    ) {
                        error_list.push(e.message.clone());
                    }
                    on_progress(&ScanProgress {
                        files_seen,
                        files_hashed,
                        files_skipped,
                        bytes_hashed,
                        errors: error_list.len() as u64,
                    });
                }
                true
            }
            Err(e) => {
                error_list.push(format!("{}: {e}", fm.rel_path));
                true
            }
        }
    });
    error_list.extend(walk_result.1);
    let _ = flush(
        conn,
        &mut batch,
        &mut contents_new,
        &mut placements_new,
        &mut placements_updated,
    );
    Ok(ScanOutcome {
        progress: ScanProgress {
            files_seen,
            files_hashed,
            files_skipped,
            bytes_hashed,
            errors: error_list.len() as u64,
        },
        contents_new,
        placements_new,
        placements_updated,
        error_list,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;

    fn make_tmp() -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        std::fs::write(root.join("a.bin"), b"hello").unwrap();
        std::fs::write(root.join("b.bin"), b"world").unwrap();
        (dir, root)
    }

    #[test]
    fn incremental_rescan_skips_unchanged() {
        let mut conn = rm_catalog::open_memory().unwrap();
        let _ = conn.execute(
            "INSERT INTO device VALUES('dv1','dev','linux','fp','online',0)",
            [],
        );
        let _ = conn.execute(
            "INSERT INTO location VALUES('loc1','dv1','/tmp/x','local','online',NULL)",
            [],
        );
        let (_d, root) = make_tmp();
        let cancel = AtomicBool::new(false);
        let mut prog = |_: &ScanProgress| {};
        let o1 = scan_location(
            &mut conn,
            ScanOptions {
                location_id: "loc1",
                root: &root,
                full: false,
                batch_size: 100,
                ignores: &[],
            },
            &cancel,
            &mut prog,
        )
        .unwrap();
        assert_eq!(o1.progress.files_seen, 2);
        assert_eq!(o1.progress.files_hashed, 2);
        assert_eq!(o1.placements_new, 2);
        // 第二次扫描：size+mtime 未变 → 跳过哈希。
        let o2 = scan_location(
            &mut conn,
            ScanOptions {
                location_id: "loc1",
                root: &root,
                full: false,
                batch_size: 100,
                ignores: &[],
            },
            &cancel,
            &mut prog,
        )
        .unwrap();
        assert_eq!(o2.progress.files_hashed, 0);
        assert_eq!(o2.progress.files_skipped, 2);
        assert_eq!(o2.placements_new, 0);
        // full=true 强制重算。
        let o3 = scan_location(
            &mut conn,
            ScanOptions {
                location_id: "loc1",
                root: &root,
                full: true,
                batch_size: 100,
                ignores: &[],
            },
            &AtomicBool::new(false),
            &mut prog,
        )
        .unwrap();
        assert_eq!(o3.progress.files_hashed, 2);
    }

    #[test]
    fn cancel_stops_at_batch_boundary() {
        let mut conn = rm_catalog::open_memory().unwrap();
        let _ = conn.execute(
            "INSERT INTO device VALUES('dv1','dev','linux','fp','online',0)",
            [],
        );
        let _ = conn.execute(
            "INSERT INTO location VALUES('loc1','dv1','/tmp/x','local','online',NULL)",
            [],
        );
        let (_d, root) = make_tmp();
        let cancel = AtomicBool::new(true); // 立即取消
        let mut prog = |_: &ScanProgress| {};
        let o = scan_location(
            &mut conn,
            ScanOptions {
                location_id: "loc1",
                root: &root,
                full: false,
                batch_size: 1,
                ignores: &[],
            },
            &cancel,
            &mut prog,
        )
        .unwrap();
        assert!(o.progress.files_seen == 0 || o.placements_new == 0);
    }
}
