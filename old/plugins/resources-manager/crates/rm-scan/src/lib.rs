//! `rm-scan`：扫描作业的纯逻辑（`docs/01` §7.1–§7.2）。
//!
//! 分层哈希：第一级大小（免费）→ 第二级首尾 64 KB 快速指纹 → 第三级全文 BLAKE3。
//! 增量：路径/大小/mtime 未变则跳过重算（第二次全盘只读元数据）。

pub mod hash_tier;
pub mod walker;

pub use walker::{scan_location, ScanOptions, ScanOutcome, ScanProgress};
