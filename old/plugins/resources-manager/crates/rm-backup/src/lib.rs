//! `rm-backup`：段式备份（M4，`docs/03` §5.3，格式已冻结）。
//!
//! - `format`：manifest / segments.idx / CHECKSUM / 段读写
//! - `ops`：backup_run / verify / restore
//! - `crypt`：占位加密（§5.3.1，非真实保密）

pub mod codes;
pub mod crypt;
pub mod format;
pub mod ops;

/// 与 rm-store 共用的压缩实现（避免领域包互 import，薄 re-export）。
pub mod compress {
    pub use rm_store::compress::{compress_bytes, decompress_bytes};
}

pub use ops::{backup_restore, backup_run, backup_verify, list_contents, BackupItem, BackupStats, VerifyReport, VerifyStatus};
