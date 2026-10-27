use std::sync::Arc;

use ling_sdk::{CallError, Capability, ContextError, EffectKind, Value};

use crate::caps;
use crate::state::{Handler, MethodEntry, PluginState, Shared};

/// 服务①：接口方法路由（`docs/01` §3.2）。
///
/// 锁纪律：`plugins` 与 `methods` 不嵌套持有（先放 methods 再记账，或反序时
/// 先释放外层再取内层），杜绝 AB-BA。
pub fn register(
    shared: &Arc<Shared>,
    owner: &str,
    name: &str,
    handler: Handler,
    plugin_undo: ling_sdk::UndoFn,
    requires: Option<Capability>,
) -> Result<(), ContextError> {
    if !name.starts_with(owner) || name.as_bytes().get(owner.len()) != Some(&b'.') {
        shared.record_violation(owner, "method", name);
        return Err(ContextError::namespace_violation(format!(
            "方法名 {name:?} 未带本插件编号前缀 {owner}."
        )));
    }
    if let Some(cap) = requires {
        let declared = {
            let caps = shared.caps.read().unwrap();
            caps.get(owner)
                .map(|c| c.declared.contains(&cap))
                .unwrap_or(false)
        };
        if !declared {
            return Err(ContextError::capability_denied(format!(
                "方法 {name} 标注需要能力 {cap}，但清单未声明"
            )));
        }
    }

    {
        let mut methods = shared.methods.write().unwrap();
        if methods.contains_key(name) {
            return Err(ContextError::new(
                "E_METHOD_DUPLICATE",
                format!("方法已注册：{name}"),
            ));
        }
        methods.insert(
            name.to_string(),
            MethodEntry {
                owner: owner.to_string(),
                requires,
                handler,
            },
        );
    }

    let undo_name = name.to_string();
    let shared_undo = Arc::clone(shared);
    let undo: Box<dyn FnOnce() -> Result<(), String> + Send> = Box::new(move || {
        shared_undo.methods.write().unwrap().remove(&undo_name);
        plugin_undo();
        Ok(())
    });

    let mut plugins = shared.plugins.write().unwrap();
    match plugins.get_mut(owner) {
        Some(record) => {
            record.push_effect(
                EffectKind::MethodRegistered,
                format!("注册方法 {name}"),
                undo,
            );
            Ok(())
        }
        None => {
            drop(plugins);
            shared.methods.write().unwrap().remove(name);
            Err(ContextError::new(
                ling_sdk::codes::E_PLUGIN_MISSING,
                format!("插件无装载记录：{owner}"),
            ))
        }
    }
}

pub fn call(
    shared: &Arc<Shared>,
    plugin: &str,
    method: &str,
    args: Value,
) -> Result<Value, CallError> {
    let full = Shared::full_method_name(plugin, method);

    let (owner, requires, handler) = {
        let methods = shared.methods.read().unwrap();
        let entry = methods
            .get(&full)
            .ok_or_else(|| CallError::method_missing(&full))?;
        (
            entry.owner.clone(),
            entry.requires,
            Arc::clone(&entry.handler),
        )
    };

    {
        let plugins = shared.plugins.read().unwrap();
        let record = plugins
            .get(&owner)
            .ok_or_else(|| CallError::plugin_missing(&owner))?;
        if record.state != PluginState::Activated {
            return Err(CallError::plugin_missing(&owner));
        }
    }

    if let Some(cap) = requires {
        if !caps::effective(shared, &owner, cap) {
            return Err(CallError::capability_denied(format!(
                "方法 {full} 需要能力 {cap}，插件 {owner} 当前无此生效能力"
            )));
        }
    }

    handler(args)
}
