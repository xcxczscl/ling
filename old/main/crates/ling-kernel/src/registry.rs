use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io;
use std::path::Path;

use serde::{Deserialize, Serialize};

/// 宿主级登记文件 `ling/registry.json`（`docs-common/01` §2.3）：
/// 实际端口、停用名单、能力撤销黑名单——"宿主级登记，不归任何插件"。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RegistryFile {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub management_port: Option<u16>,
    /// core-ui 网页端口（`docs/06` §4.1：实际端口写 registry.json）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ui_port: Option<u16>,
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub disabled: BTreeSet<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub revoked: std::collections::BTreeMap<String, BTreeSet<String>>,
}

pub fn read(data_root: &Path) -> RegistryFile {
    let path = data_root.join("registry.json");
    fs::read_to_string(&path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

/// 以内存中的宿主状态（停用名单、撤销黑名单）覆盖写回，保留文件中其他字段。
pub fn persist_shared(shared: &crate::state::Shared) -> io::Result<()> {
    let mut registry = read(&shared.data_root);
    registry.disabled = shared.disabled.read().unwrap().clone();
    registry.revoked = shared
        .revoked
        .read()
        .unwrap()
        .iter()
        .map(|(plugin, caps)| {
            (
                plugin.clone(),
                caps.iter().map(|c| c.as_str().to_string()).collect(),
            )
        })
        .collect();
    write(&shared.data_root, &registry)
}

pub fn write(data_root: &Path, registry: &RegistryFile) -> io::Result<()> {
    let path = data_root.join("registry.json");
    let text = serde_json::to_string_pretty(registry).map_err(io::Error::other)?;
    let tmp = data_root.join(".registry.json.tmp");
    fs::write(&tmp, text)?;
    fs::rename(&tmp, &path)?;
    Ok(())
}
