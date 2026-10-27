//! P6 验收（`docs/01` §9）：会话持久化——player.db（§5.11）跨重启续播。
//!
//! ```sh
//! cargo test -p player --test p6_acceptance -- --test-threads=1
//! ```
//!
//! 覆盖（每例内 `shutdown` → 同一数据根重 boot，验证真持久化而非内存态）：
//! - e1 续播主链：播 → seek 5.5 → volume 0.42 → set_subtitle(-1) → 暂停
//!   （pause 钩子写穿）→ shutdown → boot B 同根：同一 content_id play →
//!   `resumed:true` + `resume_position_sec`≈暂停位、进度从暂停位继续前进（非
//!   从 0）、volume/subtitle 回放；首播（无已存行）`resumed:false`
//! - e2 输出选择态持久化（#39）：outputs.set(audio id) → shutdown → boot B
//!   → outputs.list `selected` 同 id（activate 恢复）
//! - e3 stop 路径与 Finished 归零：seek 后 stop（stop 前写穿）→ 重启续播
//!   ≈seek 位；整片播完 Finished → 重启 `resumed:false` / 位置归 0

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use ling_sdk::Value;

// ===== 内核引导（与 p0–p5_acceptance 同风格） =====

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

/// 起一台内核（同 root 反复调用 = 同一数据根重 boot，P6 续播的「重启」面）。
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

// ===== 夹具（10s mkv：mpeg4 视频 + aac 音频 + 内嵌 srt 字幕轨） =====

fn ffmpeg(args: &[&str]) {
    let st = std::process::Command::new("ffmpeg")
        .args(args)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .expect("ffmpeg 运行");
    assert!(st.success(), "ffmpeg 失败：{args:?}");
}

/// 建库扫描 movie.mkv → (placement_id, content_id)。
fn setup(dev: &Device, root: &Path) -> (i64, String) {
    let src_dir = root.join("src");
    std::fs::create_dir_all(&src_dir).unwrap();

    // 字幕放 src 外（不入扫描，p1 同法）；覆盖整片时长。
    let srt = root.join("sub.srt");
    std::fs::write(
        &srt,
        "1\n00:00:00,000 --> 00:00:09,500\nP6-SUBTITLE\n\n",
    )
    .unwrap();

    let mkv = src_dir.join("movie.mkv");
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
        mkv.to_str().unwrap(),
    ]);

    dev.call(
        "resources-manager",
        "resources-manager.location.add",
        serde_json::json!({"path": src_dir.display().to_string(), "kind": "local", "label": "p6-src"}),
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
    let row = browse["rows"].as_array().unwrap().iter()
        .find(|r| r["rel_path"] == "movie.mkv")
        .expect("browse 应含 movie.mkv");
    (
        row["placement_id"].as_i64().unwrap(),
        row["content_id"].as_str().unwrap().to_string(),
    )
}

// ===== status 观测 =====

fn status_of(dev: &Device, sid: &str) -> Value {
    let wrap = dev.call("player", "player.status", serde_json::json!({"session_id": sid}));
    wrap["sessions"]
        .as_array()
        .and_then(|a| a.first().cloned())
        .unwrap_or_else(|| panic!("status 中无会话 {sid}：{wrap}"))
}

fn wait_status(dev: &Device, sid: &str, timeout: Duration, pred: impl Fn(&Value) -> bool, what: &str) -> Value {
    let deadline = Instant::now() + timeout;
    let mut last = status_of(dev, sid);
    while Instant::now() < deadline {
        if pred(&last) {
            return last;
        }
        std::thread::sleep(Duration::from_millis(150));
        last = status_of(dev, sid);
    }
    panic!("{what} 超时；最后状态：{last}");
}

fn wait_position(dev: &Device, sid: &str, timeout: Duration, pred: impl Fn(f64) -> bool, what: &str) -> f64 {
    let deadline = Instant::now() + timeout;
    loop {
        let p = status_of(dev, sid)["position_sec"].as_f64().unwrap_or(-1.0);
        if pred(p) {
            return p;
        }
        assert!(Instant::now() < deadline, "{what} 超时；最后 position={p}");
        std::thread::sleep(Duration::from_millis(150));
    }
}

/// 播到 duration 可读（解复用就绪的信号，p0 同法）。
fn wait_duration(dev: &Device, sid: &str) -> Value {
    wait_status(
        dev,
        sid,
        Duration::from_secs(6),
        |s| s["duration_sec"].as_f64().unwrap_or(0.0) > 9.0,
        "duration 识别",
    )
}

// ===== 用例 =====

/// e1 续播主链跨重启：暂停位/音量/字幕在 boot A 落库，boot B 同根读回续播。
#[test]
fn e1_resume_position_volume_subtitle_across_restart() {
    std::env::set_var("PLAYER_VO", "null");
    let rm_dir = install_dir("rm", &rm_root(), "resources-manager", "resources-manager");
    let pl_dir = install_dir("player", &player_root(), "player", "player");
    let tmp = tempfile::tempdir().unwrap();
    let dev = boot(&rm_dir, &pl_dir, tmp.path());
    let (_pid, cid) = setup(&dev, tmp.path());

    // --- boot A：首播（无已存行）→ resumed:false ---
    let opened = dev.call("player", "player.play", serde_json::json!({"content_id": &cid}));
    let sid = opened["session_id"].as_str().unwrap().to_string();
    assert_eq!(opened["resumed"], false, "首播不应续播：{opened}");
    assert_eq!(opened["resume_position_sec"].as_f64(), Some(0.0), "{opened}");
    wait_duration(&dev, &sid);

    // seek 5.5 → 落点；音量 0.42；字幕关（-1，区别于 mpv 自动选中的 1）
    dev.call("player", "player.seek", serde_json::json!({"session_id": &sid, "position_sec": 5.5}));
    wait_position(&dev, &sid, Duration::from_secs(3), |p| (p - 5.5).abs() < 0.5, "seek 落点");
    dev.call("player", "player.set_volume", serde_json::json!({"session_id": &sid, "volume": 0.42}));
    let r = dev.call("player", "player.set_subtitle", serde_json::json!({"session_id": &sid, "track": -1}));
    assert_eq!(r["subtitle_track"], -1, "{r}");

    // 暂停（pause 钩子写穿当前进度），记下暂停位
    dev.call("player", "player.pause", serde_json::json!({"session_id": &sid}));
    let paused = wait_status(&dev, &sid, Duration::from_secs(2), |s| s["phase"] == "paused", "phase paused");
    let p_saved = paused["position_sec"].as_f64().unwrap();
    assert!((p_saved - 5.5).abs() < 2.0, "暂停位应在 seek 附近：{p_saved}");
    assert_eq!(paused["volume"].as_f64(), Some(0.42), "暂停快照音量：{paused}");
    assert_eq!(paused["subtitle_track"].as_i64(), Some(-1), "暂停快照字幕：{paused}");

    dev.kernel.shutdown().expect("停机 A（flush 落库）");

    // --- boot B：同根重起 → 续播 ---
    let dev = boot(&rm_dir, &pl_dir, tmp.path());
    let opened = dev.call("player", "player.play", serde_json::json!({"content_id": &cid}));
    let sid2 = opened["session_id"].as_str().unwrap().to_string();
    assert_eq!(opened["resumed"], true, "重启后续播：{opened}");
    let resume_pos = opened["resume_position_sec"].as_f64().unwrap();
    assert!(
        (resume_pos - p_saved).abs() < 0.3,
        "resume 位置应对上暂停位：saved={p_saved} resume={resume_pos}"
    );

    // 首读状态：位置已在暂停位附近（非从 0 起），音量/字幕已回放
    let st = wait_duration(&dev, &sid2);
    let p_first = st["position_sec"].as_f64().unwrap();
    assert!(
        p_first >= p_saved - 1.0 && p_first < p_saved + 2.5,
        "续播起点应在暂停位附近：{p_first}（saved {p_saved}）"
    );
    assert!(
        (st["volume"].as_f64().unwrap() - 0.42).abs() < 1e-6,
        "音量回放：{st}"
    );
    assert_eq!(st["subtitle_track"].as_i64(), Some(-1), "字幕回放：{st}");

    // 从暂停位继续前进（播放真恢复，不是停在原地）
    wait_position(&dev, &sid2, Duration::from_secs(6), |p| p > p_saved + 0.5, "续播前进");

    dev.call("player", "player.stop", serde_json::json!({"session_id": &sid2}));
    dev.kernel.shutdown().expect("停机 B");
}

/// e2 输出选择态随设置持久化（#39）：set → 重启 → activate 恢复 selected。
#[test]
fn e2_selected_output_survives_restart() {
    std::env::set_var("PLAYER_VO", "null");
    let rm_dir = install_dir("rm", &rm_root(), "resources-manager", "resources-manager");
    let pl_dir = install_dir("player", &player_root(), "player", "player");
    let tmp = tempfile::tempdir().unwrap();
    let dev = boot(&rm_dir, &pl_dir, tmp.path());

    let out = dev.call("player", "player.outputs.list", serde_json::json!({}));
    let audio_id = out["rows"].as_array().unwrap().iter()
        .find(|r| r["kind"] == "audio" && r["id"] == "auto")
        .map(|r| r["id"].as_str().unwrap().to_string())
        .expect("audio auto 行");
    let r = dev.call("player", "player.outputs.set", serde_json::json!({"output_id": &audio_id}));
    assert_eq!(r["kind"], "audio", "{r}");
    let list = dev.call("player", "player.outputs.list", serde_json::json!({}));
    assert_eq!(list["selected"], audio_id.as_str(), "{list}");

    dev.kernel.shutdown().expect("停机 A");

    // boot B：activate 从 setting 表恢复选择
    let dev = boot(&rm_dir, &pl_dir, tmp.path());
    let list = dev.call("player", "player.outputs.list", serde_json::json!({}));
    assert_eq!(
        list["selected"],
        audio_id.as_str(),
        "重启后 selected 应从 player.db 恢复：{list}"
    );
    dev.kernel.shutdown().expect("停机 B");
}

/// e3 stop 路径（stop 前写穿）+ Finished 归零：两种收束的重启续播语义。
#[test]
fn e3_stop_persists_and_finished_resets_on_restart() {
    std::env::set_var("PLAYER_VO", "null");
    let rm_dir = install_dir("rm", &rm_root(), "resources-manager", "resources-manager");
    let pl_dir = install_dir("player", &player_root(), "player", "player");
    let tmp = tempfile::tempdir().unwrap();
    let dev = boot(&rm_dir, &pl_dir, tmp.path());
    let (_pid, cid) = setup(&dev, tmp.path());

    // --- stop 路径：seek 3.3 → stop（先写穿再停）→ 重启续播 ≈3.3 ---
    let sid = dev.call("player", "player.play", serde_json::json!({"content_id": &cid}))
        ["session_id"].as_str().unwrap().to_string();
    wait_duration(&dev, &sid);
    dev.call("player", "player.seek", serde_json::json!({"session_id": &sid, "position_sec": 3.3}));
    let p_before = wait_position(&dev, &sid, Duration::from_secs(3), |p| (p - 3.3).abs() < 0.5, "seek 落点");
    dev.call("player", "player.stop", serde_json::json!({"session_id": &sid}));
    dev.kernel.shutdown().expect("停机 A");

    let dev = boot(&rm_dir, &pl_dir, tmp.path());
    let opened = dev.call("player", "player.play", serde_json::json!({"content_id": &cid}));
    let sid2 = opened["session_id"].as_str().unwrap().to_string();
    assert_eq!(opened["resumed"], true, "stop 落库后应续播：{opened}");
    let resume_pos = opened["resume_position_sec"].as_f64().unwrap();
    assert!(
        (resume_pos - p_before).abs() < 0.4,
        "续播位应对上 stop 前位置：before={p_before} resume={resume_pos}"
    );
    dev.call("player", "player.stop", serde_json::json!({"session_id": &sid2}));
    dev.kernel.shutdown().expect("停机 B");

    // --- Finished 归零：播到尾（事件/停机 flush → position 0）→ 重启从头 ---
    let dev = boot(&rm_dir, &pl_dir, tmp.path());
    let sid = dev.call("player", "player.play", serde_json::json!({"content_id": &cid}))
        ["session_id"].as_str().unwrap().to_string();
    wait_duration(&dev, &sid);
    dev.call("player", "player.seek", serde_json::json!({"session_id": &sid, "position_sec": 9.4}));
    wait_status(&dev, &sid, Duration::from_secs(8), |s| s["phase"] == "finished", "Finished 收束");
    dev.kernel.shutdown().expect("停机 C");

    let dev = boot(&rm_dir, &pl_dir, tmp.path());
    let opened = dev.call("player", "player.play", serde_json::json!({"content_id": &cid}));
    assert_eq!(opened["resumed"], false, "播完重启应从头：{opened}");
    assert_eq!(opened["resume_position_sec"].as_f64(), Some(0.0), "{opened}");
    let sid3 = opened["session_id"].as_str().unwrap().to_string();
    let st = wait_duration(&dev, &sid3);
    assert!(
        st["position_sec"].as_f64().unwrap() < 1.5,
        "Finished 归零后从头播放：{st}"
    );
    dev.call("player", "player.stop", serde_json::json!({"session_id": &sid3}));
    dev.kernel.shutdown().expect("停机 D");
}
