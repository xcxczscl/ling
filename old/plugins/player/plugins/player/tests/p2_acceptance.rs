//! P2 验收（`docs/01` §9）：本机输出设备枚举/切换 + UI 风格统一。
//!
//! ```sh
//! cargo test -p player --test p2_acceptance -- --test-threads=1
//! ```
//!
//! 覆盖：
//! - e1 outputs.list：恒有 display 行（DRM 连接器或兜底）+ audio 行（mpv ao 枚举）、
//!   字段形态、selected 初值 null
//! - e2 outputs.set：未知/缺参 P100；音频切换生效（选中回显 + active 标记 + 播放中
//!   会话应用后继续播放）；显示切换记录选择（headless 无窗口可迁，applied=0）
//! - e3 UI 统一：control/viewer 同源 shell（同样式块/同 rpc 网关/同 data-shell 标记），
//!   页面差异内容各就位；未知页 P100

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use ling_sdk::Value;

// ===== 与 p0/p1_acceptance 相同风格的最小内核引导 =====

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

/// 5s 测试视频 + 建库扫描 → placement_id。
fn setup_video(dev: &Device, root: &Path) -> i64 {
    let src_dir = root.join("src");
    std::fs::create_dir_all(&src_dir).unwrap();
    let mp4 = src_dir.join("clip.mp4");
    let st = std::process::Command::new("ffmpeg")
        .args([
            "-y", "-f", "lavfi", "-i", "testsrc=duration=5:size=320x240:rate=15",
            "-pix_fmt", "yuv420p", "-c:v", "mpeg4", "-q:v", "5",
            mp4.to_str().unwrap(),
        ])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .expect("ffmpeg 生成测试视频");
    assert!(st.success());

    dev.call(
        "resources-manager",
        "resources-manager.location.add",
        serde_json::json!({"path": src_dir.display().to_string(), "kind": "local", "label": "p2-src"}),
    );
    let loc = dev.call("resources-manager", "resources-manager.location.list", serde_json::json!({}))
        ["rows"][0]["id"].as_str().unwrap().to_string();
    let scan = dev.call("resources-manager", "resources-manager.scan.start", serde_json::json!({"location_id": loc}));
    let row = dev.wait_job(scan["job_id"].as_str().unwrap());
    assert_eq!(row["state"], "done", "扫描失败：{row}");

    let browse = dev.call("resources-manager", "resources-manager.browse.query", serde_json::json!({"page": {"limit": 100}}));
    browse["rows"].as_array().unwrap().iter()
        .find(|r| r["rel_path"] == "clip.mp4")
        .expect("browse 应含 clip.mp4")["placement_id"]
        .as_i64()
        .unwrap()
}

fn status_of(dev: &Device, sid: &str) -> Value {
    let wrap = dev.call("player", "player.status", serde_json::json!({"session_id": sid}));
    wrap["sessions"]
        .as_array()
        .and_then(|a| a.first().cloned())
        .unwrap_or_else(|| panic!("status 中无会话 {sid}：{wrap}"))
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
fn e1_outputs_list_rows_and_fields() {
    std::env::set_var("PLAYER_VO", "null");
    let rm_dir = install_dir("rm", &rm_root(), "resources-manager", "resources-manager");
    let pl_dir = install_dir("player", &player_root(), "player", "player");
    let tmp = tempfile::tempdir().unwrap();
    let dev = boot(&rm_dir, &pl_dir, tmp.path());

    let out = dev.call("player", "player.outputs.list", serde_json::json!({}));
    let rows = out["rows"].as_array().expect("rows 数组");
    assert!(out["selected"].is_null(), "初始无选中输出");

    // display 行恒在（DRM 连接器或 local-0 兜底）
    let displays: Vec<_> = rows.iter().filter(|r| r["kind"] == "display").collect();
    assert!(!displays.is_empty(), "恒有 display 行：{out}");
    for r in &displays {
        assert!(!r["id"].as_str().unwrap().is_empty());
        assert!(!r["name"].as_str().unwrap().is_empty());
        assert!(r["status"].is_string(), "display 应带 status：{r}");
        assert_eq!(r["active"], false, "未选中前无 active");
    }
    // audio 行（mpv ao 枚举；本环境 libmpv 可用 → 至少 auto）
    let audios: Vec<_> = rows.iter().filter(|r| r["kind"] == "audio").collect();
    assert!(!audios.is_empty(), "恒有 audio 行：{out}");
    assert!(audios.iter().any(|r| r["id"] == "auto"), "mpv 应枚举 auto：{out}");

    // 幂等
    let out2 = dev.call("player", "player.outputs.list", serde_json::json!({}));
    assert_eq!(out2["rows"].as_array().unwrap().len(), rows.len());
}

#[test]
fn e2_outputs_set_switch_and_apply() {
    std::env::set_var("PLAYER_VO", "null");
    let rm_dir = install_dir("rm", &rm_root(), "resources-manager", "resources-manager");
    let pl_dir = install_dir("player", &player_root(), "player", "player");
    let tmp = tempfile::tempdir().unwrap();
    let dev = boot(&rm_dir, &pl_dir, tmp.path());
    let pid = setup_video(&dev, tmp.path());

    // 缺参 / 未知设备 → P100
    let err = dev.try_call("player", "player.outputs.set", serde_json::json!({})).unwrap_err();
    assert_eq!(err.code(), "P100");
    let err = dev.try_call("player", "player.outputs.set",
        serde_json::json!({"output_id": "no-such-device"})).unwrap_err();
    assert_eq!(err.code(), "P100", "未知输出设备应 P100：{err:?}");

    let out = dev.call("player", "player.outputs.list", serde_json::json!({}));
    let rows = out["rows"].as_array().unwrap();
    let audio_id = rows.iter()
        .find(|r| r["kind"] == "audio" && r["id"] == "auto")
        .map(|r| r["id"].as_str().unwrap().to_string())
        .expect("audio auto 行");
    let display_id = rows.iter()
        .find(|r| r["kind"] == "display")
        .map(|r| r["id"].as_str().unwrap().to_string())
        .unwrap();

    // 播一会话（播放中切换音频 → 应用到该会话）
    let sid = dev.call("player", "player.play", serde_json::json!({"placement_id": pid}))
        ["session_id"].as_str().unwrap().to_string();
    wait_position(&dev, &sid, Duration::from_secs(6), |p| p > 0.2, "position 前进");

    // 音频切换：应用到在播会话
    let r = dev.call("player", "player.outputs.set", serde_json::json!({"output_id": &audio_id}));
    assert_eq!(r["kind"], "audio");
    assert_eq!(r["applied"], 1, "在播 mpv 会话应被应用：{r}");
    let list = dev.call("player", "player.outputs.list", serde_json::json!({}));
    assert_eq!(list["selected"], audio_id.as_str());
    let active = list["rows"].as_array().unwrap().iter()
        .find(|x| x["id"] == audio_id).unwrap()["active"]
        .as_bool().unwrap();
    assert!(active, "选中行 active=true：{list}");
    // 切换后会话仍健康播放
    wait_position(&dev, &sid, Duration::from_secs(4), |p| p > 0.7, "切换后继续播放");

    // 显示切换：记录选择，headless 无窗口可迁 → applied=0
    let r = dev.call("player", "player.outputs.set", serde_json::json!({"output_id": &display_id}));
    assert_eq!(r["kind"], "display");
    assert_eq!(r["applied"], 0);
    let list = dev.call("player", "player.outputs.list", serde_json::json!({}));
    assert_eq!(list["selected"], display_id.as_str());
    assert_eq!(status_of(&dev, &sid)["phase"], "playing", "显示切换不得打断播放");

    dev.call("player", "player.stop", serde_json::json!({"session_id": &sid}));
}

#[test]
fn e3_ui_shell_unified_across_pages() {
    std::env::set_var("PLAYER_VO", "null");
    let rm_dir = install_dir("rm", &rm_root(), "resources-manager", "resources-manager");
    let pl_dir = install_dir("player", &player_root(), "player", "player");
    let tmp = tempfile::tempdir().unwrap();
    let dev = boot(&rm_dir, &pl_dir, tmp.path());

    let control = dev.call("player", "player.ui.page", serde_json::json!({"path": "/player/control"}))
        ["html"].as_str().unwrap().to_string();
    let viewer = dev.call("player", "player.ui.page", serde_json::json!({"path": "/player/viewer"}))
        ["html"].as_str().unwrap().to_string();

    // 同源 shell：共享标记 / 样式块逐字一致 / rpc 网关一致
    for h in [&control, &viewer] {
        assert!(h.starts_with("<!DOCTYPE html>"));
        assert!(h.contains(r#"data-shell="p2""#), "共享 shell 标记");
        assert!(h.contains("player.' + method"), "共享 rpc 网关");
    }
    let style_of = |h: &str| -> String {
        let s = h.find("<style>").unwrap() + 7;
        let e = h[s..].find("</style>").unwrap() + s;
        h[s..e].to_string()
    };
    assert_eq!(style_of(&control), style_of(&viewer), "两页样式块必须同源逐字一致");
    assert!(control.contains("输出设备（P2）"), "控制页有输出设备卡");
    assert!(control.contains("outputs.list") && control.contains("outputs.set"));
    assert!(viewer.contains("上一页") && viewer.contains("goto_page"), "查看器翻页能力");
    assert!(control.contains("字幕轨"), "控制页字幕下拉（P1 遗留）");

    // 未知页 P100
    let err = dev.try_call("player", "player.ui.page", serde_json::json!({"path": "/nope"})).unwrap_err();
    assert_eq!(err.code(), "P100");
}
