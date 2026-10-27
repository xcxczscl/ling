//! P4 验收（`docs/01` §9）：串流输出 v2——重编码模式 B + 自动/手动路由 + 电视协议占位。
//!
//! ```sh
//! cargo test -p player --test p4_acceptance -- --test-threads=1
//! ```
//!
//! 覆盖：
//! - e1 自动路由 → A：h264 内容 ↔ lite 宣称 `codecs:["h264","aac"]` → 对端起播、
//!   position 前进；`receiver.list` 行含 codecs 宣称
//! - e2 自动重编码 → B：mpeg4 内容（对端不宣称）→ 自动选 B（§5.3.2）→
//!   发送端 ffmpeg 现场转码分块流 → 对端 mpv 起播且 position 前进
//! - e3 手动强制与拒绝面：强制 C（帧流）在 h264 内容上生效并起播；强制 A 在 mpeg4
//!   内容上覆盖自动 B；mode D（电视协议占位）→ P100；未知 mode → P100

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use ling_sdk::Value;

// ===== 内核引导（与 p0–p3_acceptance 同风格） =====

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

/// 夹具：h264 mp4（内容编码 ∈ lite 宣称 → 自动 A）+ mpeg4 mp4（不宣称 → 自动 B）。
/// 返回 (h264_pid, mpeg4_pid)。
fn setup_fixtures(dev: &Device, root: &Path) -> (i64, i64) {
    let src_dir = root.join("src");
    std::fs::create_dir_all(&src_dir).unwrap();
    let mk = |name: &str, codec: &str| {
        let out = src_dir.join(name);
        let st = Command::new("ffmpeg")
            .args([
                "-y", "-f", "lavfi", "-i", "testsrc=duration=10:size=320x240:rate=15",
                "-pix_fmt", "yuv420p", "-c:v", codec, "-q:v", "5",
                out.to_str().unwrap(),
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .unwrap_or_else(|e| panic!("ffmpeg {name}: {e}"));
        assert!(st.success(), "ffmpeg 生成 {name} 失败");
    };
    mk("clip_h264.mp4", "libx264");
    mk("clip_mpeg4.mp4", "mpeg4");

    dev.call(
        "resources-manager",
        "resources-manager.location.add",
        serde_json::json!({"path": src_dir.display().to_string(), "kind": "local", "label": "p4-src"}),
    );
    let loc = dev.call("resources-manager", "resources-manager.location.list", serde_json::json!({}))
        ["rows"][0]["id"].as_str().unwrap().to_string();
    let scan = dev.call("resources-manager", "resources-manager.scan.start", serde_json::json!({"location_id": loc}));
    let row = dev.wait_job(scan["job_id"].as_str().unwrap());
    assert_eq!(row["state"], "done", "扫描失败：{row}");

    let browse = dev.call("resources-manager", "resources-manager.browse.query", serde_json::json!({"page": {"limit": 100}}));
    let rows = browse["rows"].as_array().unwrap();
    let find = |name: &str| -> i64 {
        rows.iter()
            .find(|r| r["rel_path"] == name)
            .unwrap_or_else(|| panic!("browse 应含 {name}"))["placement_id"]
            .as_i64()
            .unwrap()
    };
    (find("clip_h264.mp4"), find("clip_mpeg4.mp4"))
}

fn play(dev: &Device, placement_id: i64) -> String {
    dev.call("player", "player.play", serde_json::json!({"placement_id": placement_id}))
        ["session_id"]
        .as_str()
        .unwrap()
        .to_string()
}

fn wait_position(dev: &Device, sid: &str, timeout: Duration, min: f64, what: &str) -> f64 {
    let deadline = Instant::now() + timeout;
    loop {
        let wrap = dev.call("player", "player.status", serde_json::json!({"session_id": sid}));
        let p = wrap["sessions"][0]["position_sec"].as_f64().unwrap_or(-1.0);
        if p > min {
            return p;
        }
        assert!(Instant::now() < deadline, "{what} 超时；最后 position={p}");
        std::thread::sleep(Duration::from_millis(150));
    }
}

// ===== lite 接收器进程管理 =====

/// 接收器子进程守卫：Drop 即杀（防孤儿进程泄漏进后续用例）。
struct LiteGuard(Child);

impl Drop for LiteGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn build_lite() -> PathBuf {
    let root = player_root();
    let st = Command::new(env!("CARGO"))
        .args(["build", "-p", "player", "--bin", "player-lite"])
        .current_dir(&root)
        .status()
        .expect("build player-lite");
    assert!(st.success(), "build player-lite 失败");
    let bin = root.join("target/debug/player-lite");
    assert!(bin.is_file(), "player-lite 未找到");
    bin
}

/// 起 lite（`--port 0` → stdout `PORT=<n>` 回显）；`hb` = 心跳目的地（指向本内核实例
/// `receiver.list` 外发的 discovery_port，附录 B #44）。
fn spawn_lite(id: &str, name: &str, hb: &str) -> (LiteGuard, u16) {
    let bin = build_lite();
    let mut child = Command::new(&bin)
        .args(["--port", "0", "--id", id, "--name", name, "--hb", hb])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("启动 player-lite");
    let stdout = child.stdout.take().unwrap();
    let mut lines = BufReader::new(stdout).lines();
    let first = lines
        .next()
        .unwrap_or_else(|| panic!("lite 无输出（启动失败？）"))
        .unwrap_or_else(|e| panic!("读 lite 输出失败：{e}"));
    let port: u16 = first
        .strip_prefix("PORT=")
        .unwrap_or_else(|| panic!("lite 首行应为 PORT=，实为：{first}"))
        .parse()
        .expect("PORT 解析");
    (LiteGuard(child), port)
}

/// 本内核实例的发现端口（activate 时已绑；断言非 0）。
fn discovery_port(dev: &Device) -> u16 {
    let out = dev.call("player", "player.receiver.list", serde_json::json!({}));
    let port = out["discovery_port"].as_u64().unwrap_or(0) as u16;
    assert!(port != 0, "发现端口应非 0（7916 或临时退让）：{out}");
    port
}

fn http_client() -> reqwest::blocking::Client {
    reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap()
}

fn receiver_status(addr: &str, port: u16) -> Value {
    let url = format!("http://{addr}:{port}/status");
    let resp = http_client().get(&url).send().expect("receiver /status");
    resp.text()
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or(Value::Null)
}

fn wait_receiver(
    dev: &Device,
    id: &str,
    timeout: Duration,
    pred: impl Fn(&Value) -> bool,
    what: &str,
) -> Value {
    let deadline = Instant::now() + timeout;
    loop {
        let rows = dev.call("player", "player.receiver.list", serde_json::json!({}))
            ["rows"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        if let Some(row) = rows.iter().find(|r| r["id"] == id) {
            if pred(row) {
                return row.clone();
            }
        }
        if Instant::now() >= deadline {
            panic!("等接收器 {id} {what} 超时；当前列表：{rows:?}");
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

fn wait_receiver_state(addr: &str, port: u16, state: &str, timeout: Duration) -> Value {
    let deadline = Instant::now() + timeout;
    loop {
        let st = receiver_status(addr, port);
        if st["state"] == state {
            return st;
        }
        assert!(
            Instant::now() < deadline,
            "receiver 应回 {state}，实为 {}（{st}）",
            st["state"]
        );
        std::thread::sleep(Duration::from_millis(250));
    }
}

/// 对端 position 前进（B 转码现场流由 mpv 按 pts 正常步进）。
fn wait_receiver_position(addr: &str, port: u16, min: f64, timeout: Duration) -> f64 {
    let deadline = Instant::now() + timeout;
    loop {
        let st = receiver_status(addr, port);
        let p = st["position_sec"].as_f64().unwrap_or(0.0);
        if p > min {
            return p;
        }
        assert!(Instant::now() < deadline, "对端 position 未>{min}：{st}");
        std::thread::sleep(Duration::from_millis(250));
    }
}

// ===== 用例 =====

#[test]
fn e1_auto_route_h264_content_picks_mode_a() {
    std::env::set_var("PLAYER_VO", "null");
    let rm_dir = install_dir("rm", &rm_root(), "resources-manager", "resources-manager");
    let pl_dir = install_dir("player", &player_root(), "player", "player");
    let tmp = tempfile::tempdir().unwrap();
    let dev = boot(&rm_dir, &pl_dir, tmp.path());
    let (h264_pid, _) = setup_fixtures(&dev, tmp.path());

    let sid = play(&dev, h264_pid);
    wait_position(&dev, &sid, Duration::from_secs(6), 0.2, "本机 position 前进");

    let dp = discovery_port(&dev);
    let (_lite, rx_port) = spawn_lite("lite-p4e1", "p4-客厅", &format!("127.0.0.1:{dp}"));
    let rx = wait_receiver(&dev, "lite-p4e1", Duration::from_secs(6), |_| true, "出现");
    // #50：codecs 宣称随发现行外发（自动路由的对端能力输入）
    let codecs = rx["codecs"].as_array().cloned().unwrap_or_default();
    let claims_h264 = codecs.iter().any(|c| c.as_str() == Some("h264"));
    assert!(claims_h264, "lite 应宣称 h264：{rx}");

    // 缺省 mode → 自动路由：h264 ∈ 宣称 → A（§5.3.2 效率最优）。
    let r = dev.call(
        "player",
        "player.cast",
        serde_json::json!({"session_id": &sid, "target_device": "lite-p4e1"}),
    );
    assert_eq!(r["accepted"], true, "{r}");
    assert_eq!(r["mode"], "A", "h264 内容对端宣称支持 → 自动 A：{r}");
    assert_eq!(r["receiver_id"], "lite-p4e1");

    let st = wait_receiver_state("127.0.0.1", rx_port, "playing", Duration::from_secs(15));
    assert_eq!(st["mode"], "A", "对端按 A 起播：{st}");
    wait_receiver_position("127.0.0.1", rx_port, 0.2, Duration::from_secs(10));

    let r = dev.call("player", "player.cast.stop", serde_json::json!({"session_id": &sid}));
    assert_eq!(r["stopped"], true, "{r}");
    wait_receiver_state("127.0.0.1", rx_port, "idle", Duration::from_secs(8));
    dev.call("player", "player.stop", serde_json::json!({"session_id": &sid}));
}

#[test]
fn e2_auto_route_unsupported_codec_reencodes_to_b() {
    std::env::set_var("PLAYER_VO", "null");
    let rm_dir = install_dir("rm", &rm_root(), "resources-manager", "resources-manager");
    let pl_dir = install_dir("player", &player_root(), "player", "player");
    let tmp = tempfile::tempdir().unwrap();
    let dev = boot(&rm_dir, &pl_dir, tmp.path());
    let (_, mpeg4_pid) = setup_fixtures(&dev, tmp.path());

    let sid = play(&dev, mpeg4_pid);
    wait_position(&dev, &sid, Duration::from_secs(6), 0.2, "本机 position 前进");

    let dp = discovery_port(&dev);
    let (_lite, rx_port) = spawn_lite("lite-p4e2", "p4-盒子", &format!("127.0.0.1:{dp}"));
    wait_receiver(&dev, "lite-p4e2", Duration::from_secs(6), |_| true, "出现");

    // 缺省 mode → 自动：mpeg4 ∉ 宣称、h264 ∈ 宣称、带宽 100 ≥ 20 → B（§5.3.2）。
    let r = dev.call(
        "player",
        "player.cast",
        serde_json::json!({"session_id": &sid, "target_device": "lite-p4e2"}),
    );
    assert_eq!(r["accepted"], true, "{r}");
    assert_eq!(r["mode"], "B", "mpeg4 内容应自动重编码到 B：{r}");

    // 对端经发送端 ffmpeg 转码分块流起播且 position 前进（§9 P4 验收核心）。
    let st = wait_receiver_state("127.0.0.1", rx_port, "playing", Duration::from_secs(20));
    assert_eq!(st["mode"], "B", "对端按 B 起播：{st}");
    let stream_url = st["stream_url"].as_str().unwrap_or_default().to_string();
    assert!(stream_url.contains("/s/"), "对端仍从代理拉现场流：{stream_url}");
    wait_receiver_position("127.0.0.1", rx_port, 0.2, Duration::from_secs(12));

    dev.call("player", "player.cast.stop", serde_json::json!({"session_id": &sid}));
    dev.call("player", "player.stop", serde_json::json!({"session_id": &sid}));
}

#[test]
fn e3_manual_override_and_mode_faces() {
    std::env::set_var("PLAYER_VO", "null");
    let rm_dir = install_dir("rm", &rm_root(), "resources-manager", "resources-manager");
    let pl_dir = install_dir("player", &player_root(), "player", "player");
    let tmp = tempfile::tempdir().unwrap();
    let dev = boot(&rm_dir, &pl_dir, tmp.path());
    let (h264_pid, mpeg4_pid) = setup_fixtures(&dev, tmp.path());

    let dp = discovery_port(&dev);
    let (_lite, rx_port) = spawn_lite("lite-p4e3", "p4-书房", &format!("127.0.0.1:{dp}"));
    wait_receiver(&dev, "lite-p4e3", Duration::from_secs(6), |_| true, "出现");

    // 手动强制 C：h264 内容本会自动 A，用户覆盖 → 帧流（mpjpeg 现场流起播即证生效）。
    let sid_a = play(&dev, h264_pid);
    let r = dev.call(
        "player",
        "player.cast",
        serde_json::json!({"session_id": &sid_a, "target_device": "lite-p4e3", "mode": "C"}),
    );
    assert_eq!(r["accepted"], true, "{r}");
    assert_eq!(r["mode"], "C", "强制 C 覆盖自动 A：{r}");
    let st = wait_receiver_state("127.0.0.1", rx_port, "playing", Duration::from_secs(20));
    assert_eq!(st["mode"], "C", "对端按 C 起播：{st}");
    dev.call("player", "player.cast.stop", serde_json::json!({"session_id": &sid_a}));
    wait_receiver_state("127.0.0.1", rx_port, "idle", Duration::from_secs(8));
    dev.call("player", "player.stop", serde_json::json!({"session_id": &sid_a}));

    // 手动强制 A：mpeg4 内容本会自动 B，用户覆盖 → 直接字节转发
    //（对端 mpv 实际能解 mpeg4——宣称是保守基线，override 以用户为准）。
    let sid_b = play(&dev, mpeg4_pid);
    let r = dev.call(
        "player",
        "player.cast",
        serde_json::json!({"session_id": &sid_b, "target_device": "lite-p4e3", "mode": "A"}),
    );
    assert_eq!(r["accepted"], true, "{r}");
    assert_eq!(r["mode"], "A", "强制 A 覆盖自动 B：{r}");
    wait_receiver_state("127.0.0.1", rx_port, "playing", Duration::from_secs(15));

    // mode D（电视协议 Sink，P4 仅占位）→ P100；未知 mode → P100。
    for (mode, needle) in [("D", "占位"), ("E", "auto/A/B/C")] {
        let err = dev
            .try_call(
                "player",
                "player.cast",
                serde_json::json!({"session_id": &sid_b, "target_device": "lite-p4e3", "mode": mode}),
            )
            .unwrap_err();
        assert_eq!(err.code(), "P100", "mode {mode}：{err:?}");
        assert!(err.message().contains(needle), "{}", err.message());
    }

    dev.call("player", "player.cast.stop", serde_json::json!({"session_id": &sid_b}));
    dev.call("player", "player.stop", serde_json::json!({"session_id": &sid_b}));
}
