use std::path::PathBuf;
use std::sync::Arc;

use ling_sdk::{
    CallError, Capability, Context, ContextCore, ContextError, EffectKind, EventCallback,
    LedgerEntryInfo, MethodHandler, UndoFn, Value,
};

use crate::{broker, bus, ledger, state::Shared};

/// [`ContextCore`] 的内核实现：Context 的每一步 = 查表 + 写表 + 记一笔账
/// （`docs/02` §1.3）。
pub struct KernelCore {
    pub shared: Arc<Shared>,
}

impl KernelCore {
    pub fn context_for(
        self: &Arc<Self>,
        plugin: &str,
        caps: std::collections::BTreeSet<Capability>,
    ) -> Context {
        Context::new(Arc::clone(self) as Arc<dyn ContextCore>, plugin, caps)
    }
}

impl ContextCore for KernelCore {
    fn register_method(
        &self,
        plugin: &str,
        name: &str,
        handler: MethodHandler,
        unregister: UndoFn,
        requires: Option<Capability>,
    ) -> Result<(), ContextError> {
        broker::register(
            &self.shared,
            plugin,
            name,
            handler.into(),
            unregister,
            requires,
        )
    }

    fn call(&self, plugin: &str, method: &str, args: Value) -> Result<Value, CallError> {
        broker::call(&self.shared, plugin, method, args)
    }

    fn subscribe(
        &self,
        plugin: &str,
        pattern: &str,
        callback: EventCallback,
        unsubscribe: UndoFn,
    ) -> Result<(), ContextError> {
        bus::subscribe(
            &self.shared,
            plugin,
            pattern,
            bus::wrap_plugin_callback(callback),
            unsubscribe,
        )
    }

    fn emit(&self, plugin: &str, topic: &str, payload: Value) -> Result<(), ContextError> {
        bus::emit(&self.shared, plugin, topic, payload)
    }

    fn data_dir(&self, plugin: &str) -> PathBuf {
        let dir = self.shared.plugin_dir(plugin).join("data");
        let _ = std::fs::create_dir_all(&dir);
        dir
    }

    fn on_deactivate(&self, plugin: &str, callback: UndoFn) {
        let mut plugins = self.shared.plugins.write().unwrap();
        if let Some(record) = plugins.get_mut(plugin) {
            record.push_effect(
                EffectKind::DeactivateCallback,
                "登记停用回调",
                Box::new(|| Ok(())),
            );
            record.push_deactivate_cb(callback);
        }
    }

    fn ledger_info(&self, plugin: &str) -> Vec<LedgerEntryInfo> {
        let plugins = self.shared.plugins.read().unwrap();
        plugins.get(plugin).map(ledger::info).unwrap_or_default()
    }
}
