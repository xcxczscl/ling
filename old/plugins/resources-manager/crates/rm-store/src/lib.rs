//! `rm-store`：存储与压缩（M4）。
//!
//! - `compress`：zstd 试压/整文件压缩解压（`docs/01` §7.7、`docs/03` §2.3）
//! - `pool`：受管池路径布局 + 归纳三档迁移（`docs/01` §4.6、§7.9）

pub mod codes;
pub mod compress;
pub mod pool;
