//! L5 验收：真实 `ling serve` 子进程——首启自动安装三件套、界面壳可访问、
//! 命令行仍可管理，最后优雅停机。

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use ling_sdk::{json, Value};

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn ling_bin() -> PathBuf {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let status = Command::new(env!("CARGO"))
            .args([
                "build",
                "-p",
                "ling-cli",
                "-p",
                "core-ui",
                "-p",
                "core-settings",
                "-p",
                "core-plugin-manager",
            ])
            .current_dir(workspace_root())
            .status()
            .expect("构建 ling-cli 与三件套");
        assert!(status.success(), "构建失败");
    });
    let exe = std::env::current_exe().expect("test binary path");
    let profile = exe
        .parent()
        .and_then(|d| d.parent())
        .expect("target layout");
    profile.join("ling")
}

fn run_cli(home: &Path, args: &[&str]) -> (bool, String) {
    let output = Command::new(ling_bin())
        .args(args)
        .env("LING_HOME", home)
        .output()
        .expect("运行 ling");
    (
        output.status.success(),
        String::from_utf8_lossy(&output.stdout).to_string(),
    )
}

fn session_token(home: &Path) -> Option<String> {
    fs::read_to_string(home.join("session.json"))
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .and_then(|v| v["token"].as_str().map(str::to_string))
}

fn registry(home: &Path) -> Value {
    fs::read_to_string(home.join("registry.json"))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or(Value::Null)
}

fn admin_list(home: &Path) -> Option<Vec<Value>> {
    let port = registry(home)["management_port"].as_u64()? as u16;
    let token = session_token(home)?;
    let stream = TcpStream::connect(("127.0.0.1", port)).ok()?;
    let _ = stream.set_read_timeout(Some(Duration::from_secs(3)));
    let mut writer = stream.try_clone().ok()?;
    writeln!(writer, "{}", json!({"op": "auth", "token": token})).ok()?;
    let mut reader = BufReader::new(stream.try_clone().ok()?);
    let mut line = String::new();
    reader.read_line(&mut line).ok()?;
    writeln!(
        writer,
        "{}",
        json!({"id": "1", "method": "kernel.plugins.list"})
    )
    .ok()?;
    line.clear();
    reader.read_line(&mut line).ok()?;
    let response: Value = serde_json::from_str(line.trim()).ok()?;
    response["result"]["plugins"].as_array().cloned()
}

fn spawn_serve(home: &Path) -> Child {
    Command::new(ling_bin())
        .args([
            "serve",
            "--builtin-dir",
            workspace_root()
                .join("builtin-plugins")
                .display()
                .to_string()
                .as_str(),
        ])
        .env("LING_HOME", home)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("启动 ling serve")
}

#[test]
fn cli_l5_first_start_trio_and_ui() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let home = tmp.path();
    let mut child = spawn_serve(home);

    // 等三件套装好且全部激活
    let deadline = Instant::now() + Duration::from_secs(30);
    let plugins = loop {
        if let Some(list) = admin_list(home) {
            let activated: Vec<&str> = list
                .iter()
                .filter(|p| p["state"] == json!("activated"))
                .filter_map(|p| p["id"].as_str())
                .collect();
            if activated.len() == 3 {
                break list;
            }
        }
        assert!(
            Instant::now() < deadline,
            "等待三件套激活超时（registry={:?}）",
            registry(home)
        );
        std::thread::sleep(Duration::from_millis(200));
    };
    let ids: Vec<&str> = plugins.iter().filter_map(|p| p["id"].as_str()).collect();
    for expected in ["core-ui", "core-settings", "core-plugin-manager"] {
        assert!(ids.contains(&expected), "三件套含 {expected}：{ids:?}");
    }

    // 界面壳端口已登记，带口令可访问
    let ui_port = registry(home)["ui_port"].as_u64().expect("ui_port 已登记") as u16;
    let token = session_token(home).expect("token");
    let stream = TcpStream::connect(("127.0.0.1", ui_port)).expect("连界面端口");
    let _ = stream.set_read_timeout(Some(Duration::from_secs(3)));
    let mut writer = stream.try_clone().unwrap();
    write!(
        writer,
        "GET / HTTP/1.1\r\nHost: 127.0.0.1\r\nX-Ling-Token: {token}\r\nConnection: close\r\n\r\n"
    )
    .unwrap();
    let mut reader = BufReader::new(stream);
    let mut status = String::new();
    reader.read_line(&mut status).unwrap();
    assert!(status.contains("200"), "界面壳 200：{status}");

    // 命令行仍可管理（`ling plugins` 成功）
    let (ok, out) = run_cli(home, &["plugins"]);
    assert!(ok, "ling plugins 失败：{out}");
    assert!(out.contains("core-ui"), "plugins 输出：{out}");

    // 优雅停机
    let (ok, _) = run_cli(home, &["shutdown"]);
    assert!(ok, "shutdown 失败");
    let status = child.wait().expect("等待 serve 退出");
    assert!(status.success(), "serve 退出码 0");
}
