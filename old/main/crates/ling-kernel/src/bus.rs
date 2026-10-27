use std::sync::Arc;

use ling_sdk::{ContextError, EffectKind, Value};

use crate::state::{Callback, Shared, SubEntry};

/// 服务②：事件广播（`docs/01` §3.2）。订阅不限制命名空间（插件可听 `kernel.*`），
/// 广播必须以自己编号开头。
pub fn subscribe(
    shared: &Arc<Shared>,
    owner: &str,
    pattern: &str,
    callback: Callback,
    plugin_undo: ling_sdk::UndoFn,
) -> Result<(), ContextError> {
    let id = shared
        .sub_seq
        .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    {
        let mut subs = shared.subs.write().unwrap();
        subs.push(SubEntry {
            id,
            owner: owner.to_string(),
            pattern: pattern.to_string(),
            callback,
        });
    }

    let shared_undo = Arc::clone(shared);
    let undo: Box<dyn FnOnce() -> Result<(), String> + Send> = Box::new(move || {
        shared_undo.subs.write().unwrap().retain(|s| s.id != id);
        plugin_undo();
        Ok(())
    });

    let mut plugins = shared.plugins.write().unwrap();
    match plugins.get_mut(owner) {
        Some(record) => {
            record.push_effect(
                EffectKind::EventSubscribed,
                format!("订阅事件 {pattern}"),
                undo,
            );
            Ok(())
        }
        None => {
            drop(plugins);
            shared.subs.write().unwrap().retain(|s| s.id != id);
            Err(ContextError::new(
                ling_sdk::codes::E_PLUGIN_MISSING,
                format!("插件无装载记录：{owner}"),
            ))
        }
    }
}

/// 把插件侧 SDK 回调（只见载荷）适配到内核回调（带主题）。
pub fn wrap_plugin_callback(callback: ling_sdk::EventCallback) -> Callback {
    Arc::new(move |_topic: &str, payload: Value| callback(payload))
}

pub fn emit(
    shared: &Arc<Shared>,
    owner: &str,
    topic: &str,
    payload: Value,
) -> Result<(), ContextError> {
    if !topic.starts_with(owner) || topic.as_bytes().get(owner.len()) != Some(&b'.') {
        shared.record_violation(owner, "topic", topic);
        return Err(ContextError::namespace_violation(format!(
            "事件主题 {topic:?} 未带本插件编号前缀 {owner}."
        )));
    }
    shared.dispatch(topic, payload);
    Ok(())
}

/// 宿主侧订阅（`Kernel::subscribe`）：不记入任何插件账本，卸载核对不受影响。
pub fn kernel_subscribe(shared: &Arc<Shared>, pattern: &str, callback: Callback) -> u64 {
    let id = shared
        .sub_seq
        .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    shared.subs.write().unwrap().push(SubEntry {
        id,
        owner: "kernel".to_string(),
        pattern: pattern.to_string(),
        callback,
    });
    id
}

pub fn kernel_unsubscribe(shared: &Arc<Shared>, id: u64) {
    shared.subs.write().unwrap().retain(|s| s.id != id);
}
