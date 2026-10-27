//! L6 验收（`docs/07` §2）：Python 侧车可装载/注册/被调用、崩溃注销与
//! on-crash 自动重启、热更新（换进程）、Rust 间调用开销不受影响。

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ling_kernel::Kernel;
use ling_sdk::{json, Value};

struct Fixture {
    dir: PathBuf,
    _tmp: tempfile::TempDir,
}

fn sidecar_source() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugins/demo-sidecar")
}

fn sidecar_fixture() -> Fixture {
    let tmp = tempfile::tempdir().expect("tempdir");
    let dir = tmp.path().join("demo-sidecar");
    copy_dir(&sidecar_source(), &dir);
    let script = dir.join("sidecar/demo_sidecar.py");
    // 仅在 Unix 上给脚本加可执行位（Windows 上无需，且 `PermissionsExt` 不存在）。
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&script).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&script, perms).unwrap();
    }
    Fixture { dir, _tmp: tmp }
}

fn copy_dir(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap().flatten() {
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), &target).unwrap();
        }
    }
}

fn setup() -> (tempfile::TempDir, Arc<Kernel>) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let kernel = Arc::new(Kernel::new(tmp.path()).expect("kernel"));
    (tmp, kernel)
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

fn install_demo_sidecar(kernel: &Kernel) -> Fixture {
    let fix = sidecar_fixture();
    kernel.install(&fix.dir).expect("安装 demo-sidecar");
    assert!(
        kernel
            .activated_plugins()
            .contains(&"demo-sidecar".to_string()),
        "侧车应已激活：{:?}",
        kernel.activated_plugins()
    );
    // 侧车注册方法在 activate 之后异步到达——等首批方法就位。
    wait_until(
        Duration::from_secs(5),
        || {
            kernel
                .call("demo-sidecar", "demo-sidecar.ping", json!({}))
                .is_ok()
        },
        "侧车方法注册完成",
    );
    fix
}

#[test]
fn l6_1_python_sidecar_registers_and_is_callable() {
    let (_tmp, kernel) = setup();
    let _fix = install_demo_sidecar(&kernel);

    // Rust 侧 → 侧车方法（控制面 invoke 往返）
    let pong = kernel
        .call("demo-sidecar", "demo-sidecar.ping", json!({}))
        .expect("ping 可调");
    assert_eq!(pong, json!("pong from python"));

    // 侧车 emit → 内核订阅方收到
    let seen: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(Vec::new()));
    let seen_cb = Arc::clone(&seen);
    kernel.subscribe(
        "demo-sidecar.*",
        Box::new(move |payload| {
            seen_cb.lock().unwrap().push(payload);
        }),
    );
    kernel
        .call("demo-sidecar", "demo-sidecar.announce", json!({}))
        .expect("announce 可调");
    wait_until(
        Duration::from_secs(3),
        || {
            seen.lock()
                .unwrap()
                .iter()
                .any(|p| p["by"] == json!("python"))
        },
        "侧车广播到达订阅方",
    );

    // 侧车 subscribe → 内核 emit → 侧车收到（event 通路）
    kernel.emit("demo-sidecar.note", json!({"hello": "sidecar"}));
    let event = kernel
        .call("demo-sidecar", "demo-sidecar.last_event", json!({}))
        .expect("last_event 可调");
    assert_eq!(event["topic"], json!("demo-sidecar.note"));
    assert_eq!(event["payload"]["hello"], json!("sidecar"));
}

#[test]
fn l6_2_crash_unregisters_effects_and_restarts() {
    let (_tmp, kernel) = setup();
    let _fix = install_demo_sidecar(&kernel);

    let states: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let states_cb = Arc::clone(&states);
    kernel.subscribe(
        "kernel.plugin.state",
        Box::new(move |payload| {
            if payload["plugin"] == json!("demo-sidecar") {
                states_cb
                    .lock()
                    .unwrap()
                    .push(payload["state"].as_str().unwrap_or("").to_string());
            }
        }),
    );

    let _ = kernel
        .call("demo-sidecar", "demo-sidecar.crash", json!({}))
        .expect("crash 方法本身成功返回");

    // 崩溃：广播 crashed + 注销效应（方法表清空）
    wait_until(
        Duration::from_secs(5),
        || states.lock().unwrap().contains(&"crashed".to_string()),
        "kernel.plugin.state crashed",
    );

    // restart=on-crash：自动重启，方法恢复可用
    wait_until(
        Duration::from_secs(10),
        || {
            kernel
                .call("demo-sidecar", "demo-sidecar.ping", json!({}))
                .is_ok()
        },
        "on-crash 自动重启后 ping 恢复",
    );
    let states_now = states.lock().unwrap().clone();
    assert!(
        states_now.contains(&"activated".to_string()),
        "重启后再次激活：{states_now:?}"
    );
}

#[test]
fn l6_3_reload_restarts_sidecar_process() {
    let (_tmp, kernel) = setup();
    let _fix = install_demo_sidecar(&kernel);

    let pid_before: u64 = kernel
        .call("demo-sidecar", "demo-sidecar.pid", json!({}))
        .expect("pid")
        .as_u64()
        .expect("pid 数字");

    kernel.reload("demo-sidecar").expect("热更新侧车");

    let pid_after: u64 = kernel
        .call("demo-sidecar", "demo-sidecar.pid", json!({}))
        .expect("pid")
        .as_u64()
        .expect("pid 数字");
    assert_ne!(pid_before, pid_after, "热更新后是新进程");
    let pong = kernel
        .call("demo-sidecar", "demo-sidecar.ping", json!({}))
        .expect("新进程服务");
    assert_eq!(pong, json!("pong from python"));
}

#[test]
fn l6_4_rust_call_overhead_unaffected_by_sidecar() {
    let (_tmp, kernel) = setup();
    let fix_b = common::fixture("demo-b");
    kernel.install(&fix_b.dir).expect("装 demo-b");
    let _fix = install_demo_sidecar(&kernel);

    // 对照组：纯 Rust 插件间调用（demo-b.add，本地处理函数）。侧车在场与否
    // 不影响查表路径。
    let add = json!({"a": 1, "b": 2});
    for _ in 0..100 {
        kernel.call("demo-b", "demo-b.add", add.clone()).unwrap();
    }
    let iterations = 20_000u32;
    let start = Instant::now();
    for _ in 0..iterations {
        kernel.call("demo-b", "demo-b.add", add.clone()).unwrap();
    }
    let avg = start.elapsed() / iterations;
    // debug 构建门槛（<1µs 为 release 目标，见 07 §3 与附录 B 登记）。
    assert!(
        avg < Duration::from_micros(10),
        "Rust 间调用平均 {avg:?}，应与 L1 同量级（不受侧车影响）"
    );
}
