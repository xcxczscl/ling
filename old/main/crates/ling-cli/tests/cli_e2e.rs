//! L4 验收 4：`ling` 命令行端到端——serve 前台宿主 + 经管理端口的
//! plugins/install/enable/disable/shutdown 全链路（真实子进程）。

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn ling_bin() -> PathBuf {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let status = Command::new(env!("CARGO"))
            .args(["build", "-p", "ling-cli", "-p", "demo-b"])
            .current_dir(workspace_root())
            .status()
            .expect("构建 ling-cli");
        assert!(status.success(), "构建 ling-cli / demo-b 失败");
    });
    let exe = std::env::current_exe().expect("test binary path");
    let profile = exe
        .parent()
        .and_then(|d| d.parent())
        .expect("target layout");
    profile.join("ling")
}

fn cdylib_for(id: &str) -> PathBuf {
    let exe = std::env::current_exe().expect("test binary path");
    let profile = exe
        .parent()
        .and_then(|d| d.parent())
        .expect("target layout");
    let lib = format!(
        "{}{}{}",
        std::env::consts::DLL_PREFIX,
        id.replace('-', "_"),
        std::env::consts::DLL_SUFFIX
    );
    let path = profile.join(lib);
    assert!(path.is_file(), "示例插件动态库未构建：{}", path.display());
    path
}

fn install_source(id: &str) -> PathBuf {
    let tmp = std::env::temp_dir().join(format!("ling-cli-src-{}-{id}", std::process::id()));
    let _ = fs::remove_dir_all(&tmp);
    fs::create_dir_all(&tmp).unwrap();
    let src_root = workspace_root().join("plugins").join(id);
    fs::copy(src_root.join("plugin.toml"), tmp.join("plugin.toml")).unwrap();
    fs::copy(
        cdylib_for(id),
        tmp.join(format!("{id}{}", std::env::consts::DLL_SUFFIX)),
    )
    .unwrap();
    tmp
}

fn wait_for_file(path: &Path, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if path.is_file() {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("等待 {} 超时", path.display());
}

fn run_cli(home: &Path, args: &[&str]) -> (bool, String) {
    let output = Command::new(ling_bin())
        .args(args)
        .env("LING_HOME", home)
        .output()
        .expect("运行 ling");
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    (output.status.success(), format!("{stdout}{stderr}"))
}

#[test]
fn cli_e2e_serve_manage_shutdown() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let home = tmp.path().join("ling-home");
    fs::create_dir_all(&home).unwrap();

    let mut child = Command::new(ling_bin())
        .arg("serve")
        .env("LING_HOME", &home)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("启动 ling serve");

    wait_for_file(&home.join("registry.json"), Duration::from_secs(10));
    wait_for_file(&home.join("session.json"), Duration::from_secs(10));

    let (ok, out) = run_cli(&home, &["plugins"]);
    assert!(ok, "plugins 失败：{out}");

    let source = install_source("demo-b");
    let (ok, out) = run_cli(&home, &["install", source.to_str().unwrap()]);
    assert!(ok, "install 失败：{out}");
    assert!(out.contains("demo-b"), "install 输出应含编号：{out}");

    let (ok, out) = run_cli(&home, &["plugins"]);
    assert!(ok, "plugins 失败：{out}");
    assert!(out.contains("demo-b"), "{out}");
    assert!(out.contains("activated"), "安装后自动激活：{out}");

    let (ok, out) = run_cli(&home, &["disable", "demo-b"]);
    assert!(ok, "disable 失败：{out}");
    let (ok, out) = run_cli(&home, &["plugins"]);
    assert!(ok);
    assert!(out.contains("disabled"), "停用状态可见：{out}");

    let (ok, out) = run_cli(&home, &["enable", "demo-b"]);
    assert!(ok, "enable 失败：{out}");
    let (ok, out) = run_cli(&home, &["plugins"]);
    assert!(ok);
    assert!(out.contains("activated"), "重新启用：{out}");

    // 热更新：重投递产物（装载后 {id}.so 路径已被释放，可安全覆盖）再 reload。
    for entry in fs::read_dir(&source).unwrap() {
        let entry = entry.unwrap();
        fs::copy(
            entry.path(),
            home.join("plugins/demo-b").join(entry.file_name()),
        )
        .unwrap();
    }
    let (ok, out) = run_cli(&home, &["reload", "demo-b"]);
    assert!(ok, "reload 失败：{out}");

    let (ok, out) = run_cli(&home, &["shutdown"]);
    assert!(ok, "shutdown 失败：{out}");

    let status = child.wait().expect("等待 serve 退出");
    assert!(status.success(), "serve 应以 0 退出");
}
