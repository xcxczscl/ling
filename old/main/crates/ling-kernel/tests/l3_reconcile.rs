use std::fs;
use std::sync::{Arc, Mutex};

use ling_sdk::{codes, Value};

mod common;

use common::{fixture, fixture_with_version, fresh_kernel, manifest_only_fixture};

#[test]
fn l3_1_dependency_satisfied_activates_without_manual_load() {
    let (_tmp, kernel) = fresh_kernel();

    let fix_a = fixture("demo-a");
    kernel.install(&fix_a.dir).unwrap();
    assert!(
        kernel.activated_plugins().is_empty(),
        "依赖未满足 → 待激活，不自动装载"
    );
    assert_eq!(kernel.installed_plugins(), vec!["demo-a"]);
    let e = kernel
        .call("demo-a", "demo-a.ping", Value::Null)
        .unwrap_err();
    assert_eq!(e.code(), codes::E_METHOD_MISSING, "待激活插件方法不可调用");

    let fix_b = fixture("demo-b");
    kernel.install(&fix_b.dir).unwrap();
    assert_eq!(
        kernel.activated_plugins(),
        vec!["demo-a".to_string(), "demo-b".to_string()],
        "安装依赖方后自动激活，无需重启或手动装载"
    );
    let pong = kernel
        .call("demo-a", "demo-a.ping", Value::Null)
        .expect("自动激活后可调用");
    assert_eq!(pong, serde_json::json!("pong"));
}

#[test]
fn l3_2_uninstalling_dependency_cascades_and_reinstall_recovers() {
    let (_tmp, kernel) = fresh_kernel();
    let fix_b = fixture("demo-b");
    kernel.install(&fix_b.dir).unwrap();
    let fix_a = fixture("demo-a");
    kernel.install(&fix_a.dir).unwrap();
    assert_eq!(kernel.activated_plugins().len(), 2);

    let report = kernel.uninstall("demo-b", false).expect("卸载依赖方");
    assert!(report.complete(), "{}", report.summary());
    assert!(
        kernel.activated_plugins().is_empty(),
        "依赖方卸载后其激活中的依赖方（demo-a）自动停用"
    );
    let e = kernel
        .call("demo-a", "demo-a.ping", Value::Null)
        .unwrap_err();
    assert_eq!(e.code(), codes::E_METHOD_MISSING, "停用后方法消失");
    assert_eq!(
        kernel.installed_plugins(),
        vec!["demo-a".to_string()],
        "被级联停用的 demo-a 工件保留"
    );

    kernel.install(&fix_b.dir).expect("重装依赖方");
    assert_eq!(
        kernel.activated_plugins(),
        vec!["demo-a".to_string(), "demo-b".to_string()],
        "重装依赖方后级联停用的插件自动恢复"
    );
}

#[test]
fn l3_3_dependency_cycle_rejected_with_path() {
    let (_tmp, kernel) = fresh_kernel();
    let cyc_a = manifest_only_fixture("cyc-a", &["cyc-b@0.1.0"]);
    let cyc_b = manifest_only_fixture("cyc-b", &["cyc-a@0.1.0"]);

    kernel.install(&cyc_a.path().join("cyc-a")).unwrap();
    let e = kernel.install(&cyc_b.path().join("cyc-b")).unwrap_err();
    assert_eq!(e.ecode(), Some(codes::E_DEPENDENCY_CYCLE));
    let message = e.to_string();
    assert!(
        message.contains("cyc-a") && message.contains("cyc-b") && message.contains("→"),
        "环错误应给出路径：{message}"
    );
    assert_eq!(
        kernel.installed_plugins(),
        vec!["cyc-a".to_string()],
        "构成环的安装被拒绝，刚拷贝的工件已移除"
    );
}

#[test]
fn l3_4_version_mismatch_pending_until_compatible() {
    let (_tmp, kernel) = fresh_kernel();

    let fix_b020 = fixture_with_version("demo-b", "0.2.0");
    kernel.install(&fix_b020.dir).unwrap();
    let fix_a = fixture("demo-a");
    kernel.install(&fix_a.dir).unwrap();
    assert_eq!(
        kernel.activated_plugins(),
        vec!["demo-b".to_string()],
        "demo-a 需要 demo-b@0.1.0，0.2.0 不满足 → 待激活"
    );
    let e = kernel
        .call("demo-a", "demo-a.ping", Value::Null)
        .unwrap_err();
    assert_eq!(e.code(), codes::E_METHOD_MISSING);

    kernel.uninstall("demo-b", false).unwrap();
    let fix_b = fixture("demo-b");
    kernel.install(&fix_b.dir).unwrap();
    assert_eq!(
        kernel.activated_plugins(),
        vec!["demo-a".to_string(), "demo-b".to_string()],
        "安装兼容版本后自动激活"
    );
}

#[test]
fn l3_5_reconcile_broadcasts_plan_steps() {
    let (_tmp, kernel) = fresh_kernel();
    let recorded: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&recorded);
    kernel.subscribe(
        "kernel.plan.step",
        Box::new(move |v| {
            sink.lock().unwrap().push(v);
        }),
    );

    let fix_a = fixture("demo-a");
    kernel.install(&fix_a.dir).unwrap();
    assert!(recorded.lock().unwrap().is_empty(), "待激活场景无步骤事件");

    let fix_b = fixture("demo-b");
    kernel.install(&fix_b.dir).unwrap();
    let steps = recorded.lock().unwrap().clone();
    assert_eq!(steps.len(), 2, "两步激活各广播一次");
    assert_eq!(steps[0]["action"], "activate");
    assert_eq!(steps[0]["plugin"], "demo-b");
    assert_eq!(steps[0]["step"], 1);
    assert_eq!(steps[0]["ok"], true);
    assert_eq!(steps[1]["action"], "activate");
    assert_eq!(steps[1]["plugin"], "demo-a");
    assert_eq!(steps[1]["step"], 2);
    assert_eq!(steps[1]["ok"], true);
    let plan_id = steps[0]["plan_id"].as_str().expect("plan_id 为字符串");
    assert_eq!(plan_id, steps[1]["plan_id"], "同一次计划");
    let (date, seq) = plan_id.split_once('-').expect("plan_id 形如 YYYYMMDD-NNN");
    assert_eq!(date.len(), 8);
    assert_eq!(seq.len(), 3);

    let report = kernel.last_reconcile().expect("留存调和报告");
    assert!(report.ok());
    assert_eq!(report.steps.len(), 2);
    assert_eq!(report.plan_id, plan_id);
}

#[test]
fn l3_6_disable_enable_toggles_activation_and_persists() {
    let (_tmp, kernel) = fresh_kernel();
    let fix_b = fixture("demo-b");
    kernel.install(&fix_b.dir).unwrap();
    let fix_a = fixture("demo-a");
    kernel.install(&fix_a.dir).unwrap();
    assert_eq!(kernel.activated_plugins().len(), 2);

    kernel.disable("demo-b").expect("停用");
    assert!(
        kernel.activated_plugins().is_empty(),
        "停用依赖方后其依赖方一并停用"
    );
    let e = kernel
        .call("demo-a", "demo-a.ping", Value::Null)
        .unwrap_err();
    assert_eq!(e.code(), codes::E_METHOD_MISSING);
    assert_eq!(kernel.installed_plugins().len(), 2, "停用不动工件");

    let registry_text = fs::read_to_string(kernel.data_root().join("registry.json"))
        .expect("registry.json 已持久化");
    let registry: serde_json::Value = serde_json::from_str(&registry_text).unwrap();
    assert_eq!(registry["disabled"], serde_json::json!(["demo-b"]));

    kernel.enable("demo-b").expect("启用");
    assert_eq!(
        kernel.activated_plugins(),
        vec!["demo-a".to_string(), "demo-b".to_string()],
        "启用后依赖齐备自动激活"
    );
    let registry_text = fs::read_to_string(kernel.data_root().join("registry.json")).unwrap();
    let registry: serde_json::Value = serde_json::from_str(&registry_text).unwrap();
    let disabled = registry
        .get("disabled")
        .cloned()
        .unwrap_or(serde_json::json!([]));
    assert_eq!(disabled, serde_json::json!([]), "启用后停用名单已清空");

    let e = kernel.disable("no-such-plugin").unwrap_err();
    assert!(matches!(e, ling_kernel::KernelError::NotFound(_)));
}
