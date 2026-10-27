//! 内置插件目录（`docs/06` §2–§3）：随发行包分发、从属 `ling/builtin-plugins/`，
//! 与普通插件遵守完全相同规范。首次启动（`plugins/` 为空）自动安装三件套，
//! 其余（core-job / core-log）经插件管理页或 `ling install` 按需安装。
//!
//! 发行/开发布局：源码在 `builtin-plugins/<编号>/`（含 `plugin.toml`），
//! 构建产物在工作区 `target/<profile>/lib<编号下划线化>.<平台后缀>`；安装时
//! 暂存为 `{编号}.so` + `plugin.toml` 后走与普通插件完全相同的 `install` 通道。

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::error::KernelError;
use crate::state::Shared;

/// 首次启动默认安装的三件套（`docs/06` §3）。
pub const DEFAULT_TRIO: [&str; 3] = ["core-ui", "core-settings", "core-plugin-manager"];

/// 发行包里的一个内置插件条目（源码目录 + 构建产物）。
#[derive(Debug, Clone)]
pub struct BuiltinEntry {
    pub id: String,
    pub name: String,
    pub description: String,
    pub source_dir: PathBuf,
    pub library: PathBuf,
}

impl BuiltinEntry {
    pub fn to_json(&self) -> Value {
        json!({
            "id": self.id,
            "name": self.name,
            "description": self.description,
            "library": self.library.display().to_string(),
        })
    }
}

/// 扫描 `builtin-plugins/` 目录：每个含 `plugin.toml` 的子目录是一个候选；
/// 对应动态库从 `target/{debug,release}/` 解析（缺失则跳过——未构建）。
pub fn discover(builtin_dir: &Path) -> Vec<BuiltinEntry> {
    let Ok(entries) = fs::read_dir(builtin_dir) else {
        return Vec::new();
    };
    let mut out: Vec<BuiltinEntry> = Vec::new();
    for entry in entries.filter_map(Result::ok) {
        let source_dir = entry.path();
        let manifest_path = source_dir.join("plugin.toml");
        if !manifest_path.is_file() {
            continue;
        }
        let Ok(text) = fs::read_to_string(&manifest_path) else {
            continue;
        };
        let Ok(manifest) = crate::manifest::Manifest::parse_and_validate(&text, &source_dir) else {
            continue;
        };
        let id = manifest.plugin.id.clone();
        let Some(library) = find_library(&source_dir, &id) else {
            continue;
        };
        out.push(BuiltinEntry {
            name: manifest.plugin.name.clone().unwrap_or_else(|| id.clone()),
            description: manifest.plugin.description.clone().unwrap_or_default(),
            id,
            source_dir,
            library,
        });
    }
    out.sort_by(|a, b| a.id.cmp(&b.id));
    out
}

fn find_library(source_dir: &Path, id: &str) -> Option<PathBuf> {
    let workspace = source_dir.parent()?.parent()?;
    let crate_name = id.replace('-', "_");
    let file = format!(
        "{}{}{}",
        std::env::consts::DLL_PREFIX,
        crate_name,
        std::env::consts::DLL_SUFFIX
    );
    for profile in ["debug", "release"] {
        let candidate = workspace.join("target").join(profile).join(&file);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

/// 把一个内置插件条目暂存为安装源目录（`{id}.so` + `plugin.toml`），
/// 落在数据根 `.staging/` 下（安装完成后由调用方删除）。
pub fn stage(shared: &Shared, entry: &BuiltinEntry) -> Result<PathBuf, KernelError> {
    let dir =
        shared
            .data_root
            .join(".staging")
            .join(format!("{}-{}", entry.id, std::process::id()));
    if dir.exists() {
        let _ = fs::remove_dir_all(&dir);
    }
    fs::create_dir_all(&dir).map_err(|e| KernelError::io("stage builtin dir", e.to_string()))?;
    fs::copy(
        entry.source_dir.join("plugin.toml"),
        dir.join("plugin.toml"),
    )
    .map_err(|e| KernelError::io("stage builtin manifest", e.to_string()))?;
    fs::copy(
        &entry.library,
        dir.join(format!("{}{}", entry.id, std::env::consts::DLL_SUFFIX)),
    )
    .map_err(|e| KernelError::io("stage builtin library", e.to_string()))?;
    Ok(dir)
}

/// `plugins/` 目录当前是否没有任何已安装插件（首次启动判定，`docs/06` §3）。
pub fn is_first_start(shared: &Shared) -> bool {
    fs::read_dir(shared.plugins_dir())
        .map(|mut it| it.next().is_none())
        .unwrap_or(true)
}

/// 已安装插件与目录条目的交集（供"可安装"列表筛掉已装的）。
pub fn installed_intersection(shared: &Shared, entries: &[BuiltinEntry]) -> BTreeSet<String> {
    crate::loader::installed_ids(shared)
        .into_iter()
        .filter(|id| entries.iter().any(|e| &e.id == id))
        .collect()
}
