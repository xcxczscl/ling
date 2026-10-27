//! P7 验收（`docs/01` §9）：远程执行 + 跨设备接力。
//!
//! ```sh
//! cargo test -p player --test p7_acceptance -- --test-threads=1
//! ```
//!
//! 覆盖（每例**先 boot 接收侧**——其控制平面优先占 7917；发起侧退 7918+）：
//! - e1 会话接力：单向配对 A→B（A 的 rm peer 表记 B）+ 两侧**字节相同**
//!   movie.mkv（同 content_id）→ A 播 → seek 5.5 → 音量 0.42 → 字幕 -1 →
//!   暂停（force 同步）→ 轮询 B `player.control.status.sync_applied ≥1` →
//!   B play → `resumed:true` + 续播位≈A 暂停位 + 音量/字幕回放（§5.9）
//! - e2 远程执行链：caps（device_id / 直给 host:port / 缺省三种 peer 解析）→
//!   `remote.run` echoer（exit 5）→ `remote.status` 收束 → `remote.log`
//!   stdout/stderr 尾 → reader 夹具 + `remote.input` text → 读 B 沙箱
//!   `input.log` → `remote.stop`；video content_id → **P103** 跨机回传；
//!   缺 content_id → **P100**（发起侧关）；视频会话 id 当 run_id 输入 →
//!   **P100**（§5.15 命名空间安全面，跨机）
//! - e3 协议与安全面（单机）：直连 `/sync` LWW（新→applied、旧→stale、
//!   自→self，`sync_applied/ignored` 精确计数、不扰在播会话）；未知 run /
//!   视频会话 input、坏事件 → 400+**P100**；无配对 `remote.caps` → **P303**

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use ling_sdk::Value;

// ===== 内核引导（与 p0–p6_acceptance 同风格；Device 带 root 供 lan_addr/沙箱路径） =====

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
    root: PathBuf,
}

/// 起一台内核（rm + player）；`root` 即该设备的数据根。
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
    Device { kernel, root: root.to_path_buf() }
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

    fn player(&self, method: &str, args: Value) -> Value {
        self.call("player", &format!("player.{method}"), args)
    }

    fn try_player(&self, method: &str, args: Value) -> Result<Value, ling_sdk::CallError> {
        self.try_call("player", &format!("player.{method}"), args)
    }

    /// rm LAN 地址（transfer.json 的 lan_port；m3 同法）——配对用。
    fn lan_addr(&self) -> String {
        let text = std::fs::read_to_string(
            self.root
                .join("ling")
                .join("plugins")
                .join("resources-manager")
                .join("data")
                .join("transfer.json"),
        )
        .expect("transfer.json 应在 rm 激活时写出");
        let v: Value = serde_json::from_str(&text).unwrap();
        let port = v["lan_port"].as_u64().expect("lan_port") as u16;
        format!("127.0.0.1:{port}")
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

/// 单向配对：B 出码（`pairing_code` 在对端），A 侧 `device.pair`（A 的 rm peer 表记 B）。
/// 返回 B 的 rm device_id（A 视角对端 id，`peer` 入参用）。
fn pair_a_with_b(a: &Device, b: &Device) -> String {
    let code = b
        .call("resources-manager", "resources-manager.device.pairing_code", serde_json::json!({}))
        ["code"]
        .as_str()
        .expect("pairing_code.code")
        .to_string();
    let addr_b = b.lan_addr();
    let out = a.call(
        "resources-manager",
        "resources-manager.device.pair",
        serde_json::json!({ "code": code, "addr": addr_b }),
    );
    out["fingerprint"]
        .as_str()
        .unwrap_or_default()
        .starts_with("sha256:")
        .then(|| out["device_id"].as_str().unwrap().to_string())
        .expect("配对应写入指纹并回对端 device_id")
}

/// 轮询到条件满足（超时 panic，带回最后观测值）。
fn wait_for<T>(what: &str, timeout: Duration, mut f: impl FnMut() -> Option<T>) -> T {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(v) = f() {
            return v;
        }
        assert!(Instant::now() < deadline, "{what} 超时");
        std::thread::sleep(Duration::from_millis(120));
    }
}

// ===== 夹具 =====

fn ffmpeg(args: &[&str]) {
    let st = std::process::Command::new("ffmpeg")
        .args(args)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .expect("ffmpeg 运行");
    assert!(st.success(), "ffmpeg 失败：{args:?}");
}

/// 生成 10s mkv（mpeg4+aac+内嵌 srt，p6 同方）到 `dest/movie.mkv`。
/// 字幕放 dest 外（不入扫描）。
fn make_movie(dest: &Path) {
    std::fs::create_dir_all(dest).unwrap();
    let srt = dest.join("..").join(format!("sub-{}.srt", std::process::id()));
    std::fs::write(&srt, "1\n00:00:00,000 --> 00:00:09,500\nP7-SUBTITLE\n\n").unwrap();
    ffmpeg(&[
        "-y",
        "-f", "lavfi", "-i", "testsrc=duration=10:size=320x240:rate=15",
        "-f", "lavfi", "-i", "sine=frequency=440:duration=10",
        "-i", srt.to_str().unwrap(),
        "-map", "0", "-map", "1", "-map", "2",
        "-c:v", "mpeg4", "-q:v", "5",
        "-c:a", "aac",
        "-c:s", "srt",
        "-metadata:s:s:0", "language=eng",
        "-pix_fmt", "yuv420p",
        dest.join("movie.mkv").to_str().unwrap(),
    ]);
    let _ = std::fs::remove_file(&srt);
}

/// 扫描目录 → browse 行全量（location.add + scan.start + wait）。
fn scan_rows(dev: &Device, src_dir: &Path) -> Vec<Value> {
    dev.call(
        "resources-manager",
        "resources-manager.location.add",
        serde_json::json!({
            "path": src_dir.display().to_string(),
            "kind": "local",
            "label": format!("p7-{}", src_dir.parent().unwrap().file_name().unwrap().to_string_lossy()),
        }),
    );
    let loc = dev.call("resources-manager", "resources-manager.location.list", serde_json::json!({}))
        ["rows"][0]["id"]
        .as_str()
        .unwrap()
        .to_string();
    let scan = dev.call(
        "resources-manager",
        "resources-manager.scan.start",
        serde_json::json!({ "location_id": loc }),
    );
    let row = dev.wait_job(scan["job_id"].as_str().unwrap());
    assert_eq!(row["state"], "done", "扫描失败：{row}");
    let browse = dev.call(
        "resources-manager",
        "resources-manager.browse.query",
        serde_json::json!({ "page": { "limit": 100 } }),
    );
    browse["rows"].as_array().cloned().unwrap_or_default()
}

fn find_row<'a>(rows: &'a [Value], rel_path: &str) -> &'a Value {
    rows.iter()
        .find(|r| r["rel_path"] == rel_path)
        .unwrap_or_else(|| panic!("browse 应含 {rel_path}"))
}

/// e1：字节相同 movie.mkv 落两侧 src → 各自扫描 → 断言同 content_id。
fn setup_same_movie(
    a: &Device,
    b: &Device,
    shared: &Path,
    a_root: &Path,
    b_root: &Path,
) -> (String, String) {
    make_movie(shared);
    for (root, dev) in [(a_root, a), (b_root, b)] {
        let src = root.join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::copy(shared.join("movie.mkv"), src.join("movie.mkv")).unwrap();
        let _ = dev; // 各自扫描在下面做
    }
    let rows_a = scan_rows(a, &a_root.join("src"));
    let rows_b = scan_rows(b, &b_root.join("src"));
    let cid_a = find_row(&rows_a, "movie.mkv")["content_id"].as_str().unwrap().to_string();
    let cid_b = find_row(&rows_b, "movie.mkv")["content_id"].as_str().unwrap().to_string();
    assert_eq!(cid_a, cid_b, "字节相同 → content_id 必须一致（接力前提）");
    (cid_a, cid_b)
}

/// e2：B 侧扫描 echoer/reader/movie（rm 扫 `.exe` → app，p5 同法）。
struct AppFx {
    echoer_cid: String,
    reader_cid: String,
    movie_cid: String,
}

fn setup_apps(b: &Device, b_root: &Path) -> AppFx {
    let src = b_root.join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(
        src.join("echoer.exe"),
        "#!/bin/sh\necho console:remote-hello\necho err-line >&2\nexit 5\n",
    )
    .unwrap();
    std::fs::write(src.join("reader.exe"), "#!/bin/sh\ncat > input.log\n").unwrap();
    make_movie(&src);
    let rows = scan_rows(b, &src);
    AppFx {
        echoer_cid: find_row(&rows, "echoer.exe")["content_id"].as_str().unwrap().to_string(),
        reader_cid: find_row(&rows, "reader.exe")["content_id"].as_str().unwrap().to_string(),
        movie_cid: find_row(&rows, "movie.mkv")["content_id"].as_str().unwrap().to_string(),
    }
}

// ===== 播放观测（p6 同法：直读 player.status） =====

fn status_of(dev: &Device, sid: &str) -> Value {
    let wrap = dev.player("status", serde_json::json!({"session_id": sid}));
    wrap["sessions"]
        .as_array()
        .and_then(|a| a.first().cloned())
        .unwrap_or_else(|| panic!("status 中无会话 {sid}：{wrap}"))
}

fn wait_duration(dev: &Device, sid: &str) -> Value {
    wait_for("duration 识别", Duration::from_secs(6), || {
        let s = status_of(dev, sid);
        (s["duration_sec"].as_f64().unwrap_or(0.0) > 9.0).then_some(s)
    })
}

fn wait_position(dev: &Device, sid: &str, timeout: Duration, pred: impl Fn(f64) -> bool, what: &str) -> f64 {
    wait_for(what, timeout, || {
        let p = status_of(dev, sid)["position_sec"].as_f64().unwrap_or(-1.0);
        pred(p).then_some(p)
    })
}

/// `player.control.status` 便捷读。
fn control_status(dev: &Device) -> Value {
    dev.player("control.status", serde_json::json!({}))
}

// ===== e1 会话接力 =====

/// e1：A 播放现场（暂停位/音量/字幕）经 §5.9 同步落 B，B play 从 A 进度续播。
#[test]
fn e1_handoff_pause_position_volume_subtitle() {
    std::env::set_var("PLAYER_VO", "null");
    let rm_dir = install_dir("rm", &rm_root(), "resources-manager", "resources-manager");
    let pl_dir = install_dir("player", &player_root(), "player", "player");
    let tmp = tempfile::tempdir().unwrap();
    let b_root = tmp.path().join("b");
    let a_root = tmp.path().join("a");

    // 先 boot 接收侧 B（控 7917），后 boot 发起侧 A（退 7918+）。
    let b = boot(&rm_dir, &pl_dir, &b_root);
    let a = boot(&rm_dir, &pl_dir, &a_root);
    let b_ctl = control_status(&b);
    assert_eq!(b_ctl["enabled"], true, "{b_ctl}");
    assert_eq!(b_ctl["port"], 7917, "接收侧占首选口：{b_ctl}");

    // 单向配对 A→B；A 的同步目标 = 对端 host+7917。
    let _b_rm_id = pair_a_with_b(&a, &b);
    let a_ctl = control_status(&a);
    assert_ne!(a_ctl["port"], 7917, "发起侧应退让端口：{a_ctl}");
    assert_eq!(
        a_ctl["sync_targets"],
        serde_json::json!(["127.0.0.1:7917"]),
        "peer 表推导控制口：{a_ctl}"
    );
    assert_eq!(a_ctl["sync_applied"], 0, "发起侧不应收到同步：{a_ctl}");

    let (cid_a, _cid_b) = setup_same_movie(&a, &b, &tmp.path().join("shared"), &a_root, &b_root);

    // --- A：播 → seek 5.5 → 音量 0.42 → 字幕 -1 → 暂停（force 同步） ---
    let sid = a.player("play", serde_json::json!({"content_id": &cid_a}))
        ["session_id"]
        .as_str()
        .unwrap()
        .to_string();
    wait_duration(&a, &sid);
    a.player("seek", serde_json::json!({"session_id": &sid, "position_sec": 5.5}));
    wait_position(&a, &sid, Duration::from_secs(3), |p| (p - 5.5).abs() < 0.5, "seek 落点");
    a.player("set_volume", serde_json::json!({"session_id": &sid, "volume": 0.42}));
    let r = a.player("set_subtitle", serde_json::json!({"session_id": &sid, "track": -1}));
    assert_eq!(r["subtitle_track"], -1, "{r}");
    a.player("pause", serde_json::json!({"session_id": &sid}));
    let paused = wait_for("A 暂停态", Duration::from_secs(2), || {
        let s = status_of(&a, &sid);
        (s["phase"] == "paused").then_some(s)
    });
    let p_saved = paused["position_sec"].as_f64().unwrap();
    assert!((p_saved - 5.5).abs() < 2.5, "暂停位应在 seek 附近：{p_saved}");
    assert_eq!(paused["volume"].as_f64(), Some(0.42), "{paused}");

    // --- B：收到同步（至少一次；最后合入行 = A 暂停现场） ---
    let applied = wait_for("B 收到同步", Duration::from_secs(6), || {
        let s = control_status(&b);
        let n = s["sync_applied"].as_u64().unwrap_or(0);
        (n >= 1).then_some(n)
    });
    assert!(applied >= 1, "{applied}");

    // --- B：play 同一 content_id → 续播 A 进度 ---
    let opened = b.player("play", serde_json::json!({"content_id": &cid_a}));
    assert_eq!(opened["resumed"], true, "接力续播：{opened}");
    let resume_pos = opened["resume_position_sec"].as_f64().unwrap();
    assert!(
        (resume_pos - p_saved).abs() < 0.3,
        "续播位应对上 A 暂停位：saved={p_saved} resume={resume_pos}"
    );
    let sid_b = opened["session_id"].as_str().unwrap().to_string();
    let st = wait_duration(&b, &sid_b);
    let p_first = st["position_sec"].as_f64().unwrap();
    assert!(
        p_first >= p_saved - 1.0 && p_first < p_saved + 2.5,
        "B 起播位应在暂停位附近：{p_first}（saved {p_saved}）"
    );
    assert!((st["volume"].as_f64().unwrap() - 0.42).abs() < 1e-6, "音量接力：{st}");
    assert_eq!(st["subtitle_track"].as_i64(), Some(-1), "字幕接力：{st}");
    // 播放真恢复（继续前进，非停在原地）
    wait_position(&b, &sid_b, Duration::from_secs(6), |p| p > p_saved + 0.5, "B 续播前进");

    b.player("stop", serde_json::json!({"session_id": &sid_b}));
    b.kernel.shutdown().expect("停机 B");
    a.kernel.shutdown().expect("停机 A");
}

// ===== e2 远程执行链 =====

/// e2：caps → run(echoer exit 5) → status/log → reader+input → stop；
/// P103/P100 跨机回传；§5.15 视频会话拿不到输入（跨机 P100）。
#[test]
fn e2_remote_run_chain_and_error_faces() {
    std::env::set_var("PLAYER_VO", "null");
    let rm_dir = install_dir("rm", &rm_root(), "resources-manager", "resources-manager");
    let pl_dir = install_dir("player", &player_root(), "player", "player");
    let tmp = tempfile::tempdir().unwrap();
    let b_root = tmp.path().join("b");
    let a_root = tmp.path().join("a");

    let b = boot(&rm_dir, &pl_dir, &b_root);
    let a = boot(&rm_dir, &pl_dir, &a_root);
    assert_eq!(control_status(&b)["port"], 7917, "接收侧占首选口");
    let b_rm_id = pair_a_with_b(&a, &b);
    let fx = setup_apps(&b, &b_root);

    // --- caps：三种 peer 解析（device_id / 直给 host:port / 缺省单配对） ---
    let caps = a.player("remote.caps", serde_json::json!({"peer": &b_rm_id}));
    assert_eq!(caps["caps"]["run"], true, "{caps}");
    let caps2 = a.player("remote.caps", serde_json::json!({"peer": "127.0.0.1:7917"}));
    assert_eq!(caps2["caps"]["sync"], true, "{caps2}");
    let caps3 = a.player("remote.caps", serde_json::json!({}));
    assert_eq!(caps3["caps"]["input"], true, "缺省单配对解析：{caps3}");

    // --- 发起侧关：缺 content_id → P100（不发包） ---
    let e = a.try_player("remote.run", serde_json::json!({})).unwrap_err();
    assert_eq!(e.code(), "P100", "{e:?}");

    // --- run echoer：exit 5 + stdout/stderr 尾 ---
    let run = a.player("remote.run", serde_json::json!({"content_id": &fx.echoer_cid}));
    let echoer_id = run["run_id"].as_str().unwrap().to_string();
    assert!(run["pid"].as_i64().unwrap_or(0) > 0, "{run}");
    assert!(
        run["isolation"].get("fs").is_some(),
        "远程执行同样走 P5 沙箱全链：{run}"
    );
    let st = wait_for("echoer 退出", Duration::from_secs(8), || {
        let s = a.player("remote.status", serde_json::json!({"run_id": &echoer_id}));
        (s["state"] == "exited").then_some(s)
    });
    assert_eq!(st["exit_code"].as_i64(), Some(5), "{st}");
    let log = a.player("remote.log", serde_json::json!({"run_id": &echoer_id}));
    assert!(
        log["data"].as_str().unwrap_or("").contains("console:remote-hello"),
        "stdout 尾：{log}"
    );
    let log_err = a.player(
        "remote.log",
        serde_json::json!({"run_id": &echoer_id, "stream": "stderr"}),
    );
    assert!(
        log_err["data"].as_str().unwrap_or("").contains("err-line"),
        "stderr 尾：{log_err}"
    );

    // --- reader：stdin 输入投递（§5.15 text → JSONL → 沙箱 input.log） ---
    let run2 = a.player("remote.run", serde_json::json!({"content_id": &fx.reader_cid}));
    let reader_id = run2["run_id"].as_str().unwrap().to_string();
    let sandbox_dir = run2["sandbox_dir"].as_str().unwrap().to_string();
    let inp = a.player(
        "remote.input",
        serde_json::json!({
            "run_id": &reader_id,
            "event": {"type": "input.text", "text": "hello"},
        }),
    );
    assert_eq!(inp["accepted"], true, "{inp}");
    // B 侧沙箱文件（同机直读）：cat 收到 JSONL 行落盘
    let input_log = Path::new(&sandbox_dir).join("input.log");
    wait_for("input.log 落 hello", Duration::from_secs(4), || {
        std::fs::read_to_string(&input_log)
            .ok()
            .filter(|t| t.contains("hello"))
    });
    // 坜格式：一行 JSON（JSONL transport 登记口径）
    let text = std::fs::read_to_string(&input_log).unwrap();
    assert!(text.contains("input.text"), "JSONL 行含事件类型：{text}");

    // --- stop：显式停（进程组杀 + 监控收束） ---
    let stop = a.player("remote.stop", serde_json::json!({"run_id": &reader_id}));
    assert_eq!(stop["stopped"], true, "{stop}");
    wait_for("reader 退出", Duration::from_secs(4), || {
        let s = a.player("remote.status", serde_json::json!({"run_id": &reader_id}));
        (s["state"] == "exited").then_some(s)
    });

    // --- §5.15 跨机安全面：视频会话 id 当 run_id 输入 → P100 ---
    let movie_sid = b
        .player("play", serde_json::json!({"content_id": &fx.movie_cid}))
        ["session_id"]
        .as_str()
        .unwrap()
        .to_string();
    let e = a
        .try_player(
            "remote.input",
            serde_json::json!({
                "run_id": &movie_sid,
                "event": {"type": "input.key", "key": "Enter", "action": "down"},
            }),
        )
        .unwrap_err();
    assert_eq!(e.code(), "P100", "视频会话不接受输入（§5.15）：{e:?}");

    // --- P103 跨机回传：video content 远程执行 ---
    let e = a
        .try_player("remote.run", serde_json::json!({"content_id": &fx.movie_cid}))
        .unwrap_err();
    assert_eq!(e.code(), "P103", "远程执行仅 app/game：{e:?}");

    b.player("stop", serde_json::json!({"session_id": &movie_sid}));
    b.kernel.shutdown().expect("停机 B");
    a.kernel.shutdown().expect("停机 A");
}

// ===== e3 协议与安全面（单机直连 HTTP） =====

fn http_post(port: u16, path: &str, body: &Value) -> (u16, Value) {
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();
    let resp = client
        .post(format!("http://127.0.0.1:{port}{path}"))
        .header("content-type", "application/json")
        .body(body.to_string())
        .send()
        .expect("POST 可达");
    let status = resp.status().as_u16();
    let v: Value = resp
        .text()
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or(Value::Null);
    (status, v)
}

fn http_get(port: u16, path: &str) -> (u16, Value) {
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();
    let resp = client
        .get(format!("http://127.0.0.1:{port}{path}"))
        .send()
        .expect("GET 可达");
    let status = resp.status().as_u16();
    let v: Value = resp
        .text()
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or(Value::Null);
    (status, v)
}

/// e3：LWW 三态 + 精确计数 + §5.15/入参拒绝面 + P303 未配对 + 不扰在播。
#[test]
fn e3_sync_lww_and_safety_faces() {
    std::env::set_var("PLAYER_VO", "null");
    let rm_dir = install_dir("rm", &rm_root(), "resources-manager", "resources-manager");
    let pl_dir = install_dir("player", &player_root(), "player", "player");
    let tmp = tempfile::tempdir().unwrap();
    let dev = boot(&rm_dir, &pl_dir, tmp.path());
    let port = control_status(&dev)["port"].as_u64().unwrap() as u16;
    assert_eq!(port, 7917, "单机占首选口");

    // 在播会话（/sync 只落库、不扰它）
    let src = tmp.path().join("src");
    make_movie(&src);
    let rows = scan_rows(&dev, &src);
    let cid = find_row(&rows, "movie.mkv")["content_id"].as_str().unwrap().to_string();
    let sid = dev.player("play", serde_json::json!({"content_id": &cid}))
        ["session_id"]
        .as_str()
        .unwrap()
        .to_string();
    wait_duration(&dev, &sid);
    dev.player("set_volume", serde_json::json!({"session_id": &sid, "volume": 0.7}));

    // --- 直连 GET 面 ---
    let (st, caps) = http_get(port, "/remote/caps");
    assert_eq!(st, 200, "{caps}");
    assert_eq!(caps["caps"]["run"], true, "{caps}");
    let (st, cs) = http_get(port, "/control/status");
    assert_eq!(st, 200, "{cs}");
    assert_eq!(cs["port"], 7917, "{cs}");
    let my_device_id = cs["device_id"].as_str().unwrap().to_string();
    assert_eq!(my_device_id.len(), 16, "本机 device_id：{cs}");

    // --- /sync LWW 三态 ---
    let base = serde_json::json!({
        "type": "session.sync",
        "content_id": "blake3:e3probe",
        "position_sec": 1234.5,
        "volume": 0.9,
        "subtitle_track": 0,
    });
    let newer = |at: i64| {
        let mut v = base.clone();
        v["updated_at"] = serde_json::json!(at);
        v["device_id"] = serde_json::json!("dv_e3peer");
        v
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    let (st, r) = http_post(port, "/sync", &newer(now + 10_000));
    assert_eq!(st, 200, "{r}");
    assert_eq!(r["applied"], true, "新行合入：{r}");
    let (st, r) = http_post(port, "/sync", &newer(now + 5_000));
    assert_eq!(st, 200, "{r}");
    assert_eq!(r["applied"], false, "旧报丢弃：{r}");
    assert_eq!(r["reason"], "stale", "{r}");
    let mut selfr = newer(now + 20_000);
    selfr["device_id"] = serde_json::json!(my_device_id);
    let (st, r) = http_post(port, "/sync", &selfr);
    assert_eq!(st, 200, "{r}");
    assert_eq!(r["applied"], false, "自报丢弃：{r}");
    assert_eq!(r["reason"], "self", "{r}");

    // 计数精确（单机无对端 → 无自发推送干扰）
    let cs = control_status(&dev);
    assert_eq!(cs["sync_applied"], 1, "{cs}");
    assert_eq!(cs["sync_ignored"], 2, "{cs}");

    // --- /sync 不扰在播会话（仅落库） ---
    let st_now = status_of(&dev, &sid);
    assert!(
        matches!(st_now["phase"].as_str(), Some("playing") | Some("paused")),
        "在播会话不受 /sync 影响：{st_now}"
    );
    assert!(
        st_now["position_sec"].as_f64().unwrap() < 60.0,
        "在播进度未被同步报文污染：{st_now}"
    );
    assert!((st_now["volume"].as_f64().unwrap() - 0.7).abs() < 1e-6, "音量仍为本机会话值：{st_now}");

    // --- 400 + P100 拒绝面（直连） ---
    let bad = serde_json::json!({"event": {"type": "input.text", "text": "x"}});
    let (st, r) = http_post(port, "/remote/run/run-nonexistent/input", &bad);
    assert_eq!(st, 400, "{r}");
    assert_eq!(r["error"]["code"], "P100", "未知 run 输入：{r}");
    let (st, r) = http_post(
        port,
        &format!("/remote/run/{sid}/input"),
        &bad,
    );
    assert_eq!(st, 400, "{r}");
    assert_eq!(r["error"]["code"], "P100", "视频会话输入拒绝（§5.15）：{r}");
    let (st, r) = http_post(
        port,
        "/remote/run/run-x/input",
        &serde_json::json!({"event": {"type": "input.bogus"}}),
    );
    assert_eq!(st, 400, "{r}");
    assert_eq!(r["error"]["code"], "P100", "坏事件类型：{r}");
    let (st, r) = http_get(port, "/remote/run/run-nope");
    assert_eq!(st, 400, "{r}");
    assert_eq!(r["error"]["code"], "P100", "未知 run 查询：{r}");
    let (st, r) = http_get(port, "/totally/unknown");
    assert_eq!(st, 400, "{r}");
    assert_eq!(r["error"]["code"], "P100", "未知路径：{r}");

    // --- 方法面：无配对 → P303；缺 content_id → P100（先于对端解析） ---
    let e = dev.try_player("remote.caps", serde_json::json!({})).unwrap_err();
    assert_eq!(e.code(), "P303", "无配对对端：{e:?}");
    let e = dev.try_player("remote.run", serde_json::json!({})).unwrap_err();
    assert_eq!(e.code(), "P100", "{e:?}");

    dev.player("stop", serde_json::json!({"session_id": &sid}));
    dev.kernel.shutdown().expect("停机");
}

