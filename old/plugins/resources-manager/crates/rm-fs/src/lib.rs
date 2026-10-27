//! `rm-fs`：文件系统适配 —— 遍历、增量判断所需的元数据。
//!
//! 所有平台差异封装在 `platform` 子模块内；本文件及上游业务代码
//! 均零行 `#[cfg(target_os = ...)]`。

pub mod platform;
pub mod walk;

use std::io;
use std::path::Path;
use std::time::UNIX_EPOCH;

/// 遍历到的一个普通文件（相对根路径的元数据）。
#[derive(Debug, Clone)]
pub struct FileMeta {
    pub rel_path: String,
    pub size: i64,
    pub mtime: i64,
    pub inode: Option<i64>,
}

/// 读取单个文件的元数据（大小/修改时间/inode）。
pub fn file_meta(root: &Path, path: &Path) -> io::Result<FileMeta> {
    let path = platform::to_long_path(path);
    let root = platform::to_long_path(root);

    let meta = std::fs::metadata(&path)?;
    let rel = path
        .strip_prefix(&root)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "路径不在根目录内"))?;
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let inode = platform::inode_of_path(&path);
    let rel_path = platform::from_long_path(rel)
        .to_string_lossy()
        .replace('\\', "/");
    Ok(FileMeta {
        rel_path,
        size: meta.len() as i64,
        mtime,
        inode,
    })
}
