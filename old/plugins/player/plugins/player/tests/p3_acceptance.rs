//! P3 验收（`docs/01` §9）：串流输出 v1——模式 A 字节转发 + lite 接收器 + 局域网发现。
//!
//! ```sh
//! cargo test -p player --test p3_acceptance -- --test-threads=1
//! ```
//!
//! 覆盖：
//! - e1 全链路：播放 → lite 心跳发现 → cast（模式 A 强制，字节对拍口径）→ 对端正常播放
//!   （position 前进）→ 代理字节与直读 rm 流完全一致（整流 + Range 分段）→ 坏 token 403 →
//!   cast.stop 对端回待机 → 摘路由后代理 403
//! - e2 拒绝面：mode D → P100（P4 起 B/C 合法→未知接收器转 P201，见 #50）、缺参 → P100、
//!   未知接收器 → P201、查看器会话 → P103、无投屏时 cast.stop → `{stopped:false}`
//! - e3 会话 stop 自动结束投屏（摘路由 + 通知对端回待机 + cast map 清理）

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use ling_sdk::Value;

// ===== 内核引导（与 p0/p1/p2_acceptance 同风格） =====

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

/// 夹具：ffmpeg mp4（视频投屏）+ PNG（查看器拒绝面）→ (video_pid, image_pid)。
fn setup_fixtures(dev: &Device, root: &Path) -> (i64, i64) {
    let src_dir = root.join("src");
    std::fs::create_dir_all(&src_dir).unwrap();
    let mp4 = src_dir.join("clip.mp4");
    let st = std::process::Command::new("ffmpeg")
        .args([
            "-y", "-f", "lavfi", "-i", "testsrc=duration=5:size=320x240:rate=15",
            "-pix_fmt", "yuv420p", "-c:v", "mpeg4", "-q:v", "5",
            mp4.to_str().unwrap(),
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("ffmpeg 生成测试视频");
    assert!(st.success());
    let png = src_dir.join("pic.png");
    let st = std::process::Command::new("ffmpeg")
        .args([
            "-y", "-f", "lavfi", "-i", "color=red:size=64x48:duration=0.1",
            "-frames:v", "1", png.to_str().unwrap(),
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("ffmpeg 生成测试图片");
    assert!(st.success());

    dev.call(
        "resources-manager",
        "resources-manager.location.add",
        serde_json::json!({"path": src_dir.display().to_string(), "kind": "local", "label": "p3-src"}),
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
    (find("clip.mp4"), find("pic.png"))
}

fn status_of(dev: &Device, sid: &str) -> Value {
    let wrap = dev.call("player", "player.status", serde_json::json!({"session_id": sid}));
    wrap["sessions"]
        .as_array()
        .and_then(|a| a.first().cloned())
        .unwrap_or_else(|| panic!("status 中无会话 {sid}：{wrap}"))
}

fn wait_position(dev: &Device, sid: &str, timeout: Duration, min: f64, what: &str) -> f64 {
    let deadline = Instant::now() + timeout;
    loop {
        let p = status_of(dev, sid)["position_sec"].as_f64().unwrap_or(-1.0);
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
    let st = std::process::Command::new(env!("CARGO"))
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
/// `receiver.list` 外发的 discovery_port——7916 可能被同进程早前装载实例占着，#44）。
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

// ===== 用例 =====

#[test]
fn e1_cast_flow_discovery_play_bytes_and_stop() {
    std::env::set_var("PLAYER_VO", "null");
    let rm_dir = install_dir("rm", &rm_root(), "resources-manager", "resources-manager");
    let pl_dir = install_dir("player", &player_root(), "player", "player");
    let tmp = tempfile::tempdir().unwrap();
    let dev = boot(&rm_dir, &pl_dir, tmp.path());
    let (video_pid, _) = setup_fixtures(&dev, tmp.path());

    // 播放（投屏需 mpv 会话；§5.12 player.cast）。
    let sid = dev.call("player", "player.play", serde_json::json!({"placement_id": video_pid}))
        ["session_id"]
        .as_str()
        .unwrap()
        .to_string();
    wait_position(&dev, &sid, Duration::from_secs(6), 0.2, "本机 position 前进");

    // 发现面：列表结构 + 无接收器时为空。
    let rows = dev.call("player", "player.receiver.list", serde_json::json!({}))["rows"]
        .as_array()
        .unwrap()
        .clone();
    assert!(rows.iter().all(|r| r["id"] != "lite-e1"));

    // lite 心跳 → 发现（附录 B #43/#44：心跳打到本实例的 discovery_port）。
    // 注意：lite 同时发 loopback 与广播两个心跳，后者源地址是容器网卡 IP
    //（172.x）——upsert 后到者覆盖，addr 以实际报文源为准，两种都可达本机代理。
    let dp = discovery_port(&dev);
    let (lite, rx_port) = spawn_lite("lite-e1", "e2e-客厅", &format!("127.0.0.1:{dp}"));
    let rx = wait_receiver(&dev, "lite-e1", Duration::from_secs(6), |_| true, "出现");
    assert_eq!(rx["port"].as_u64().unwrap(), rx_port as u64);
    assert!(!rx["addr"].as_str().unwrap().is_empty(), "心跳源地址非空：{rx}");
    assert_eq!(rx["state"], "idle");

    // 投屏（模式 A 强制——本用例断言字节转发，auto 路由属 P4 p4_acceptance）。
    let r = dev.call(
        "player",
        "player.cast",
        serde_json::json!({"session_id": &sid, "target_device": "lite-e1", "mode": "A"}),
    );
    assert_eq!(r["accepted"], true, "{r}");
    assert_eq!(r["mode"], "A");
    assert_eq!(r["receiver_id"], "lite-e1");

    // 对端正常播放（§9 P3 验收：position 前进）。
    let mut st = wait_receiver_state("127.0.0.1", rx_port, "playing", Duration::from_secs(15));
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let p = st["position_sec"].as_f64().unwrap_or(0.0);
        if p > 0.2 {
            break;
        }
        assert!(Instant::now() < deadline, "对端 position 未前进：{st}");
        std::thread::sleep(Duration::from_millis(250));
        st = receiver_status("127.0.0.1", rx_port);
    }
    let stream_url = st["stream_url"].as_str().unwrap().to_string();
    assert!(stream_url.contains("/s/"), "代理 URL 形态：{stream_url}");
    assert!(stream_url.contains(":7914/"), "代理默认端口 7914：{stream_url}");

    // 代理字节完整性：与直读 rm 流逐字节一致（整流 + Range 分段）。
    let opened = dev.call(
        "resources-manager",
        "resources-manager.stream.open",
        serde_json::json!({"placement_id": video_pid, "purpose": "verify"}),
    );
    let direct_url = opened["url"].as_str().unwrap();
    let client = http_client();
    let direct = client.get(direct_url).send().unwrap().bytes().unwrap().to_vec();
    assert!(!direct.is_empty());
    let via_proxy = client.get(&stream_url).send().unwrap().bytes().unwrap().to_vec();
    assert_eq!(direct, via_proxy, "代理转发字节须与 rm 流完全一致");
    let range_direct = client
        .get(direct_url)
        .header("Range", "bytes=100-199")
        .send()
        .unwrap()
        .bytes()
        .unwrap()
        .to_vec();
    let range_proxy = client
        .get(&stream_url)
        .header("Range", "bytes=100-199")
        .send()
        .unwrap()
        .bytes()
        .unwrap()
        .to_vec();
    assert_eq!(range_direct, range_proxy, "Range 分段透传一致（seek 依赖）");
    assert_eq!(range_direct.len(), 100);

    // 坏 token → 403（§5.3.1 代理鉴权）。
    let bad_url = {
        let mut u = stream_url.clone();
        // 换掉路径尾部 token（最后 32 位 hex）。
        let cut = u.rfind('/').unwrap();
        u.replace_range(cut + 1.., &"0".repeat(32));
        u
    };
    let bad = client.get(&bad_url).send().unwrap();
    assert_eq!(bad.status().as_u16(), 403, "坏 token 应 403");

    // cast.stop → 对端回待机（§5.13）+ 摘路由后代理 403。
    let r = dev.call("player", "player.cast.stop", serde_json::json!({"session_id": &sid}));
    assert_eq!(r["stopped"], true, "{r}");
    wait_receiver_state("127.0.0.1", rx_port, "idle", Duration::from_secs(5));
    let gone = client.get(&stream_url).send().unwrap();
    assert_eq!(gone.status().as_u16(), 403, "摘路由后代理应 403");
    // 幂等：再停 → stopped:false
    let r = dev.call("player", "player.cast.stop", serde_json::json!({"session_id": &sid}));
    assert_eq!(r["stopped"], false, "{r}");

    dev.call("player", "player.stop", serde_json::json!({"session_id": &sid}));
    drop(lite);
}

#[test]
fn e2_cast_rejection_face() {
    std::env::set_var("PLAYER_VO", "null");
    let rm_dir = install_dir("rm", &rm_root(), "resources-manager", "resources-manager");
    let pl_dir = install_dir("player", &player_root(), "player", "player");
    let tmp = tempfile::tempdir().unwrap();
    let dev = boot(&rm_dir, &pl_dir, tmp.path());
    let (video_pid, image_pid) = setup_fixtures(&dev, tmp.path());

    // 缺 session_id → P100
    let err = dev
        .try_call("player", "player.cast", serde_json::json!({"target_device": "x"}))
        .unwrap_err();
    assert_eq!(err.code(), "P100");

    // mode D（P4 起电视协议仅占位）→ P100，且先于接收器查找（无需 lite）。
    // B/C 在 P4 已合法（#50）→ 走到接收器查找才失败：未知接收器 → P201。
    let sid = dev.call("player", "player.play", serde_json::json!({"placement_id": video_pid}))
        ["session_id"]
        .as_str()
        .unwrap()
        .to_string();
    let err = dev
        .try_call(
            "player",
            "player.cast",
            serde_json::json!({"session_id": &sid, "target_device": "lite-x", "mode": "D"}),
        )
        .unwrap_err();
    assert_eq!(err.code(), "P100", "{err:?}");
    assert!(err.message().contains("占位"), "D 占位提示：{}", err.message());
    for mode in ["B", "C"] {
        let err = dev
            .try_call(
                "player",
                "player.cast",
                serde_json::json!({"session_id": &sid, "target_device": "lite-x", "mode": mode}),
            )
            .unwrap_err();
        assert_eq!(err.code(), "P201", "mode {mode} 合法后未知接收器应 P201：{err:?}");
    }

    // 未知接收器 → P201
    let err = dev
        .try_call(
            "player",
            "player.cast",
            serde_json::json!({"session_id": &sid, "target_device": "lite-none"}),
        )
        .unwrap_err();
    assert_eq!(err.code(), "P201", "{err:?}");

    // 查看器会话（图片）→ P103
    let vsid = dev.call("player", "player.play", serde_json::json!({"placement_id": image_pid}))
        ["session_id"]
        .as_str()
        .unwrap()
        .to_string();
    let err = dev
        .try_call(
            "player",
            "player.cast",
            serde_json::json!({"session_id": &vsid, "target_device": "lite-x"}),
        )
        .unwrap_err();
    assert_eq!(err.code(), "P103", "{err:?}");

    // 无投屏时 cast.stop → {stopped:false}（不报错）
    let r = dev.call("player", "player.cast.stop", serde_json::json!({"session_id": &sid}));
    assert_eq!(r["stopped"], false, "{r}");

    dev.call("player", "player.stop", serde_json::json!({"session_id": &sid}));
    dev.call("player", "player.stop", serde_json::json!({"session_id": &vsid}));
}

#[test]
fn e3_session_stop_auto_ends_cast() {
    std::env::set_var("PLAYER_VO", "null");
    let rm_dir = install_dir("rm", &rm_root(), "resources-manager", "resources-manager");
    let pl_dir = install_dir("player", &player_root(), "player", "player");
    let tmp = tempfile::tempdir().unwrap();
    let dev = boot(&rm_dir, &pl_dir, tmp.path());
    let (video_pid, _) = setup_fixtures(&dev, tmp.path());

    let sid = dev.call("player", "player.play", serde_json::json!({"placement_id": video_pid}))
        ["session_id"]
        .as_str()
        .unwrap()
        .to_string();
    wait_position(&dev, &sid, Duration::from_secs(6), 0.2, "本机 position 前进");

    let dp = discovery_port(&dev);
    let (lite, rx_port) = spawn_lite("lite-e3", "e2e-书房", &format!("127.0.0.1:{dp}"));
    wait_receiver(&dev, "lite-e3", Duration::from_secs(6), |_| true, "出现");
    let r = dev.call(
        "player",
        "player.cast",
        serde_json::json!({"session_id": &sid, "target_device": "lite-e3", "mode": "A"}),
    );
    assert_eq!(r["accepted"], true, "{r}");
    wait_receiver_state("127.0.0.1", rx_port, "playing", Duration::from_secs(15));

    // 会话 stop → 自动结束投屏（摘路由 + 通知对端，附录 B #45）。
    dev.call("player", "player.stop", serde_json::json!({"session_id": &sid}));
    wait_receiver_state("127.0.0.1", rx_port, "idle", Duration::from_secs(5));
    // cast map 已清：再 cast.stop → stopped:false
    let r = dev.call("player", "player.cast.stop", serde_json::json!({"session_id": &sid}));
    assert_eq!(r["stopped"], false, "{r}");
    drop(lite);
}
