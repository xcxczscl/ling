use std::fs;
use std::path::PathBuf;

use ling_sdk::{codes, Value};

mod common;

use common::{fixture, fresh_kernel};

fn events_path(kernel_root: &std::path::Path) -> PathBuf {
    kernel_root.join("plugins/demo-b/data/events.ndjson")
}

#[test]
fn l2_1_uninstalled_methods_return_method_missing_to_callers() {
    let (_tmp, kernel) = fresh_kernel();
    let fix_b = fixture("demo-b");
    kernel.install(&fix_b.dir).unwrap();
    let fix_a = fixture("demo-a");
    kernel.install(&fix_a.dir).unwrap();

    let pong = kernel
        .call("demo-b", "demo-b.ping_a", Value::Null)
        .expect("卸载前 demo-b 可调用 demo-a");
    assert_eq!(pong, serde_json::json!("pong"));
    assert_eq!(kernel.ledger_of("demo-a").len(), 4);

    let report = kernel.uninstall("demo-a", false).expect("卸载");
    assert_eq!(report.effects_total, 4);
    assert!(report.complete(), "{}", report.summary());
    assert!(!report.purged);
    assert!(report.summary().starts_with("卸载完整"));

    let e = kernel
        .call("demo-b", "demo-b.ping_a", Value::Null)
        .unwrap_err();
    assert_eq!(
        e.code(),
        codes::E_METHOD_MISSING,
        "卸载后 demo-b 调用 demo-a 方法返回 E_METHOD_MISSING"
    );
    let e = kernel
        .call("demo-a", "demo-a.ping", Value::Null)
        .unwrap_err();
    assert_eq!(e.code(), codes::E_METHOD_MISSING);

    assert_eq!(kernel.activated_plugins(), vec!["demo-b".to_string()]);
    assert_eq!(
        kernel.installed_plugins(),
        vec!["demo-b".to_string()],
        "非 purge 卸载后 demo-a 目录仅剩 data/，不再计入已安装"
    );
}

#[test]
fn l2_2_uninstalled_subscription_stops_receiving_events() {
    let (_tmp, kernel) = fresh_kernel();
    let fix_b = fixture("demo-b");
    kernel.install(&fix_b.dir).unwrap();
    let fix_a = fixture("demo-a");
    kernel.install(&fix_a.dir).unwrap();

    kernel
        .call(
            "demo-a",
            "demo-a.emit",
            serde_json::json!({"topic": "demo-a.before", "payload": 1}),
        )
        .expect("广播");
    let path = events_path(kernel.data_root());
    let before = fs::read_to_string(&path).expect("事件文件存在");
    assert_eq!(before.lines().count(), 1, "卸载前回调落盘 1 条");

    let report = kernel.uninstall("demo-b", false).expect("卸载");
    assert!(report.complete(), "{}", report.summary());
    assert!(report.remnant_subs.is_empty(), "无残留订阅");
    assert_eq!(report.effects_total, 6, "5 个方法 + 1 条订阅");

    kernel.emit("demo-a.after", serde_json::json!({"x": 2}));
    let after = fs::read_to_string(&path).expect("事件文件仍在（data/ 保留）");
    assert_eq!(
        before, after,
        "卸载后广播其订阅的事件，其回调不再被调用（文件未增长）"
    );
}

#[test]
fn l2_3_uninstall_file_semantics_and_reinstall() {
    let (_tmp, kernel) = fresh_kernel();
    let fix = fixture("demo-a");
    kernel.install(&fix.dir).unwrap();
    kernel.load("demo-a").unwrap();

    let dir = kernel.data_root().join("plugins/demo-a");
    let state_file = dir.join("data/state.txt");
    fs::write(&state_file, "keep-me").unwrap();

    let report = kernel.uninstall("demo-a", false).expect("卸载");
    assert!(report.complete());
    assert!(!report.purged);
    assert!(state_file.is_file(), "data/ 用户数据保留");
    assert!(!dir.join("plugin.toml").is_file(), "安装工件已移除");
    assert!(
        !dir.join(format!("demo-a{}", std::env::consts::DLL_SUFFIX))
            .is_file(),
        "动态库已移除"
    );
    assert!(kernel.installed_plugins().is_empty());

    kernel.install(&fix.dir).expect("卸载后可重新安装");
    kernel.load("demo-a").expect("重新装载");
    let pong = kernel
        .call("demo-a", "demo-a.ping", Value::Null)
        .expect("调用");
    assert_eq!(pong, serde_json::json!("pong"));
    assert!(
        state_file.is_file(),
        "重装后原 data/ 用户数据仍在（合并安装）"
    );

    let report = kernel.uninstall("demo-a", true).expect("purge 卸载");
    assert!(report.purged);
    assert!(!dir.exists(), "purge 连 data/ 一并删除");

    let e = kernel.uninstall("no-such-plugin", false).unwrap_err();
    assert!(matches!(e, ling_kernel::KernelError::NotFound(_)));
}

#[test]
fn l2_4_uninstall_reports_complete_for_clean_plugin() {
    let (_tmp, kernel) = fresh_kernel();
    let fix = fixture("demo-a");
    kernel.install(&fix.dir).unwrap();
    kernel.load("demo-a").unwrap();
    assert_eq!(kernel.ledger_of("demo-a").len(), 4);

    let report = kernel.uninstall("demo-a", false).expect("卸载");
    assert_eq!(report.effects_total, 4);
    assert!(report.complete(), "{}", report.summary());
    assert!(report.undo_failures.is_empty());
    assert!(report.remnant_methods.is_empty());
    assert!(report.remnant_subs.is_empty());
    assert!(
        report.summary().contains("卸载完整"),
        "{}",
        report.summary()
    );
    assert!(kernel.activated_plugins().is_empty());
    assert!(
        kernel.ledger_of("demo-a").is_empty(),
        "卸载后账本随记录移除"
    );
}
