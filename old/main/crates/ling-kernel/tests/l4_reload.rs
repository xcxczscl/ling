use std::fs;
use std::net::TcpStream;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use ling_kernel::admin;
use ling_kernel::Kernel;
use ling_sdk::{codes, json, Value};

mod common;

use common::{fixture, fresh_kernel, Fixture};

fn overwrite_installed(kernel_root: &Path, fix: &Fixture, id: &str) {
    let dst = kernel_root.join("plugins").join(id);
    for entry in fs::read_dir(&fix.dir).unwrap() {
        let entry = entry.unwrap();
        fs::copy(entry.path(), dst.join(entry.file_name())).unwrap();
    }
}

fn read_line(stream: &mut TcpStream) -> String {
    use std::io::Read;
    let mut data = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        let n = stream.read(&mut byte).unwrap();
        if n == 0 || byte[0] == b'\n' {
            break;
        }
        data.push(byte[0]);
    }
    String::from_utf8_lossy(&data).to_string()
}

fn admin_roundtrip(port: u16, token: &str, request: Value) -> Value {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("连接管理端口");
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    use std::io::Write;
    writeln!(stream, "{}", json!({"op": "auth", "token": token})).unwrap();
    let auth = read_line(&mut stream);
    let auth: Value = serde_json::from_str(&auth).unwrap();
    assert_eq!(auth["ok"], true, "认证应通过：{auth}");
    writeln!(
        stream,
        "{}",
        json!({"id": 1, "method": request["method"], "params": request["params"]})
    )
    .unwrap();
    let response = read_line(&mut stream);
    serde_json::from_str(&response).unwrap()
}

#[test]
fn l4_1_reload_swaps_version_and_keeps_others_intact() {
    let (_tmp, kernel) = fresh_kernel();
    let fix_b = fixture("demo-b");
    kernel.install(&fix_b.dir).unwrap();
    let fix_a = fixture("demo-a");
    kernel.install(&fix_a.dir).unwrap();
    assert_eq!(kernel.activated_plugins().len(), 2);

    let events_before =
        fs::read_to_string(kernel.data_root().join("plugins/demo-b/data/events.ndjson"))
            .unwrap_or_default();

    overwrite_installed(kernel.data_root(), &fix_a, "demo-a");
    let report = kernel.reload("demo-a").expect("热更新成功");
    assert_eq!(report.plugin, "demo-a");
    assert_eq!(report.version, "0.1.0");
    assert!(report.downstream.is_empty(), "demo-a 无依赖方");
    assert_eq!(kernel.activated_plugins().len(), 2, "双方仍在运行");

    kernel
        .call(
            "demo-a",
            "demo-a.emit",
            serde_json::json!({"topic": "demo-a.hello", "payload": {"n": 1}}),
        )
        .expect("新版可调用");
    let events_after =
        fs::read_to_string(kernel.data_root().join("plugins/demo-b/data/events.ndjson")).unwrap();
    assert_eq!(
        events_after.lines().count(),
        events_before.lines().count() + 1,
        "demo-b 无感：其订阅照常收到新版广播"
    );

    overwrite_installed(kernel.data_root(), &fix_b, "demo-b");
    let report = kernel.reload("demo-b").expect("重载依赖方");
    assert_eq!(report.downstream, vec!["demo-a"], "下游被停用并重新激活");
    assert_eq!(kernel.activated_plugins().len(), 2);
    let pong = kernel
        .call("demo-a", "demo-a.ping", Value::Null)
        .expect("下游恢复后可调用");
    assert_eq!(pong, json!("pong"));
    let events =
        fs::read_to_string(kernel.data_root().join("plugins/demo-b/data/events.ndjson")).unwrap();
    assert_eq!(
        events.lines().count(),
        events_before.lines().count() + 1,
        "data/ 事件历史在重载间保留"
    );
}

#[test]
fn l4_2_reload_stress_100_times_without_state_bleed() {
    let (_tmp, kernel) = fresh_kernel();
    let fix_b = fixture("demo-b");
    kernel.install(&fix_b.dir).unwrap();
    let fix_a = fixture("demo-a");
    kernel.install(&fix_a.dir).unwrap();

    for i in 0..100 {
        overwrite_installed(kernel.data_root(), &fix_a, "demo-a");
        kernel
            .reload("demo-a")
            .unwrap_or_else(|e| panic!("第 {} 次 reload 失败：{e}", i + 1));
        assert_eq!(
            kernel.activated_plugins().len(),
            2,
            "第 {} 次后仍在运行",
            i + 1
        );
    }
    let pong = kernel
        .call("demo-a", "demo-a.ping", Value::Null)
        .expect("100 次热更新后方法正常");
    assert_eq!(pong, json!("pong"));
    let sum = kernel
        .call(
            "demo-a",
            "demo-a.forward",
            serde_json::json!({
                "plugin": "demo-b",
                "method": "demo-b.add",
                "args": {"a": 40, "b": 2}
            }),
        )
        .expect("跨插件调用正常");
    assert_eq!(sum, json!(42), "无状态串味");
}

#[test]
fn l4_3_reload_failure_rolls_back_old_version() {
    let (_tmp, kernel) = fresh_kernel();
    let fix_b = fixture("demo-b");
    kernel.install(&fix_b.dir).unwrap();
    let fix_a = fixture("demo-a");
    kernel.install(&fix_a.dir).unwrap();

    let so_name = format!("demo-a{}", std::env::consts::DLL_SUFFIX);
    let so_path = kernel.data_root().join("plugins/demo-a").join(&so_name);
    fs::write(&so_path, b"this is not an elf shared object").unwrap();

    let e = kernel.reload("demo-a").unwrap_err();
    assert_eq!(e.ecode(), Some(codes::E_RELOAD_FAILED));
    let message = e.to_string();
    assert!(message.contains("回滚"), "错误应说明已回滚：{message}");
    assert_eq!(
        kernel.activated_plugins().len(),
        2,
        "旧版已回滚，双方仍在运行"
    );
    let pong = kernel
        .call("demo-a", "demo-a.ping", Value::Null)
        .expect("旧版继续服务");
    assert_eq!(pong, json!("pong"));
    assert!(
        so_path.is_file(),
        "回滚后 `{}` 原件在盘（装载用的是其私有副本）",
        so_name
    );
    let restored_so = fs::read(&so_path).unwrap();
    assert!(
        restored_so.len() > 64 && !restored_so.starts_with(b"this is not"),
        "原件已从备份还原为旧版"
    );
    let all: Vec<String> = fs::read_dir(kernel.data_root().join("plugins/demo-a"))
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .collect();
    let privates: Vec<_> = all
        .iter()
        .filter(|n| {
            n.starts_with("demo-a.")
                && n.ends_with(std::env::consts::DLL_SUFFIX)
                && n.as_str() != so_name.as_str()
        })
        .cloned()
        .collect();
    assert_eq!(privates.len(), 1, "旧版从备份还原并以私有名装载：{all:?}");

    let manifest_path = kernel.data_root().join("plugins/demo-a/plugin.toml");
    let good_manifest = fs::read_to_string(&manifest_path).unwrap();
    fs::write(&manifest_path, "这不是合法 toml !!!").unwrap();
    let e = kernel.reload("demo-a").unwrap_err();
    assert_eq!(e.ecode(), Some(codes::E_MANIFEST_INVALID), "先校验新清单");
    assert_eq!(kernel.activated_plugins().len(), 2, "旧版未被动过");
    assert_eq!(
        fs::read_to_string(&manifest_path).unwrap(),
        "这不是合法 toml !!!",
        "校验失败不触碰目录（由调用方修复）"
    );
    fs::write(&manifest_path, good_manifest).unwrap();
}

#[test]
fn l4_4_admin_port_auth_and_list() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let kernel = Arc::new(Kernel::new(tmp.path().join("ling")).expect("kernel"));
    let server = admin::start(Arc::clone(&kernel), 0).expect("管理端口启动");
    assert_ne!(server.port, 0);

    let mut stream = TcpStream::connect(("127.0.0.1", server.port)).expect("连接");
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    use std::io::Write;
    writeln!(stream, "{}", json!({"op": "auth", "token": "wrong-token"})).unwrap();
    let response = read_line(&mut stream);
    let response: Value = serde_json::from_str(&response).unwrap();
    assert_eq!(response["ok"], false);
    assert_eq!(response["error"]["code"], codes::E_AUTH, "错口令被拒");
    drop(stream);

    let session: Value =
        serde_json::from_str(&fs::read_to_string(tmp.path().join("ling/session.json")).unwrap())
            .unwrap();
    let token = session["token"].as_str().unwrap();
    let registry: Value =
        serde_json::from_str(&fs::read_to_string(tmp.path().join("ling/registry.json")).unwrap())
            .unwrap();
    assert_eq!(
        registry["management_port"].as_u64(),
        Some(server.port as u64)
    );

    let fix = fixture("demo-b");
    kernel.install(&fix.dir).unwrap();
    let response = admin_roundtrip(server.port, token, json!({"method": "kernel.plugins.list"}));
    assert_eq!(response["ok"], true);
    let plugins = response["result"]["plugins"].as_array().unwrap();
    assert_eq!(plugins.len(), 1);
    assert_eq!(plugins[0]["id"], "demo-b");
    assert_eq!(plugins[0]["state"], "activated");
    assert_eq!(plugins[0]["version"], "0.1.0");

    let response = admin_roundtrip(
        server.port,
        token,
        json!({"method": "kernel.plugins.disable", "params": {"id": "demo-b"}}),
    );
    assert_eq!(response["ok"], true);
    assert!(kernel.activated_plugins().is_empty());

    let response = admin_roundtrip(
        server.port,
        token,
        json!({"method": "kernel.plugins.enable", "params": {"id": "demo-b"}}),
    );
    assert_eq!(response["ok"], true);
    assert_eq!(kernel.activated_plugins(), vec!["demo-b".to_string()]);

    overwrite_installed(kernel.data_root(), &fix, "demo-b");
    let response = admin_roundtrip(
        server.port,
        token,
        json!({"method": "kernel.plugins.reload", "params": {"id": "demo-b"}}),
    );
    assert_eq!(response["ok"], true, "{}", response);
    assert_eq!(kernel.activated_plugins(), vec!["demo-b".to_string()]);

    let response = admin_roundtrip(server.port, token, json!({"method": "kernel.shutdown"}));
    assert_eq!(response["ok"], true);
    assert_eq!(
        response["result"]["stopped"],
        json!(["demo-b"]),
        "shutdown 停用仍激活的插件"
    );
    assert!(kernel.activated_plugins().is_empty());
    server.request_shutdown();
}

#[test]
fn l4_5_shutdown_stops_in_reverse_dependency_order() {
    let (_tmp, kernel) = fresh_kernel();
    let fix_b = fixture("demo-b");
    kernel.install(&fix_b.dir).unwrap();
    let fix_a = fixture("demo-a");
    kernel.install(&fix_a.dir).unwrap();
    assert_eq!(kernel.activated_plugins().len(), 2);

    let stopped = kernel.shutdown().expect("停机");
    assert_eq!(
        stopped,
        vec!["demo-a", "demo-b"],
        "先停依赖者，后停被依赖者"
    );
    assert!(kernel.activated_plugins().is_empty());
    let e = kernel
        .call("demo-a", "demo-a.ping", Value::Null)
        .unwrap_err();
    assert_eq!(e.code(), codes::E_METHOD_MISSING);
    assert_eq!(
        kernel.installed_plugins(),
        vec!["demo-a".to_string(), "demo-b".to_string()],
        "停机不动安装工件"
    );
}
