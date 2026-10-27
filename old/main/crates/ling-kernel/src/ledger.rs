use ling_sdk::LedgerEntryInfo;

use crate::state::PluginRecord;

/// 机制②：效应账本（`docs/02` §3）。条目只增不改，停用/卸载倒序消费；
/// 单条撤销失败不停止，最终汇报失败清单（L2 验收覆盖）。
pub fn take_ledger(record: &mut PluginRecord) -> Vec<crate::state::LedgerEntry> {
    std::mem::take(&mut record.ledger)
}

pub fn run_reverse(entries: Vec<crate::state::LedgerEntry>) -> Vec<String> {
    let mut failures = Vec::new();
    for entry in entries.into_iter().rev() {
        let undo = entry.undo.into_inner().unwrap_or_else(|e| e.into_inner());
        if let Some(undo) = undo {
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(undo));
            match outcome {
                Ok(Ok(())) => {}
                Ok(Err(reason)) => {
                    failures.push(format!("[{}] {}: {}", entry.seq, entry.desc, reason));
                }
                Err(payload) => {
                    let msg = payload
                        .downcast_ref::<&str>()
                        .map(|s| s.to_string())
                        .or_else(|| payload.downcast_ref::<String>().cloned())
                        .unwrap_or_else(|| "panic 载荷非字符串".to_string());
                    failures.push(format!(
                        "[{}] {}: 撤销闭包 panic: {msg}",
                        entry.seq, entry.desc
                    ));
                }
            }
        }
    }
    failures
}

pub fn info(record: &PluginRecord) -> Vec<LedgerEntryInfo> {
    record
        .ledger
        .iter()
        .map(|e| LedgerEntryInfo {
            seq: e.seq,
            kind: e.kind,
            desc: e.desc.clone(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};

    use ling_sdk::{ContextCore, MethodHandler, UndoFn, Value};

    use crate::context::KernelCore;
    use crate::manifest::Manifest;
    use crate::state::{PluginRecord, PluginState, Shared};

    const MANIFEST_TOML: &str = r#"
[plugin]
id = "p1"
version = "0.1.0"
api = "1"
"#;

    fn shared() -> Arc<Shared> {
        Arc::new(Shared::new(PathBuf::from("/tmp/opencode/ledger-unit")))
    }

    fn insert_record(shared: &Arc<Shared>) {
        let manifest = Manifest::parse_and_validate(MANIFEST_TOML, PathBuf::from("/").as_path())
            .expect("测试清单可解析");
        shared.plugins.write().unwrap().insert(
            "p1".to_string(),
            PluginRecord {
                manifest,
                manifest_text: MANIFEST_TOML.to_string(),
                state: PluginState::Activated,
                instance: None,
                library: None,
                library_file: None,
                ledger: Vec::new(),
                next_seq: 1,
                deactivate_cbs: Vec::new(),
            },
        );
    }

    fn handler() -> MethodHandler {
        Box::new(|_| Ok(Value::Null))
    }

    fn recorder(order: &Arc<Mutex<Vec<String>>>, tag: &str) -> UndoFn {
        let order = Arc::clone(order);
        let tag = tag.to_string();
        Box::new(move || order.lock().unwrap().push(tag))
    }

    fn ledger_recorder(order: &Arc<Mutex<Vec<String>>>, tag: &str) -> crate::state::Undo {
        let order = Arc::clone(order);
        let tag = tag.to_string();
        Box::new(move || {
            order.lock().unwrap().push(tag);
            Ok(())
        })
    }

    #[test]
    fn ledger_count_matches_registrations_and_runs_in_reverse() {
        let shared = shared();
        insert_record(&shared);
        let core = Arc::new(KernelCore {
            shared: Arc::clone(&shared),
        });
        let order: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));

        core.register_method("p1", "p1.m1", handler(), recorder(&order, "m1"), None)
            .expect("注册 m1");
        core.register_method("p1", "p1.m2", handler(), recorder(&order, "m2"), None)
            .expect("注册 m2");
        core.subscribe("p1", "p1.*", Box::new(|_| ()), recorder(&order, "sub"))
            .expect("订阅");
        core.on_deactivate("p1", recorder(&order, "cb"));

        let methods = shared.methods.read().unwrap();
        let method_count = methods.values().filter(|m| m.owner == "p1").count();
        drop(methods);
        let subs = shared.subs.read().unwrap();
        let sub_count = subs.iter().filter(|s| s.owner == "p1").count();
        drop(subs);
        let mut plugins = shared.plugins.write().unwrap();
        let record = plugins.get_mut("p1").unwrap();
        let cb_count = record.deactivate_cbs.len();
        assert_eq!(
            record.ledger.len(),
            method_count + sub_count + cb_count,
            "账本条目数与注册数一致"
        );
        assert_eq!(record.ledger.len(), 4);

        let cbs = record.take_deactivate_cbs();
        let entries = crate::ledger::take_ledger(record);
        drop(plugins);
        for cb in cbs {
            cb();
        }
        let failures = crate::ledger::run_reverse(entries);
        assert!(failures.is_empty(), "正常路径全部撤销成功：{failures:?}");

        assert_eq!(
            *order.lock().unwrap(),
            vec!["cb", "sub", "m2", "m1"],
            "停用回调先行，随后倒序撤销"
        );
        assert!(
            shared
                .methods
                .read()
                .unwrap()
                .keys()
                .all(|k| !k.starts_with("p1.")),
            "方法表已清空"
        );
        assert!(
            shared.subs.read().unwrap().iter().all(|s| s.owner != "p1"),
            "订阅表已清空"
        );
    }

    #[test]
    fn run_reverse_reports_failures_and_continues() {
        let shared = shared();
        insert_record(&shared);
        let order: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let mut plugins = shared.plugins.write().unwrap();
        let record = plugins.get_mut("p1").unwrap();
        record.push_effect(
            ling_sdk::EffectKind::MethodRegistered,
            "注册 a",
            ledger_recorder(&order, "a"),
        );
        record.push_effect(
            ling_sdk::EffectKind::MethodRegistered,
            "注册 b",
            Box::new(|| Err("注入错误：撤销失败".to_string())),
        );
        record.push_effect(
            ling_sdk::EffectKind::MethodRegistered,
            "注册 c",
            ledger_recorder(&order, "c"),
        );
        let entries = crate::ledger::take_ledger(record);
        drop(plugins);

        let failures = crate::ledger::run_reverse(entries);
        assert_eq!(failures.len(), 1);
        assert!(failures[0].contains("[2]"));
        assert!(failures[0].contains("注入错误：撤销失败"));
        assert_eq!(
            *order.lock().unwrap(),
            vec!["c", "a"],
            "失败条目（中间）不影响前后条目执行"
        );
    }

    #[test]
    fn run_reverse_survives_panics() {
        let shared = shared();
        insert_record(&shared);
        let order: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let mut plugins = shared.plugins.write().unwrap();
        let record = plugins.get_mut("p1").unwrap();
        record.push_effect(
            ling_sdk::EffectKind::MethodRegistered,
            "注册 a",
            ledger_recorder(&order, "a"),
        );
        record.push_effect(
            ling_sdk::EffectKind::MethodRegistered,
            "注册 toxic",
            Box::new(|| panic!("故障注入：撤销崩溃")),
        );
        let entries = crate::ledger::take_ledger(record);
        drop(plugins);

        let failures = crate::ledger::run_reverse(entries);
        assert_eq!(failures.len(), 1);
        assert!(failures[0].contains("[2]"));
        assert!(failures[0].contains("panic"));
        assert!(failures[0].contains("故障注入"));
        assert_eq!(*order.lock().unwrap(), vec!["a"], "其余撤销继续执行");
    }
}
