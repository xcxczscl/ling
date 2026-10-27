//! P0 验收（`docs/01` §9）：stream.open → libmpv 解码 → 本机输出 + 控制面。
//!
//! ```sh
//! cargo test -p player --test p0_acceptance -- --test-threads=1
//! ```
//!
//! 无显示环境（CI）自动走 `vo=null ao=null`（backend::detect_headless）——
//! 验证解码、进度、暂停冻结、seek、phase 收束与错误码；真实渲染留桌面端。
//!
//! 覆盖：
//! - e1 play（placement_id）→ 进度前进、duration 正确、pause 冻结、resume 恢复、
//!   seek 落点、set_volume、status 列表
//! - e2 content_id 主路径（browse 检索本机在线放置）
//! - e3 播完 Finished 收束；stop 摘除 → P401；坏 placement → P101
//! - e4 player.ui.page 控制面直出

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use ling_sdk::Value;

// ===== 与 rm stream_acceptance 相同风格的最小内核引导 =====

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
    let lib = format!("{}{}{}", std::env::consts::DLL_PREFIX, pkg.replace('-', "_"), std::env::consts::DLL_SUFFIX);
    let src = root.join("target/debug").join(&lib);
    assert!(src.is_file(), "cdylib 未找到：{}", src.display());
    src
}

/// 组装安装目录：`plugin.toml` + `<插件编号>.so`（loader 按 id+dlopen 校验）。
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

/// 生成 10s 测试视频 + 建库扫描 → (设备, placement_id, content_id, 文件字节数)。
fn setup(dev: &Device, root: &Path) -> (i64, String, u64) {
    let src_dir = root.join("src");
    std::fs::create_dir_all(&src_dir).unwrap();
    let mp4 = src_dir.join("movie.mp4");
    // mpeg4 编码器内置，避免依赖 libx264；10s 头寸给控制用例留余量。
    let st = std::process::Command::new("ffmpeg")
        .args([
            "-y", "-f", "lavfi", "-i", "testsrc=duration=10:size=320x240:rate=15",
            "-pix_fmt", "yuv420p", "-c:v", "mpeg4", "-q:v", "5",
            mp4.to_str().unwrap(),
        ])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .expect("ffmpeg 生成测试视频");
    assert!(st.success());
    let file_len = std::fs::metadata(&mp4).unwrap().len();

    dev.call(
        "resources-manager",
        "resources-manager.location.add",
        serde_json::json!({"path": src_dir.display().to_string(), "kind": "local", "label": "p0-src"}),
    );
    let loc = dev.call("resources-manager", "resources-manager.location.list", serde_json::json!({}))
        ["rows"][0]["id"].as_str().unwrap().to_string();
    let scan = dev.call("resources-manager", "resources-manager.scan.start", serde_json::json!({"location_id": loc}));
    let row = dev.wait_job(scan["job_id"].as_str().unwrap());
    assert_eq!(row["state"], "done", "扫描失败：{row}");

    let browse = dev.call("resources-manager", "resources-manager.browse.query", serde_json::json!({"page": {"limit": 50}}));
    let row = browse["rows"].as_array().unwrap().iter()
        .find(|r| r["rel_path"] == "movie.mp4")
        .expect("browse 应含 movie.mp4");
    (row["placement_id"].as_i64().unwrap(), row["content_id"].as_str().unwrap().to_string(), file_len)
}

/// 取单会话状态（status 响应是 {sessions:[...], playing} 包装 → 取首条）。
fn status_of(dev: &Device, sid: &str) -> Value {
    let wrap = dev.call("player", "player.status", serde_json::json!({"session_id": sid}));
    wrap["sessions"]
        .as_array()
        .and_then(|a| a.first().cloned())
        .unwrap_or_else(|| panic!("status 中无会话 {sid}：{wrap}"))
}

/// 轮询 status 直到谓词满足；超时 panic（带最后快照）。
fn wait_status(dev: &Device, sid: &str, timeout: Duration, pred: impl Fn(&Value) -> bool, what: &str) -> Value {
    let deadline = Instant::now() + timeout;
    let mut last = status_of(dev, sid);
    while Instant::now() < deadline {
        if pred(&last) {
            return last;
        }
        std::thread::sleep(Duration::from_millis(200));
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

// ===== 用例 =====

#[test]
fn e1_play_pause_seek_volume_status() {
    std::env::set_var("PLAYER_VO", "null");
    let rm_dir = install_dir("rm", &rm_root(), "resources-manager", "resources-manager");
    let pl_dir = install_dir("player", &player_root(), "player", "player");
    let tmp = tempfile::tempdir().unwrap();
    let dev = boot(&rm_dir, &pl_dir, tmp.path());
    let (pid, _cid, file_len) = setup(&dev, tmp.path());

    // play（placement_id 主路径）
    let opened = dev.call("player", "player.play", serde_json::json!({"placement_id": pid}));
    let sid = opened["session_id"].as_str().unwrap().to_string();
    assert!(sid.starts_with("ps_"), "会话号形态 ps_*：{sid}");
    assert!(opened["content_len"].as_u64().unwrap() > 0);

    // duration 已知（解码头已读）
    let st = wait_status(&dev, &sid, Duration::from_secs(6),
        |s| s["duration_sec"].as_f64().unwrap_or(0.0) > 9.0, "duration 识别");
    assert_eq!(st["phase"], "playing");
    assert_eq!(st["playing"], true);

    // 进度前进（headless 实时时钟）
    wait_position(&dev, &sid, Duration::from_secs(4), |p| p > 0.2, "position 前进");

    // pause → phase paused + position 冻结
    dev.call("player", "player.pause", serde_json::json!({"session_id": &sid}));
    let paused = wait_status(&dev, &sid, Duration::from_secs(2), |s| s["phase"] == "paused", "phase paused");
    let p1 = paused["position_sec"].as_f64().unwrap();
    std::thread::sleep(Duration::from_millis(700));
    let p2 = status_of(&dev, &sid)["position_sec"].as_f64().unwrap();
    assert!((p2 - p1).abs() < 0.15, "暂停必须冻结：{p1} → {p2}");

    // resume → 恢复前进
    dev.call("player", "player.resume", serde_json::json!({"session_id": &sid}));
    wait_status(&dev, &sid, Duration::from_secs(2), |s| s["phase"] == "playing", "phase playing");
    let p3 = status_of(&dev, &sid)["position_sec"].as_f64().unwrap();
    std::thread::sleep(Duration::from_millis(900));
    let p4 = status_of(&dev, &sid)["position_sec"].as_f64().unwrap();
    assert!(p4 > p3 + 0.2, "resume 必须恢复前进：{p3} → {p4}");

    // seek → 落点 5.0s ±0.6
    dev.call("player", "player.seek", serde_json::json!({"session_id": &sid, "position_sec": 5.0}));
    wait_position(&dev, &sid, Duration::from_secs(3), |p| (p - 5.0).abs() < 0.6, "seek 落点");

    // set_volume 0.0–1.0 → 成功（mpv 内部 0–100）
    dev.call("player", "player.set_volume", serde_json::json!({"session_id": &sid, "volume": 0.4}));
    dev.call("player", "player.set_volume", serde_json::json!({"session_id": &sid, "volume": 1.0}));

    // status 列表（无 session_id）
    let list = dev.call("player", "player.status", serde_json::json!({}));
    assert!(list["sessions"].as_array().unwrap().len() >= 1);
    assert_eq!(list["playing"], true);

    // content_len 与源文件一致（rm 流交付对齐）
    assert_eq!(opened["content_len"].as_u64().unwrap(), file_len);
}

#[test]
fn e2_play_by_content_id_browse_lookup() {
    std::env::set_var("PLAYER_VO", "null");
    let rm_dir = install_dir("rm", &rm_root(), "resources-manager", "resources-manager");
    let pl_dir = install_dir("player", &player_root(), "player", "player");
    let tmp = tempfile::tempdir().unwrap();
    let dev = boot(&rm_dir, &pl_dir, tmp.path());
    let (pid, cid, _) = setup(&dev, tmp.path());

    let opened = dev.call("player", "player.play", serde_json::json!({"content_id": &cid}));
    let sid = opened["session_id"].as_str().unwrap().to_string();
    let st = wait_status(&dev, &sid, Duration::from_secs(6),
        |s| s["duration_sec"].as_f64().unwrap_or(0.0) > 9.0, "duration 识别");
    assert_eq!(st["placement_id"].as_i64().unwrap(), pid, "content_id 应解析到同一放置");
    assert_eq!(st["content_id"].as_str().unwrap(), cid.as_str());

    // 本机没有的 content_id → P101（F501 翻译）
    let err = dev.try_call("player", "player.play",
        serde_json::json!({"content_id": "0".repeat(64)})).unwrap_err();
    assert_eq!(err.code(), "P101", "未知内容应 P101：{err:?}");
}

#[test]
fn e3_finished_stop_and_error_codes() {
    std::env::set_var("PLAYER_VO", "null");
    let rm_dir = install_dir("rm", &rm_root(), "resources-manager", "resources-manager");
    let pl_dir = install_dir("player", &player_root(), "player", "player");
    let tmp = tempfile::tempdir().unwrap();
    let dev = boot(&rm_dir, &pl_dir, tmp.path());
    let (pid, _, _) = setup(&dev, tmp.path());

    let sid = dev.call("player", "player.play", serde_json::json!({"placement_id": pid}))
        ["session_id"].as_str().unwrap().to_string();
    wait_status(&dev, &sid, Duration::from_secs(6),
        |s| s["duration_sec"].as_f64().unwrap_or(0.0) > 9.0, "duration 识别");

    // seek 到尾部 → 等 Finished 收束（mpv eof → phase finished）
    dev.call("player", "player.seek", serde_json::json!({"session_id": &sid, "position_sec": 9.4}));
    wait_status(&dev, &sid, Duration::from_secs(8), |s| s["phase"] == "finished", "Finished 收束");

    // stop 摘除 → 之后 status P401
    dev.call("player", "player.stop", serde_json::json!({"session_id": &sid}));
    let err = dev.try_call("player", "player.status", serde_json::json!({"session_id": &sid})).unwrap_err();
    assert_eq!(err.code(), "P401", "stop 后会话应 P401：{err:?}");

    // 坏 placement → P101（rm F501 翻译）
    let err = dev.try_call("player", "player.play", serde_json::json!({"placement_id": 999_999})).unwrap_err();
    assert_eq!(err.code(), "P101", "不存在的放置应 P101：{err:?}");

    // 缺参 → P100（无 placement_id 也无 content_id）
    let err = dev.try_call("player", "player.play", serde_json::json!({})).unwrap_err();
    assert_eq!(err.code(), "P100");
    // 未知会话控制 → P401
    let err = dev.try_call("player", "player.pause", serde_json::json!({"session_id": "ps_nope"})).unwrap_err();
    assert_eq!(err.code(), "P401");
}

#[test]
fn e4_ui_page_served() {
    std::env::set_var("PLAYER_VO", "null");
    let rm_dir = install_dir("rm", &rm_root(), "resources-manager", "resources-manager");
    let pl_dir = install_dir("player", &player_root(), "player", "player");
    let tmp = tempfile::tempdir().unwrap();
    let dev = boot(&rm_dir, &pl_dir, tmp.path());

    let page = dev.call("player", "player.ui.page", serde_json::json!({"path": "/player/control"}));
    let html = page["html"].as_str().expect("html 直出");
    assert!(html.contains("<!DOCTYPE html>"));
    assert!(html.contains("播放器"));
    assert!(html.contains("/rpc"), "页面须走 core-ui 网关");
    assert!(html.contains("'player.' + method"), "页面须调本插件方法");

    // 未知页面 → P100
    let err = dev.try_call("player", "player.ui.page", serde_json::json!({"path": "/nope"})).unwrap_err();
    assert_eq!(err.code(), "P100");
}
