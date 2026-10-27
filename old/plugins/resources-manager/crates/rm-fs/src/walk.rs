//! 目录遍历：流式产出普通文件，跳过目录 symlink（防环），支持忽略规则。

use std::path::{Path, PathBuf};

use crate::FileMeta;

/// 遍历回调返回 `false` 则提前停止。
pub type WalkVisitor<'a> = dyn FnMut(&FileMeta) -> bool + 'a;

/// 缺省忽略名（`docs/01` §7.1 步骤 2 的内置集；用户规则在 rm-scan 层叠加）。
pub const DEFAULT_IGNORES: [&str; 6] = [
    ".git",
    "node_modules",
    "@eaDir",
    ".Trash",
    "__pycache__",
    ".cache",
];

/// 从 `root` 流式遍历全部普通文件。目录 symlink 不进入（防环与防越界）。
///
/// 返回 (访问文件数, 遍历错误列表)。回调返回 `false` 时提前结束。
pub fn walk(root: &Path, ignores: &[String], visitor: &mut WalkVisitor<'_>) -> (u64, Vec<String>) {
    let mut files = 0u64;
    let mut errors = Vec::new();
    let mut stack: Vec<PathBuf> = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let rd = match std::fs::read_dir(&dir) {
            Ok(rd) => rd,
            Err(e) => {
                errors.push(format!("{}: {e}", dir.display()));
                continue;
            }
        };
        for entry in rd.flatten() {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();
            if DEFAULT_IGNORES.contains(&name.as_str()) || ignores.iter().any(|i| i == &name) {
                continue;
            }
            let ft = match entry.file_type() {
                Ok(ft) => ft,
                Err(e) => {
                    errors.push(format!("{}: {e}", path.display()));
                    continue;
                }
            };
            if ft.is_symlink() {
                // 只放行指向普通文件的 symlink；目录 symlink 跳过。
                if let Ok(meta) = std::fs::metadata(&path) {
                    if meta.is_file() {
                        match crate::file_meta(root, &path) {
                            Ok(fm) => {
                                files += 1;
                                if !visitor(&fm) {
                                    return (files, errors);
                                }
                            }
                            Err(e) => errors.push(format!("{}: {e}", path.display())),
                        }
                    }
                }
                continue;
            }
            if ft.is_dir() {
                stack.push(path);
            } else if ft.is_file() {
                match crate::file_meta(root, &path) {
                    Ok(fm) => {
                        files += 1;
                        if !visitor(&fm) {
                            return (files, errors);
                        }
                    }
                    Err(e) => errors.push(format!("{}: {e}", path.display())),
                }
            }
        }
    }
    (files, errors)
}
