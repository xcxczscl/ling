use std::fs;
use std::io::Write;
use std::path::Path;

use ling_kernel::KernelError;
use ling_sdk::{codes, Value};

mod common;

use common::{fixture, fresh_kernel};

fn write_manifest(dir: &Path, text: &str) {
    fs::create_dir_all(dir).unwrap();
    fs::write(dir.join("plugin.toml"), text).unwrap();
}

const BASE_MANIFEST: &str = r#"
[plugin]
id = "demo-a"
version = "0.1.0"
api = "1"

[requires]
capabilities = ["fs"]
"#;

fn err_code(e: &KernelError) -> &str {
    e.ecode().unwrap_or("<no-code>")
}

#[test]
fn l1_1_install_succeeds_and_manifest_validation_works() {
    let (_tmp, kernel) = fresh_kernel();

    let fix = fixture("demo-a");
    let id = kernel.install(&fix.dir).expect("合法插件安装成功");
    assert_eq!(id, "demo-a");
    assert_eq!(kernel.installed_plugins(), vec!["demo-a"]);
    assert!(matches!(
        kernel.install(&fix.dir),
        Err(KernelError::AlreadyInstalled(_))
    ));

    let bad_id_dir = _tmp.path().join("bad-id");
    write_manifest(&bad_id_dir, &BASE_MANIFEST.replace("demo-a", "Demo_A"));
    let e = kernel.install(&bad_id_dir).unwrap_err();
    assert_eq!(err_code(&e), codes::E_MANIFEST_INVALID);

    let bad_api_dir = _tmp.path().join("bad-api");
    write_manifest(
        &bad_api_dir,
        &BASE_MANIFEST
            .replace("demo-a", "bad-api")
            .replace(r#"api = "1""#, r#"api = "2""#),
    );
    let e = kernel.install(&bad_api_dir).unwrap_err();
    assert_eq!(err_code(&e), codes::E_API_MISMATCH);

    let bad_cap_dir = _tmp.path().join("bad-cap");
    write_manifest(
        &bad_cap_dir,
        &BASE_MANIFEST
            .replace("demo-a", "bad-cap")
            .replace("fs", "telepathy"),
    );
    let e = kernel.install(&bad_cap_dir).unwrap_err();
    assert_eq!(err_code(&e), codes::E_CAPABILITY_UNKNOWN);

    let no_manifest_dir = _tmp.path().join("no-manifest");
    fs::create_dir_all(&no_manifest_dir).unwrap();
    let e = kernel.install(&no_manifest_dir).unwrap_err();
    assert_eq!(err_code(&e), codes::E_MANIFEST_INVALID);

    let zip_path = _tmp.path().join("ok.zip");
    write_zip(
        &zip_path,
        &[("plugin.toml", &BASE_MANIFEST.replace("demo-a", "zip-ok"))],
    );
    let id = kernel.install(&zip_path).expect("zip 安装成功");
    assert_eq!(id, "zip-ok");

    let no_root = _tmp.path().join("noroot.zip");
    write_zip(&no_root, &[("inner/plugin.toml", BASE_MANIFEST)]);
    let e = kernel.install(&no_root).unwrap_err();
    assert_eq!(err_code(&e), codes::E_MANIFEST_INVALID);

    let evil = _tmp.path().join("evil.zip");
    write_zip(
        &evil,
        &[
            ("plugin.toml", &BASE_MANIFEST.replace("demo-a", "evil-zip")),
            ("../escape.txt", "x"),
        ],
    );
    let e = kernel.install(&evil).unwrap_err();
    assert_eq!(err_code(&e), codes::E_MANIFEST_INVALID);
}

#[test]
fn l1_2_method_routing_between_plugins() {
    let (_tmp, kernel) = fresh_kernel();

    let fix_b = fixture("demo-b");
    kernel.install(&fix_b.dir).unwrap();
    let fix_a = fixture("demo-a");
    kernel.install(&fix_a.dir).unwrap();
    assert_eq!(
        kernel.activated_plugins(),
        vec!["demo-a".to_string(), "demo-b".to_string()]
    );
    assert_eq!(
        kernel.ledger_of("demo-a").len(),
        4,
        "demo-a 注册 4 个方法，账本 4 条"
    );

    let result = kernel
        .call(
            "demo-a",
            "demo-a.forward",
            serde_json::json!({
                "plugin": "demo-b",
                "method": "demo-b.add",
                "args": {"a": 40, "b": 2}
            }),
        )
        .expect("经路由调用成功");
    assert_eq!(result, serde_json::json!(42));

    let direct = kernel
        .call("demo-b", "demo-b.add", serde_json::json!({"a": 1, "b": 2}))
        .expect("直调成功");
    assert_eq!(direct, serde_json::json!(3));

    let e = kernel
        .call("demo-b", "demo-b.nope", Value::Null)
        .unwrap_err();
    assert_eq!(e.code(), codes::E_METHOD_MISSING);
}

#[test]
fn l1_3_event_broadcast_reaches_subscriber() {
    let (_tmp, kernel) = fresh_kernel();

    let fix_b = fixture("demo-b");
    kernel.install(&fix_b.dir).unwrap();
    let fix_a = fixture("demo-a");
    kernel.install(&fix_a.dir).unwrap();

    kernel
        .call(
            "demo-a",
            "demo-a.emit",
            serde_json::json!({
                "topic": "demo-a.hello",
                "payload": {"x": 1, "text": "hi"}
            }),
        )
        .expect("广播成功");

    let events = kernel
        .call("demo-b", "demo-b.read_events", Value::Null)
        .expect("读取事件");
    assert_eq!(events, serde_json::json!([{"x": 1, "text": "hi"}]));

    kernel
        .call(
            "demo-a",
            "demo-a.emit",
            serde_json::json!({"topic": "demo-a.second", "payload": 2}),
        )
        .expect("广播成功");
    let events = kernel
        .call("demo-b", "demo-b.read_events", Value::Null)
        .expect("读取事件");
    assert_eq!(events.as_array().map(|a| a.len()), Some(2));
}

#[test]
fn l1_4_capability_gate_denies_undeclared() {
    let (_tmp, kernel) = fresh_kernel();

    let fix_b = fixture("demo-b");
    kernel.install(&fix_b.dir).unwrap();
    let fix_a = fixture("demo-a");
    kernel.install(&fix_a.dir).unwrap();

    kernel
        .revoke_capability("demo-b", ling_sdk::Capability::Fs)
        .unwrap();
    let denied = kernel
        .call(
            "demo-a",
            "demo-a.forward",
            serde_json::json!({
                "plugin": "demo-b",
                "method": "demo-b.dup.delete",
                "args": {"placement_ids": [1]}
            }),
        )
        .unwrap_err();
    assert_eq!(
        denied.code(),
        codes::E_CAPABILITY_DENIED,
        "撤销 fs 后调用带 fs 标注的方法被拒"
    );

    kernel
        .grant_capability("demo-b", ling_sdk::Capability::Fs)
        .unwrap();
    let ok = kernel
        .call(
            "demo-a",
            "demo-a.forward",
            serde_json::json!({
                "plugin": "demo-b",
                "method": "demo-b.dup.delete",
                "args": {"placement_ids": [1, 2]}
            }),
        )
        .expect("恢复能力后调用成功");
    assert_eq!(ok, serde_json::json!({"deleted": 2}));

    kernel
        .revoke_capability("demo-b", ling_sdk::Capability::Fs)
        .unwrap();

    let reg = kernel
        .call(
            "demo-a",
            "demo-a.probe",
            serde_json::json!({"kind": "register", "name": "demo-a.netsvc", "requires": "network"}),
        )
        .unwrap_err();
    assert_eq!(
        reg.code(),
        codes::E_CAPABILITY_DENIED,
        "注册期：清单未声明 network 的插件注册 requires=network 被拒"
    );
}

#[test]
fn l1_5_namespace_violations_rejected() {
    let (_tmp, kernel) = fresh_kernel();

    let fix_b = fixture("demo-b");
    kernel.install(&fix_b.dir).unwrap();
    let fix_a = fixture("demo-a");
    kernel.install(&fix_a.dir).unwrap();

    let reg = kernel
        .call(
            "demo-a",
            "demo-a.probe",
            serde_json::json!({"kind": "register", "name": "evil.method"}),
        )
        .unwrap_err();
    assert_eq!(reg.code(), codes::E_NAMESPACE_VIOLATION);

    let emit = kernel
        .call(
            "demo-a",
            "demo-a.probe",
            serde_json::json!({"kind": "emit", "topic": "stolen.topic"}),
        )
        .unwrap_err();
    assert_eq!(emit.code(), codes::E_NAMESPACE_VIOLATION);

    let violations = kernel.namespace_violations();
    assert!(violations
        .iter()
        .any(|v| v.kind == "method" && v.name == "evil.method"));
    assert!(violations
        .iter()
        .any(|v| v.kind == "topic" && v.name == "stolen.topic"));

    let events = kernel
        .call("demo-b", "demo-b.read_events", Value::Null)
        .expect("读取事件");
    assert_eq!(events, serde_json::json!([]), "越权广播未投递");

    let ok = kernel
        .call(
            "demo-a",
            "demo-a.probe",
            serde_json::json!({"kind": "register", "name": "demo-a.fine"}),
        )
        .expect("探针执行");
    assert_eq!(ok, serde_json::json!("ok"));
}

fn write_zip(path: &Path, files: &[(&str, &str)]) {
    let file = fs::File::create(path).unwrap();
    let mut zip = zip::ZipWriter::new(file);
    let opts = zip::write::SimpleFileOptions::default();
    for (name, content) in files {
        zip.start_file(*name, opts).unwrap();
        zip.write_all(content.as_bytes()).unwrap();
    }
    zip.finish().unwrap();
}
