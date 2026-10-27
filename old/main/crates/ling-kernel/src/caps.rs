//! 服务③：能力门禁（`docs/01` §3.2）。
//!
//! 注册时：方法标注 requires ⊆ 本插件清单声明（在 `broker::register` 执行）。
//! 调用时：核对目标插件的生效能力 = 清单声明 − 用户撤销黑名单（在 `broker::call` 执行）。
//! 黑名单是宿主级的（`docs/06` §4.4）：持久化于 `ling/registry.json`，不随
//! 装载/卸载清除；授权界面属 core-plugin-manager（L5），内核只留操作入口。

use std::collections::BTreeSet;
use std::sync::Arc;

use ling_sdk::Capability;

use crate::registry;
use crate::state::{CapEntry, Shared};

pub fn set_declared(shared: &Arc<Shared>, plugin: &str, declared: BTreeSet<Capability>) {
    let mut caps = shared.caps.write().unwrap();
    caps.insert(plugin.to_string(), CapEntry { declared });
}

pub fn remove(shared: &Arc<Shared>, plugin: &str) {
    shared.caps.write().unwrap().remove(plugin);
}

pub fn effective(shared: &Shared, plugin: &str, cap: Capability) -> bool {
    let declared = shared
        .caps
        .read()
        .unwrap()
        .get(plugin)
        .map(|e| e.declared.contains(&cap))
        .unwrap_or(false);
    let revoked = shared
        .revoked
        .read()
        .unwrap()
        .get(plugin)
        .map(|r| r.contains(&cap))
        .unwrap_or(false);
    declared && !revoked
}

pub fn revoke(shared: &Arc<Shared>, plugin: &str, cap: Capability) -> Result<(), String> {
    {
        let caps = shared.caps.read().unwrap();
        let entry = caps
            .get(plugin)
            .ok_or_else(|| format!("插件未装载：{plugin}"))?;
        if !entry.declared.contains(&cap) {
            return Err(format!("插件 {plugin} 清单未声明能力 {cap}"));
        }
    }
    shared
        .revoked
        .write()
        .unwrap()
        .entry(plugin.to_string())
        .or_default()
        .insert(cap);
    registry::persist_shared(shared).map_err(|e| format!("写入 registry.json 失败：{e}"))
}

pub fn grant(shared: &Arc<Shared>, plugin: &str, cap: Capability) -> Result<(), String> {
    {
        let caps = shared.caps.read().unwrap();
        let entry = caps
            .get(plugin)
            .ok_or_else(|| format!("插件未装载：{plugin}"))?;
        if !entry.declared.contains(&cap) {
            return Err(format!("插件 {plugin} 清单未声明能力 {cap}"));
        }
    }
    if let Some(set) = shared.revoked.write().unwrap().get_mut(plugin) {
        set.remove(&cap);
    }
    registry::persist_shared(shared).map_err(|e| format!("写入 registry.json 失败：{e}"))
}
