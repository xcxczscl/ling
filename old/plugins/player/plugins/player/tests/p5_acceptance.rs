//! P5 验收（`docs/01` §9）：运行场景——沙箱隔离（§5.8 默认全隔离）+ 生命周期。
//!
//! ```sh
//! cargo test -p player --test p5_acceptance -- --test-threads=1
//! ```
//!
//! 覆盖：
//! - e1 生命周期：`player.run`（content_id 主接口）→ 响应 run_id/pid/path/
//!   sandbox_dir/isolation（fs=landlock、network=seccomp、no_new_privs）→
//!   `player.run.exited` exit_code=7 → `run.list` 收敛 + 沙箱产物 done.txt；
//!   `run.stop` 显式杀 → stopped:true → exited 137（128+SIGKILL）→ 再停
//!   stopped:false；错误面 video→P103、allow_dirs "/etc"→P302、相对路径→
//!   P302、allow_network 类型错→P100、stop 未知 run_id→P100
//! - e2 停机即杀：shutdown 后非 background 运行组 ≤5s 消亡；
//!   `allow_background` 授权的存活（用例尾手动清理）
//! - e3 隔离三面：写 /etc 被拒（fs_code≠0 且 /etc/p5owned 不存在）、
//!   默认断网（net_code=1，network=seccomp）、allow_network 授权（net_code=0）

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ling_sdk::Value;

// ===== 内核引导（与 p0–p4_acceptance 同风格） =====

fn player_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

fn rm_root() -> PathBuf {
    player_root().join("../resources-manager").canonicalize().unwrap()
}

fn build_cdylib(root: &Path, pkg: &str) -> PathBuf {
    let status = std::process::Command::new(env!("CARGO"))
        .args(["build", "-p", pkg])
        .current_dir(root)
        .status()
        .unwrap_or_else(|e| panic!("build {pkg}: {e}"));
    assert!(status.success(), "build {pkg} 失败");
    let lib = format!(
        "{}{}{}",
        std::env::consts::DLL_PREFIX,
        pkg.replace('-', "_"),
        std::env::consts::DLL_SUFFIX
    );
    let src = root.join("target/debug").join(&lib);
    assert!(src.is_file(), "cdylib 未找到：{}", src.display());
    src
}

fn install_dir(name: &str, root: &Path, pkg: &str, plugin_id: &str) -> PathBuf {
    let src = build_cdylib(root, pkg);
    let dir = root.join(format!("target/debug/player-e2e-{name}-{}", std::process::id()));
    if !dir.join("plugin.toml").is_file() {
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::copy(root.join(format!("plugins/{pkg}/plugin.toml")), dir.join("plugin.toml"))
            .or_else(|_| {
                std::fs::copy(root.join(format!("crates/{pkg}/plugin.toml")), dir.join("plugin.toml"))
            })
            .expect("plugin.toml 复制失败");
        std::fs::copy(&src, dir.join(format!("{plugin_id}{}", std::env::consts::DLL_SUFFIX))).unwrap();
    }
    dir
}

struct Device {
    kernel: ling_kernel::Kernel,
}

fn boot(rm_dir: &Path, player_dir: &Path, root: &Path) -> Device {
    let kernel = ling_kernel::Kernel::new(root.join("ling")).unwrap();
    let installed = kernel.installed_plugins();
    if !installed.iter().any(|p| p == "resources-manager") {
        kernel.install(rm_dir).unwrap();
    }
    if !installed.iter().any(|p| p == "player") {
        kernel.install(player_dir).unwrap();
    }
    for id in ["resources-manager", "player"] {
        if !kernel.activated_plugins().iter().any(|p| p == id) {
            kernel.load(id).unwrap();
        }
    }
    Device { kernel }
}

impl Device {
    fn call(&self, plugin: &str, method: &str, args: Value) -> Value {
        self.kernel
            .call(plugin, method, args)
            .unwrap_or_else(|e| panic!("{method} 失败：{e:?}"))
    }

    fn try_call(&self, plugin: &str, method: &str, args: Value) -> Result<Value, ling_sdk::CallError> {
        self.kernel.call(plugin, method, args)
    }

    fn wait_job(&self, job_id: &str) -> Value {
        for _ in 0..300 {
            let j = self.call("resources-manager", "resources-manager.job.list", serde_json::json!({}));
            if let Some(rows) = j["rows"].as_array() {
                if let Some(row) = rows.iter().find(|r| r["id"] == job_id) {
                    if row["state"] == "done" || row["state"] == "failed" {
                        return row.clone();
                    }
                }
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        panic!("作业未完成：{job_id}");
    }
}

// ===== 夹具（shebang 脚本，rm 扫 .exe → app） =====

struct Fixtures {
    exiter: i64,
    sleeper: i64,
    fsprobe: i64,
    netprobe: i64,
    clip: i64,
}

fn setup_fixtures(dev: &Device, root: &Path) -> Fixtures {
    let src_dir = root.join("src");
    std::fs::create_dir_all(&src_dir).unwrap();
    let write = |name: &str, body: &str| {
        std::fs::write(src_dir.join(name), body).unwrap_or_else(|e| panic!("写夹具 {name}：{e}"));
    };
    // 退出码 7 + 沙箱内产物（cwd = 沙箱目录）。
    write("exiter.exe", "#!/bin/sh\necho ran > done.txt\nexit 7\n");
    // 长命进程（停机/停止的靶子）。
    write("sleeper.exe", "#!/bin/sh\nsleep 600\n");
    // 写 /etc 应被 Landlock 拒；$? 落沙箱。
    write(
        "fsprobe.exe",
        "#!/bin/sh\necho pwn > /etc/p5owned 2>/dev/null\necho $? > fs_code.txt\n",
    );
    // 起 socket：默认 seccomp 拒 → 1；授权联网 → 0（-B 不写 pyc）。
    write(
        "netprobe.exe",
        "#!/bin/sh\npython3 -B -c 'import socket; socket.socket()' >/dev/null 2>&1\necho $? > net_code.txt\n",
    );
    // 视频资产（不能运行 → P103 面，永远不 spawn）。
    write("clip.mp4", "not a real movie\n");

    dev.call(
        "resources-manager",
        "resources-manager.location.add",
        serde_json::json!({"path": src_dir.display().to_string(), "kind": "local", "label": "p5-src"}),
    );
    let loc = dev.call("resources-manager", "resources-manager.location.list", serde_json::json!({}))
        ["rows"][0]["id"].as_str().unwrap().to_string();
    let scan = dev.call("resources-manager", "resources-manager.scan.start", serde_json::json!({"location_id": loc}));
    let row = dev.wait_job(scan["job_id"].as_str().unwrap());
    assert_eq!(row["state"], "done", "扫描失败：{row}");

    let browse = dev.call(
        "resources-manager",
        "resources-manager.browse.query",
        serde_json::json!({"page": {"limit": 100}}),
    );
    let rows = browse["rows"].as_array().unwrap().clone();
    let find = |name: &str| -> i64 {
        rows.iter()
            .find(|r| r["rel_path"] == name)
            .unwrap_or_else(|| panic!("browse 应含 {name}"))["placement_id"]
            .as_i64()
            .unwrap()
    };
    Fixtures {
        exiter: find("exiter.exe"),
        sleeper: find("sleeper.exe"),
        fsprobe: find("fsprobe.exe"),
        netprobe: find("netprobe.exe"),
        clip: find("clip.mp4"),
    }
}

// ===== 退出事件与进程观测 =====

fn subscribe_exited(dev: &Device) -> Arc<Mutex<Vec<Value>>> {
    let evs = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&evs);
    dev.kernel.subscribe("player.run.exited", Box::new(move |v| {
        sink.lock().unwrap().push(v);
    }));
    evs
}

fn wait_exited(evs: &Arc<Mutex<Vec<Value>>>, run_id: &str, timeout: Duration) -> Value {
    let deadline = Instant::now() + timeout;
    loop {
        let found = evs
            .lock()
            .unwrap()
            .iter()
            .find(|v| v["run_id"] == run_id)
            .cloned();
        if let Some(v) = found {
            return v;
        }
        if Instant::now() >= deadline {
            panic!("等 player.run.exited {run_id} 超时；已收到 {:?}", evs.lock().unwrap());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn wait_gone(pid: i32, timeout: Duration, what: &str) {
    // 容器 PID1 不回收孤儿：kill(-pgid,0) 对「仅剩僵尸」的组永远成功，
    // 不能作消亡判据。改为轮询 /proc/<leader>——leader 由插件监控线程
    // try_wait 收割，reaped 后 /proc 条目即消失。
    let deadline = Instant::now() + timeout;
    while Path::new(&format!("/proc/{pid}")).exists() {
        if Instant::now() >= deadline {
            panic!("{what}：pid {pid} 应消亡，/proc 条目仍在");
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn run_row(dev: &Device, run_id: &str) -> Value {
    let rows = dev.call("player", "player.run.list", serde_json::json!({}))["rows"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    rows.iter()
        .find(|r| r["run_id"] == run_id)
        .cloned()
        .unwrap_or_else(|| panic!("run.list 应含 {run_id}：{rows:?}"))
}

// ===== 用例 =====

/// e1 生命周期：run → exited 事件 → list 收敛 → 沙箱产物；stop 显式杀（137）；
/// 错误面 P103/P302/P100。
#[test]
fn e1_run_lifecycle_stop_and_error_faces() {
    let rm_dir = install_dir("rm", &rm_root(), "resources-manager", "resources-manager");
    let pl_dir = install_dir("player", &player_root(), "player", "player");
    let tmp = tempfile::tempdir().unwrap();
    let dev = boot(&rm_dir, &pl_dir, tmp.path());
    let fx = setup_fixtures(&dev, tmp.path());
    let evs = subscribe_exited(&dev);

    // --- content_id 主接口跑 exiter（§5.12 主接口 + placement_id 兼容在下方用） ---
    let browse = dev.call(
        "resources-manager",
        "resources-manager.browse.query",
        serde_json::json!({"page": {"limit": 100}}),
    );
    let cid = browse["rows"].as_array()
        .unwrap()
        .iter()
        .find(|r| r["placement_id"].as_i64() == Some(fx.exiter))
        .and_then(|r| r["content_id"].as_str())
        .expect("exiter 行应有 content_id")
        .to_string();
    let r = dev.call("player", "player.run", serde_json::json!({"content_id": cid}));
    let run_id = r["run_id"].as_str().unwrap().to_string();
    assert!(run_id.starts_with("run-"), "run_id 形态：{r}");
    let pid = r["pid"].as_i64().unwrap();
    assert!(pid > 0, "pid 回带：{r}");
    assert!(
        r["path"].as_str().unwrap().ends_with("exiter.exe"),
        "path 回带：{r}"
    );
    let sbx = PathBuf::from(r["sandbox_dir"].as_str().unwrap());
    assert!(
        sbx.to_string_lossy().contains("/sandbox/"),
        "沙箱目录：{sbx:?}"
    );
    // §5.8 默认全隔离：如实上报（本环境 Landlock/seccomp 可用，探针实证）。
    let iso = &r["isolation"];
    assert_eq!(iso["fs"], "landlock", "fs 隔离：{r}");
    assert_eq!(iso["network"], "seccomp", "网络隔离：{r}");
    assert_eq!(iso["no_new_privs"], true, "不给提权：{r}");

    let ev = wait_exited(&evs, &run_id, Duration::from_secs(5));
    assert_eq!(ev["exit_code"], 7, "退出码原样带回：{ev}");
    let row = run_row(&dev, &run_id);
    assert_eq!(row["state"], "exited", "{row}");
    assert_eq!(row["exit_code"], 7, "{row}");
    assert_eq!(row["pid"], pid, "{row}");
    assert_eq!(row["allow_background"], false, "缺省退出即杀：{row}");
    // 沙箱产物（cwd = 沙箱）。
    let done = std::fs::read_to_string(sbx.join("done.txt"))
        .unwrap_or_else(|e| panic!("done.txt 应存在（{e}）：{sbx:?}"));
    assert_eq!(done.trim(), "ran");
    assert!(sbx.join("stdout.log").is_file(), "stdout.log 落沙箱");

    // --- placement_id 兼容 + run.stop 显式杀 → 128+SIGKILL ---
    let r2 = dev.call(
        "player",
        "player.run",
        serde_json::json!({"placement_id": fx.sleeper}),
    );
    let rid2 = r2["run_id"].as_str().unwrap().to_string();
    let s = dev.call("player", "player.run.stop", serde_json::json!({"run_id": &rid2}));
    assert_eq!(s["stopped"], true, "{s}");
    let ev2 = wait_exited(&evs, &rid2, Duration::from_secs(5));
    assert_eq!(ev2["exit_code"], 137, "SIGKILL → 128+9：{ev2}");
    assert_eq!(run_row(&dev, &rid2)["state"], "exited");
    // 已退出再停：stopped:false 不报错。
    let s2 = dev.call("player", "player.run.stop", serde_json::json!({"run_id": &rid2}));
    assert_eq!(s2["stopped"], false, "{s2}");

    // --- 错误面 ---
    let e = dev
        .try_call("player", "player.run", serde_json::json!({"placement_id": fx.clip}))
        .unwrap_err();
    assert_eq!(e.code(), "P103", "video 不能运行：{e:?}");
    let e = dev
        .try_call(
            "player",
            "player.run",
            serde_json::json!({"placement_id": fx.exiter, "sandbox_opts": {"allow_dirs": ["/etc"]}}),
        )
        .unwrap_err();
    assert_eq!(e.code(), "P302", "系统根不可整体授权：{e:?}");
    let e = dev
        .try_call(
            "player",
            "player.run",
            serde_json::json!({"placement_id": fx.exiter, "sandbox_opts": {"allow_dirs": ["relative/dir"]}}),
        )
        .unwrap_err();
    assert_eq!(e.code(), "P302", "相对路径：{e:?}");
    let e = dev
        .try_call(
            "player",
            "player.run",
            serde_json::json!({"placement_id": fx.exiter, "sandbox_opts": {"allow_network": "yes"}}),
        )
        .unwrap_err();
    assert_eq!(e.code(), "P100", "类型错：{e:?}");
    let e = dev
        .try_call("player", "player.run.stop", serde_json::json!({"run_id": "run-deadbeef"}))
        .unwrap_err();
    assert_eq!(e.code(), "P100", "未知 run_id：{e:?}");
}

/// e2 停机即杀（§5.8 生命周期行）：shutdown → 非 background 组 ≤5s 消亡；
/// allow_background 授权的继续存活（用例尾手动清理防孤儿）。
#[test]
fn e2_shutdown_kills_except_allow_background() {
    let rm_dir = install_dir("rm", &rm_root(), "resources-manager", "resources-manager");
    let pl_dir = install_dir("player", &player_root(), "player", "player");
    let tmp = tempfile::tempdir().unwrap();
    let dev = boot(&rm_dir, &pl_dir, tmp.path());
    let fx = setup_fixtures(&dev, tmp.path());

    let r1 = dev.call(
        "player",
        "player.run",
        serde_json::json!({"placement_id": fx.sleeper}),
    );
    let pid1 = r1["pid"].as_i64().unwrap() as i32;
    let r2 = dev.call(
        "player",
        "player.run",
        serde_json::json!({"placement_id": fx.sleeper, "sandbox_opts": {"allow_background": true}}),
    );
    let pid2 = r2["pid"].as_i64().unwrap() as i32;
    assert!(player::sandbox::process_alive(pid1), "sleeper 已起");
    assert!(player::sandbox::process_alive(pid2), "background sleeper 已起");

    dev.kernel.shutdown().expect("停机");
    wait_gone(pid1, Duration::from_secs(5), "停机即杀（缺省）");
    assert!(
        player::sandbox::process_alive(pid2),
        "allow_background 授权的进程应存活"
    );
    // 手动清理（防孤儿进后续用例）。
    player::sandbox::kill_process_group(pid2);
    wait_gone(pid2, Duration::from_secs(5), "用例收尾清理");
}

/// e3 隔离三面：文件系统拒写 /etc、默认断网、allow_network 授权。
#[test]
fn e3_isolation_fs_and_network_faces() {
    assert!(
        !Path::new("/etc/p5owned").exists(),
        "残留 /etc/p5owned（历史失败泄漏），先清理"
    );
    let rm_dir = install_dir("rm", &rm_root(), "resources-manager", "resources-manager");
    let pl_dir = install_dir("player", &player_root(), "player", "player");
    let tmp = tempfile::tempdir().unwrap();
    let dev = boot(&rm_dir, &pl_dir, tmp.path());
    let fx = setup_fixtures(&dev, tmp.path());
    let evs = subscribe_exited(&dev);

    // --- 文件系统面：写 /etc 被 Landlock 拒 ---
    let r = dev.call("player", "player.run", serde_json::json!({"placement_id": fx.fsprobe}));
    assert_eq!(r["isolation"]["fs"], "landlock", "{r}");
    let rid = r["run_id"].as_str().unwrap().to_string();
    let sbx = PathBuf::from(r["sandbox_dir"].as_str().unwrap());
    wait_exited(&evs, &rid, Duration::from_secs(5));
    let code = std::fs::read_to_string(sbx.join("fs_code.txt"))
        .unwrap_or_else(|e| panic!("fs_code.txt 应存在（{e}）"));
    assert_ne!(code.trim(), "0", "写 /etc 应被拒，实际：{code:?}");
    assert!(!Path::new("/etc/p5owned").exists(), "沙箱内写 /etc 不应落地");

    // --- 网络面：默认 seccomp 拦 socket → 1 ---
    let r = dev.call("player", "player.run", serde_json::json!({"placement_id": fx.netprobe}));
    assert_eq!(r["isolation"]["network"], "seccomp", "{r}");
    let rid = r["run_id"].as_str().unwrap().to_string();
    let sbx = PathBuf::from(r["sandbox_dir"].as_str().unwrap());
    wait_exited(&evs, &rid, Duration::from_secs(5));
    let code = std::fs::read_to_string(sbx.join("net_code.txt"))
        .unwrap_or_else(|e| panic!("net_code.txt 应存在（{e}）"));
    assert_eq!(code.trim(), "1", "默认断网（socket → EPERM）：{code:?}");

    // --- 授权联网：allow_network → granted → socket 成功 → 0 ---
    let r = dev.call(
        "player",
        "player.run",
        serde_json::json!({"placement_id": fx.netprobe, "sandbox_opts": {"allow_network": true}}),
    );
    assert_eq!(r["isolation"]["network"], "granted", "{r}");
    let rid = r["run_id"].as_str().unwrap().to_string();
    let sbx = PathBuf::from(r["sandbox_dir"].as_str().unwrap());
    wait_exited(&evs, &rid, Duration::from_secs(5));
    let code = std::fs::read_to_string(sbx.join("net_code.txt"))
        .unwrap_or_else(|e| panic!("net_code.txt 应存在（{e}）"));
    assert_eq!(code.trim(), "0", "授权联网后 socket 应成功：{code:?}");
}
