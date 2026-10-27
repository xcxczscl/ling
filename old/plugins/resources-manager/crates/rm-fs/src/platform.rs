//! 平台适配入口：把所有 OS 差异封装在本文件内，业务代码零行平台编译门。
//!
//! 三层结构：
//!   1. `inode_of_path()` / `dev_of_path()` —— 跨平台通用，用 file-id crate 实现，
//!      业务层（rm-plugin / rm-store）一律调这两个，不写任何 #[cfg]。
//!   2. `windows::to_long_path()` / `windows::from_long_path()` —— Windows 专属
//!      长路径 workaround，Unix 无 MAX_PATH 限制，不需要对应函数。
//!   3. `android_saf::*` —— Android 专属 SAF 授权占位，桌面平台返回空注册表。

use std::path::{Path, PathBuf};

/// 取 inode / hard-link ID（跨平台）。
/// Unix:   `FileId::Inode { inode_number, .. }`
/// Windows: `FileId::LowRes { file_index, .. }` 或 `HighRes { file_id, .. }`（取低 64 位）
/// 其他平台或出错: None
pub fn inode_of_path(path: &Path) -> Option<i64> {
    let fid = file_id::get_file_id(path).ok()?;
    match fid {
        file_id::FileId::Inode { inode_number, .. } => Some(inode_number as i64),
        file_id::FileId::LowRes { file_index, .. } => Some(file_index as i64),
        file_id::FileId::HighRes { file_id, .. } => Some(file_id as i64),
    }
}

/// 取设备号（跨平台）。用于判断两个路径是否同一文件系统（同盘 rename vs 跨盘 copy）。
/// Unix:   `device_id`
/// Windows: `volume_serial_number`
pub fn dev_of_path(path: &Path) -> Option<i64> {
    let fid = file_id::get_file_id(path).ok()?;
    match fid {
        file_id::FileId::Inode { device_id, .. } => Some(device_id as i64),
        file_id::FileId::LowRes { volume_serial_number, .. } => Some(volume_serial_number as i64),
        file_id::FileId::HighRes { volume_serial_number, .. } => Some(volume_serial_number as i64),
    }
}

// ---------------------------------------------------------------------------
// Windows 专属：长路径前缀 `\\?\` 与盘符规范化。
// 下面这两个顶层包装函数对所有平台可见——非 Windows 上原样返回，
// 这样下游调用方（lib.rs / walk.rs）完全不用写任何 #[cfg]。

/// 给路径加 `\\?\` 前缀（Windows 且长度逼近 MAX_PATH 时）；其他平台原样返回。
#[allow(unused_variables)]
pub fn to_long_path(p: &Path) -> PathBuf {
    #[cfg(windows)]
    {
        return windows::to_long_path(p);
    }
    #[cfg(not(windows))]
    {
        return p.to_path_buf();
    }
}

/// 去掉 `\\?\` / `\\.\` 前缀（Windows）；其他平台原样返回。
#[allow(unused_variables)]
pub fn from_long_path(p: &Path) -> PathBuf {
    #[cfg(windows)]
    {
        return windows::from_long_path(p);
    }
    #[cfg(not(windows))]
    {
        return p.to_path_buf();
    }
}

#[cfg(windows)]
mod windows {
    use super::{Path, PathBuf};

    /// 给路径加 `\\?\` 前缀（仅当绝对路径且尚未有前缀且长度逼近 MAX_PATH）。
    pub fn to_long_path(p: &Path) -> PathBuf {
        let s = p.to_string_lossy();
        if s.starts_with(r"\\?\") || s.starts_with(r"\\.\") {
            return p.to_path_buf();
        }
        // UNC 路径 \\server\share → \\?\UNC\server\share
        if s.starts_with(r"\\") {
            return PathBuf::from(format!(r"\\?\UNC\{}", s.trim_start_matches(r"\\")));
        }
        // 绝对路径（含盘符）且长度 >= 240 时加前缀。
        if p.is_absolute() && s.len() >= 240 {
            PathBuf::from(format!(r"\\?\{s}"))
        } else {
            p.to_path_buf()
        }
    }

    /// 去掉 `\\?\` / `\\.\` 前缀，还原用户可见形式。
    pub fn from_long_path(p: &Path) -> PathBuf {
        let s = p.to_string_lossy();
        if let Some(rest) = s.strip_prefix(r"\\?\UNC\") {
            return PathBuf::from(format!(r"\\{rest}"));
        }
        if let Some(rest) = s.strip_prefix(r"\\?\") {
            return PathBuf::from(rest);
        }
        if let Some(rest) = s.strip_prefix(r"\\.\") {
            return PathBuf::from(rest);
        }
        p.to_path_buf()
    }
}

// ---------------------------------------------------------------------------
// Android 专属：SAF 授权目录占位。

#[cfg(target_os = "android")]
pub mod android_saf {
    use serde::{Deserialize, Serialize};

    /// 一条已授权目录（树访问 URI + 显示名）。
    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub struct GrantedDir {
        pub tree_uri: String,
        pub display_name: String,
        pub granted_at: i64,
    }

    #[derive(Default)]
    pub struct SafRegistry {
        dirs: Vec<GrantedDir>,
    }

    impl SafRegistry {
        pub fn new() -> Self { Self::default() }
        pub fn grant(&mut self, dir: GrantedDir) {
            self.dirs.retain(|d| d.tree_uri != dir.tree_uri);
            self.dirs.push(dir);
        }
        pub fn revoke(&mut self, tree_uri: &str) -> bool {
            let before = self.dirs.len();
            self.dirs.retain(|d| d.tree_uri != tree_uri);
            self.dirs.len() != before
        }
        pub fn list(&self) -> &[GrantedDir] { &self.dirs }
        pub fn is_granted(&self, user_path: &str) -> bool {
            self.dirs.iter().any(|d| user_path.starts_with(&d.display_name))
        }
    }
}
