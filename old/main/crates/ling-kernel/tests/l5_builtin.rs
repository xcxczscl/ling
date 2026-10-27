//! L5 验收（`docs/07` §2）：内置插件三件套首启自动安装、界面壳 /rpc 网关、
//! 三件套可卸载、可选内置插件按需安装、插件贡献页面、设置 schema 校验。

mod common;

use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use ling_kernel::admin::{self, AdminServer};
use ling_kernel::builtin;
use ling_kernel::Kernel;
use ling_sdk::{json, Value};

struct L5 {
    _tmp: tempfile::TempDir,
    kernel: Arc<Kernel>,
    _server: AdminServer,
    token: String,
    admin_port: u16,
    ui_port: u16,
}

fn workspace() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn builtin_dir() -> PathBuf {
    common::ensure_cdylibs();
    let dir = workspace().join("builtin-plugins");
    let catalog = builtin::discover(&dir);
    assert!(
        !catalog.is_empty(),
        "内置插件目录为空（先 cargo build 构建 builtin-plugins）：{}",
        dir.display()
    );
    dir
}

fn setup() -> L5 {
    let tmp = tempfile::tempdir().expect("tempdir");
    let kernel = Arc::new(Kernel::new(tmp.path()).expect("kernel"));
    let server = admin::start(kernel.clone(), 0).expect("admin");
    let catalog = builtin::discover(&builtin_dir());
    kernel.set_builtin_catalog(catalog);
    let installed = kernel.ensure_default_trio().expect("首启三件套");
    assert_eq!(
        installed,
        vec!["core-ui", "core-settings", "core-plugin-manager"]
    );

    let token = fs::read_to_string(tmp.path().join("session.json"))
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .and_then(|v| v["token"].as_str().map(str::to_string))
        .expect("session.json 口令");
    let ui_port = fs::read_to_string(tmp.path().join("registry.json"))
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .and_then(|v| v["ui_port"].as_u64())
        .expect("registry.json ui_port") as u16;

    let admin_port = server.port;
    L5 {
        _tmp: tmp,
        kernel,
        _server: server,
        token,
        admin_port,
        ui_port,
    }
}

fn http(port: u16, request_head: &str, body: Option<&[u8]>) -> (u16, String) {
    let stream = TcpStream::connect(("127.0.0.1", port)).expect("连接界面端口");
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let mut writer = stream.try_clone().unwrap();
    writer.write_all(request_head.as_bytes()).unwrap();
    if let Some(b) = body {
        writer.write_all(b).unwrap();
    }
    writer.flush().unwrap();
    let mut reader = BufReader::new(stream);
    let mut status_line = String::new();
    reader.read_line(&mut status_line).unwrap();
    let status: u16 = status_line
        .split_whitespace()
        .nth(1)
        .unwrap()
        .parse()
        .unwrap();
    let mut content_length = 0usize;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).unwrap() == 0 || line == "\r\n" || line == "\n" {
            break;
        }
        if let Some((k, v)) = line.split_once(':') {
            if k.eq_ignore_ascii_case("content-length") {
                content_length = v.trim().parse().unwrap_or(0);
            }
        }
    }
    let mut body = vec![0u8; content_length];
    if content_length > 0 {
        reader.read_exact(&mut body).unwrap();
    }
    (status, String::from_utf8_lossy(&body).to_string())
}

fn ui_get(l5: &L5, path: &str) -> (u16, String) {
    http(
        l5.ui_port,
        &format!(
            "GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nX-Ling-Token: {}\r\nConnection: close\r\n\r\n",
            l5.token
        ),
        None,
    )
}

fn ui_rpc(l5: &L5, plugin: &str, method: &str, params: Value) -> Value {
    let body = json!({"id": "1", "plugin": plugin, "method": method, "params": params}).to_string();
    let head = format!(
        "POST /rpc HTTP/1.1\r\nHost: 127.0.0.1\r\nX-Ling-Token: {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        l5.token,
        body.len()
    );
    let (status, text) = http(l5.ui_port, &head, Some(body.as_bytes()));
    assert_eq!(status, 200, "/rpc HTTP 状态");
    let response: Value = serde_json::from_str(&text).expect("/rpc JSON 应答");
    assert_eq!(response["ok"], json!(true), "/rpc 业务成功：{response}");
    response["result"].clone()
}

fn wait_until(timeout: Duration, mut cond: impl FnMut() -> bool, what: &str) {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if cond() {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("等待超时：{what}");
}

#[test]
fn l5_1_first_start_auto_installs_trio() {
    let l5 = setup();
    let activated: Vec<String> = l5.kernel.activated_plugins();
    for id in builtin::DEFAULT_TRIO {
        assert!(
            activated.contains(&id.to_string()),
            "{id} 应已激活：{activated:?}"
        );
    }
}

#[test]
fn l5_2_ui_shell_rpc_and_auth() {
    let l5 = setup();
    let (status, body) = ui_get(&l5, "/");
    assert_eq!(status, 200, "界面壳应可访问");
    assert!(body.contains("ling"), "界面壳含品牌标识");

    // 无口令 → 401（06 §4.1 会话口令校验）
    let (status, _) = http(
        l5.ui_port,
        "GET / HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n",
        None,
    );
    assert_eq!(status, 401, "无口令请求应被拒");

    // /rpc 经内核路由到 core-plugin-manager（再经管理端口回内核）
    let list = ui_rpc(
        &l5,
        "core-plugin-manager",
        "core-plugin-manager.list",
        json!({}),
    );
    let plugins = list["plugins"].as_array().expect("plugins 数组");
    assert_eq!(plugins.len(), 3, "三件套列表：{plugins:?}");
}

#[test]
fn l5_3_trio_all_uninstallable_host_survives() {
    let l5 = setup();
    for id in ["core-ui", "core-settings", "core-plugin-manager"] {
        l5.kernel
            .uninstall(id, false)
            .unwrap_or_else(|e| panic!("卸载 {id} 失败：{e}"));
    }
    assert!(l5.kernel.activated_plugins().is_empty());
    // 宿主不崩：管理端口仍可服务
    let status = admin_roundtrip(&l5, json!({"method": "kernel.plugins.list"}));
    assert_eq!(status["ok"], json!(true), "卸载三件套后管理端口仍可用");
}

fn admin_roundtrip(l5: &L5, request: Value) -> Value {
    let stream = TcpStream::connect(("127.0.0.1", l5.admin_port)).expect("连管理端口");
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let mut writer = stream.try_clone().unwrap();
    writeln!(writer, "{}", json!({"op": "auth", "token": l5.token})).unwrap();
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut line = String::new();
    reader.read_line(&mut line).unwrap();
    writeln!(writer, "{}", request).unwrap();
    line.clear();
    reader.read_line(&mut line).unwrap();
    serde_json::from_str(line.trim()).expect("管理端口应答")
}

#[test]
fn l5_4_optional_builtins_available_and_installable() {
    let l5 = setup();
    let available = l5.kernel.builtin_available();
    let ids: Vec<&str> = available.iter().filter_map(|v| v["id"].as_str()).collect();
    assert!(ids.contains(&"core-job"), "可安装列表含 core-job：{ids:?}");
    assert!(ids.contains(&"core-log"), "可安装列表含 core-log：{ids:?}");
    assert!(!ids.contains(&"core-ui"), "已装的不出现在可安装列表");

    l5.kernel
        .install_builtin("core-job")
        .expect("按需安装 core-job");
    assert!(l5
        .kernel
        .activated_plugins()
        .contains(&"core-job".to_string()));
    let jobs = l5
        .kernel
        .call("core-job", "core-job.list", json!({}))
        .expect("core-job.list 可用");
    assert_eq!(jobs["jobs"], json!([]));

    // core-job 的作业页贡献已挂进 core-ui
    let list = ui_rpc(&l5, "core-ui", "core-ui.ui.list", json!({}));
    let paths: Vec<&str> = list["contributions"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|c| c["path"].as_str())
        .collect();
    assert!(paths.contains(&"/jobs"), "作业页已贡献：{paths:?}");
}

#[test]
fn l5_5_plugin_contributed_page_and_rpc() {
    let l5 = setup();
    let fix_b = common::fixture("demo-b");
    let fix_a = common::fixture("demo-a");
    l5.kernel.install(&fix_b.dir).expect("装 demo-b");
    l5.kernel.install(&fix_a.dir).expect("装 demo-a");
    assert!(l5
        .kernel
        .activated_plugins()
        .contains(&"demo-a".to_string()));

    // demo-a 贡献的测试页可打开
    wait_until(
        Duration::from_secs(3),
        || ui_get(&l5, "/demo-a/index.html").0 == 200,
        "demo-a 测试页",
    );
    let (status, body) = ui_get(&l5, "/demo-a/index.html");
    assert_eq!(status, 200);
    assert!(body.contains("示例 A"), "页面内容：{body}");

    // 页面上的"调用后端方法"链路：/rpc → demo-a.ping
    let pong = ui_rpc(&l5, "demo-a", "demo-a.ping", json!({}));
    assert_eq!(pong, json!("pong"));

    // 停用 demo-a → core-ui 自动撤下其贡献
    l5.kernel.disable("demo-a").expect("停用 demo-a");
    wait_until(
        Duration::from_secs(3),
        || {
            let list = l5
                .kernel
                .call("core-ui", "core-ui.ui.list", json!({}))
                .expect("ui.list");
            !list["contributions"]
                .as_array()
                .unwrap()
                .iter()
                .any(|c| c["plugin"] == json!("demo-a"))
        },
        "demo-a 贡献被自动撤下",
    );
}

#[test]
fn l5_6_settings_schema_validation() {
    let l5 = setup();
    // 以 resources-manager 的设置为场景（rm 插件落地前用同名键验证 schema 机制）
    l5.kernel
        .call(
            "core-settings",
            "core-settings.define",
            json!({
                "plugin": "resources-manager",
                "key": "scan.timeout",
                "schema": {"type": "number", "default": 30, "min": 1, "max": 3600}
            }),
        )
        .expect("声明 schema");

    l5.kernel
        .call(
            "core-settings",
            "core-settings.set",
            json!({"plugin": "resources-manager", "key": "scan.timeout", "value": 10}),
        )
        .expect("合法值可写入");

    let err = l5
        .kernel
        .call(
            "core-settings",
            "core-settings.set",
            json!({"plugin": "resources-manager", "key": "scan.timeout", "value": 9999}),
        )
        .expect_err("超上限应被拒");
    assert_eq!(err.code(), "core-settings.schema", "schema 校验生效：{err}");

    let got = l5
        .kernel
        .call(
            "core-settings",
            "core-settings.get",
            json!({"plugin": "resources-manager", "key": "scan.timeout"}),
        )
        .expect("读回");
    assert_eq!(got, json!(10));

    // 设置页贡献在核心三件套下可见
    let list = ui_rpc(&l5, "core-ui", "core-ui.ui.list", json!({}));
    let paths: Vec<&str> = list["contributions"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|c| c["path"].as_str())
        .collect();
    assert!(paths.contains(&"/settings"), "设置页已贡献：{paths:?}");
}
