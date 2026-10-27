//! P1 验收（`docs/01` §9）：图片/PDF 查看 + 字幕轨 + 不支持类型拒绝。
//!
//! ```sh
//! cargo test -p player --test p1_acceptance -- --test-threads=1
//! ```
//!
//! 依赖运行环境：ffmpeg（夹具生成）、poppler-utils（pdftoppm/pdfinfo）、
//! ffprobe（轨探测，缺失时降级空表——本用例假定可用，与 README 环境说明一致）。
//!
//! 覆盖：
//! - e1 image：play → status 尺寸/kind=viewer → frame PNG 解码回原尺寸红像素 →
//!   pause/seek → P103 → stop → P401
//! - e2 pdf：play → pages=2 → frame → goto_page 2 帧变化 → 越界 P100 →
//!   set_subtitle/pause → P103；frame 数据往返
//! - e3 video：subs.mkv 双字幕轨 → subtitle_tracks 枚举 → set_subtitle 选/关/
//!   无效轨 P100 → frame/goto_page → P103
//! - e4 不支持类型 → P103（doc 非 PDF / other / app）
//! - e5 查看器页与控制页字幕下拉直出

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use ling_sdk::Value;
use player::pdf_cap::fixture_pdf;

// ===== 与 p0_acceptance 相同风格的最小内核引导 =====

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

struct Fixtures {
    /// rel_path → (placement_id, content_id)
    rows: HashMap<String, (i64, String)>,
}

impl Fixtures {
    fn get(&self, rel: &str) -> (i64, String) {
        self.rows
            .get(rel)
            .unwrap_or_else(|| panic!("夹具未入 browse：{rel}；现有 {:?}", self.rows.keys()))
            .clone()
    }
}

fn ffmpeg(args: &[&str]) {
    let st = std::process::Command::new("ffmpeg")
        .args(args)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .expect("ffmpeg 运行");
    assert!(st.success(), "ffmpeg 失败：{args:?}");
}

/// 生成 P1 夹具（src/ 目录）→ 建库扫描 → browse 索引。
fn setup(dev: &Device, root: &Path) -> Fixtures {
    let src_dir = root.join("src");
    std::fs::create_dir_all(&src_dir).unwrap();

    // 图片：纯红 320×240 PNG
    let png = src_dir.join("fixture.png");
    ffmpeg(&[
        "-y", "-f", "lavfi", "-i", "color=c=red:s=320x240:d=1",
        "-frames:v", "1", png.to_str().unwrap(),
    ]);

    // PDF：两页（页底色红/绿 → 渲染必不同）
    std::fs::write(src_dir.join("manual.pdf"), fixture_pdf(&["PDF-ONE", "PDF-TWO"])).unwrap();

    // 双字幕轨 mkv（srt 放 src 外，避免入扫描）
    let a = root.join("a.srt");
    let b = root.join("b.srt");
    std::fs::write(&a, "1\n00:00:00,000 --> 00:00:05,000\nSUB-ONE\n\n").unwrap();
    std::fs::write(&b, "1\n00:00:00,000 --> 00:00:05,000\nSUB-TWO\n\n").unwrap();
    let mkv = src_dir.join("subs.mkv");
    ffmpeg(&[
        "-y", "-f", "lavfi", "-i", "color=c=blue:s=160x120:d=5",
        "-i", a.to_str().unwrap(), "-i", b.to_str().unwrap(),
        "-map", "0", "-map", "1", "-map", "2",
        "-c:v", "mpeg4", "-c:s", "srt",
        "-metadata:s:s:0", "language=eng", "-metadata:s:s:0", "title=English",
        "-metadata:s:s:1", "language=jpn", "-metadata:s:s:1", "title=日本語",
        mkv.to_str().unwrap(),
    ]);

    // 不支持类型三件套
    std::fs::write(src_dir.join("note.txt"), "just some text\n").unwrap();
    std::fs::write(src_dir.join("weird.xyz"), b"\x00\x01\x02unknown").unwrap();
    std::fs::write(src_dir.join("setup.exe"), b"MZ\x90\x00 fake pe").unwrap();

    dev.call(
        "resources-manager",
        "resources-manager.location.add",
        serde_json::json!({"path": src_dir.display().to_string(), "kind": "local", "label": "p1-src"}),
    );
    let loc = dev.call("resources-manager", "resources-manager.location.list", serde_json::json!({}))
        ["rows"][0]["id"].as_str().unwrap().to_string();
    let scan = dev.call("resources-manager", "resources-manager.scan.start", serde_json::json!({"location_id": loc}));
    let row = dev.wait_job(scan["job_id"].as_str().unwrap());
    assert_eq!(row["state"], "done", "扫描失败：{row}");

    let browse = dev.call("resources-manager", "resources-manager.browse.query", serde_json::json!({"page": {"limit": 500}}));
    let mut rows = HashMap::new();
    for r in browse["rows"].as_array().unwrap() {
        let rel = r["rel_path"].as_str().unwrap().to_string();
        rows.insert(
            rel,
            (r["placement_id"].as_i64().unwrap(), r["content_id"].as_str().unwrap().to_string()),
        );
    }
    Fixtures { rows }
}

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

/// 取 frame 并解码回 RGBA（校验 PNG 往返）。
fn frame_rgba(dev: &Device, sid: &str) -> (u32, u32, Vec<u8>) {
    let f = dev.call("player", "player.frame", serde_json::json!({"session_id": sid}));
    assert_eq!(f["mime"], "image/png");
    use base64::Engine;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(f["data"].as_str().unwrap())
        .expect("base64 解码");
    let img = image::load_from_memory(&bytes).expect("PNG 解码");
    assert_eq!(img.width(), f["width"].as_u64().unwrap() as u32);
    assert_eq!(img.height(), f["height"].as_u64().unwrap() as u32);
    (img.width(), img.height(), img.to_rgba8().into_raw())
}

// ===== 用例 =====

#[test]
fn e1_image_session_frame_and_guards() {
    std::env::set_var("PLAYER_VO", "null");
    let rm_dir = install_dir("rm", &rm_root(), "resources-manager", "resources-manager");
    let pl_dir = install_dir("player", &player_root(), "player", "player");
    let tmp = tempfile::tempdir().unwrap();
    let dev = boot(&rm_dir, &pl_dir, tmp.path());
    let fx = setup(&dev, tmp.path());
    let (pid, cid) = fx.get("fixture.png");

    // placement_id 路径
    let opened = dev.call("player", "player.play", serde_json::json!({"placement_id": pid}));
    let sid = opened["session_id"].as_str().unwrap().to_string();
    assert_eq!(opened["asset_type"], "image");
    assert_eq!(opened["width"], 320);
    assert_eq!(opened["height"], 240);
    assert!(opened["content_len"].as_u64().unwrap() > 0);

    let st = status_of(&dev, &sid);
    assert_eq!(st["asset_type"], "image");
    assert_eq!(st["kind"], "viewer");
    assert_eq!(st["phase"], "playing");
    assert_eq!(st["width"], 320);
    assert_eq!(st["height"], 240);
    assert_eq!(st["page"], Value::Null, "图片无页码字段");

    // frame → PNG 往返 → 红像素
    let (w, h, rgba) = frame_rgba(&dev, &sid);
    assert_eq!((w, h), (320, 240));
    let (r, g, b) = (rgba[0], rgba[1], rgba[2]);
    assert!(r > 200 && g < 80 && b < 80, "帧应为红色：({r},{g},{b})");

    // 音视频控制 → P103
    for (m, extra) in [
        ("player.pause", serde_json::json!({})),
        ("player.resume", serde_json::json!({})),
        ("player.seek", serde_json::json!({"position_sec": 1.0})),
        ("player.set_volume", serde_json::json!({"volume": 0.5})),
        ("player.set_subtitle", serde_json::json!({"track": 1})),
    ] {
        let mut args = extra;
        args["session_id"] = serde_json::json!(&sid);
        let err = dev.try_call("player", m, args).unwrap_err();
        assert_eq!(err.code(), "P103", "{m} 应 P103：{err:?}");
    }
    // goto_page 在图片 → P103
    let err = dev.try_call("player", "player.goto_page",
        serde_json::json!({"session_id": &sid, "page": 1})).unwrap_err();
    assert_eq!(err.code(), "P103");

    // content_id 路径 → 同一内容
    let opened2 = dev.call("player", "player.play", serde_json::json!({"content_id": &cid}));
    assert_eq!(opened2["width"], 320);
    let sid2 = opened2["session_id"].as_str().unwrap().to_string();
    assert_eq!(status_of(&dev, &sid2)["placement_id"].as_i64().unwrap(), pid);

    // stop 摘除 → P401（frame 也无从取）
    dev.call("player", "player.stop", serde_json::json!({"session_id": &sid}));
    let err = dev.try_call("player", "player.frame", serde_json::json!({"session_id": &sid})).unwrap_err();
    assert_eq!(err.code(), "P401");
    dev.call("player", "player.stop", serde_json::json!({"session_id": &sid2}));
}

#[test]
fn e2_pdf_session_pages_and_bounds() {
    std::env::set_var("PLAYER_VO", "null");
    let rm_dir = install_dir("rm", &rm_root(), "resources-manager", "resources-manager");
    let pl_dir = install_dir("player", &player_root(), "player", "player");
    let tmp = tempfile::tempdir().unwrap();
    let dev = boot(&rm_dir, &pl_dir, tmp.path());
    let fx = setup(&dev, tmp.path());
    let (pid, _) = fx.get("manual.pdf");

    let opened = dev.call("player", "player.play", serde_json::json!({"placement_id": pid}));
    let sid = opened["session_id"].as_str().unwrap().to_string();
    assert_eq!(opened["asset_type"], "doc");
    assert_eq!(opened["pages"], 2);
    assert_eq!(opened["page"], 1);

    let st = status_of(&dev, &sid);
    assert_eq!(st["kind"], "viewer");
    assert_eq!(st["asset_type"], "doc");
    assert_eq!(st["page"], 1);
    assert_eq!(st["pages"], 2);
    let (w1, h1, _rgba1) = frame_rgba(&dev, &sid);
    assert!(w1 > 600 && h1 > 700, "≈96dpi letter：{w1}×{h1}");

    // 翻页 → page=2 且帧变化（页底色不同）
    let f1 = dev.call("player", "player.frame", serde_json::json!({"session_id": &sid}));
    let nav = dev.call("player", "player.goto_page",
        serde_json::json!({"session_id": &sid, "page": 2}));
    assert_eq!(nav["page"], 2);
    assert_eq!(nav["pages"], 2);
    let st2 = status_of(&dev, &sid);
    assert_eq!(st2["page"], 2);
    let f2 = dev.call("player", "player.frame", serde_json::json!({"session_id": &sid}));
    assert_eq!(f1["width"], f2["width"]);
    assert_ne!(f1["data"], f2["data"], "两页帧必须不同");
    let (w2, h2, _) = frame_rgba(&dev, &sid);
    assert_eq!((w2, h2), (w1, h1));

    // 回第 1 页复原
    dev.call("player", "player.goto_page", serde_json::json!({"session_id": &sid, "page": 1}));
    let f1b = dev.call("player", "player.frame", serde_json::json!({"session_id": &sid}));
    assert_eq!(f1["data"], f1b["data"]);

    // 越界 / 零页 → P100
    for p in [0, 3, -1] {
        let err = dev.try_call("player", "player.goto_page",
            serde_json::json!({"session_id": &sid, "page": p})).unwrap_err();
        assert_eq!(err.code(), "P100", "page={p} 应 P100：{err:?}");
    }
    // 缺参 → P100
    let err = dev.try_call("player", "player.goto_page",
        serde_json::json!({"session_id": &sid})).unwrap_err();
    assert_eq!(err.code(), "P100");

    // 音视频控制 → P103
    let err = dev.try_call("player", "player.pause", serde_json::json!({"session_id": &sid})).unwrap_err();
    assert_eq!(err.code(), "P103");
    let err = dev.try_call("player", "player.set_subtitle",
        serde_json::json!({"session_id": &sid, "track": 1})).unwrap_err();
    assert_eq!(err.code(), "P103");
    let err = dev.try_call("player", "player.seek",
        serde_json::json!({"session_id": &sid, "position_sec": 1.0})).unwrap_err();
    assert_eq!(err.code(), "P103");
}

#[test]
fn e3_video_subtitle_tracks_and_frame_guards() {
    std::env::set_var("PLAYER_VO", "null");
    let rm_dir = install_dir("rm", &rm_root(), "resources-manager", "resources-manager");
    let pl_dir = install_dir("player", &player_root(), "player", "player");
    let tmp = tempfile::tempdir().unwrap();
    let dev = boot(&rm_dir, &pl_dir, tmp.path());
    let fx = setup(&dev, tmp.path());
    let (pid, _) = fx.get("subs.mkv");

    let opened = dev.call("player", "player.play", serde_json::json!({"placement_id": pid}));
    let sid = opened["session_id"].as_str().unwrap().to_string();
    assert_eq!(opened["asset_type"], "video");

    // duration 识别（解码头读出）
    let st = wait_status(&dev, &sid, Duration::from_secs(6),
        |s| s["duration_sec"].as_f64().unwrap_or(0.0) > 4.0, "duration 识别");
    assert_eq!(st["kind"], "player");

    // P1 §5.10：ffprobe 枚举 → 双字幕轨（mpv sid 按类型 1-based）
    let subs = st["subtitle_tracks"].as_array().expect("subtitle_tracks 数组");
    assert_eq!(subs.len(), 2, "夹具应有 2 条字幕轨：{st}");
    assert_eq!(subs[0]["id"], 1);
    assert_eq!(subs[0]["lang"], "eng");
    assert_eq!(subs[0]["title"], "English");
    assert_eq!(subs[1]["id"], 2);
    assert_eq!(subs[1]["lang"], "jpn");
    assert!(st["audio_tracks"].is_array(), "audio_tracks 恒为数组");
    assert_eq!(st["audio_tracks"].as_array().unwrap().len(), 0, "夹具无音轨");

    // 选轨 → 回读生效
    let r = dev.call("player", "player.set_subtitle",
        serde_json::json!({"session_id": &sid, "track": 2}));
    assert_eq!(r["subtitle_track"], 2);
    assert_eq!(status_of(&dev, &sid)["subtitle_track"], 2);

    // 关闭 → -1
    let r = dev.call("player", "player.set_subtitle",
        serde_json::json!({"session_id": &sid, "track": -1}));
    assert_eq!(r["subtitle_track"], -1);
    assert_eq!(status_of(&dev, &sid)["subtitle_track"], -1);

    // 再选轨 1
    dev.call("player", "player.set_subtitle",
        serde_json::json!({"session_id": &sid, "track": 1}));
    assert_eq!(status_of(&dev, &sid)["subtitle_track"], 1);

    // 无效轨 → P100；缺参 → P100
    let err = dev.try_call("player", "player.set_subtitle",
        serde_json::json!({"session_id": &sid, "track": 9})).unwrap_err();
    assert_eq!(err.code(), "P100", "无效 sid 应 P100：{err:?}");
    let err = dev.try_call("player", "player.set_subtitle",
        serde_json::json!({"session_id": &sid})).unwrap_err();
    assert_eq!(err.code(), "P100");

    // frame/goto_page 仅查看器 → P103
    let err = dev.try_call("player", "player.frame", serde_json::json!({"session_id": &sid})).unwrap_err();
    assert_eq!(err.code(), "P103");
    let err = dev.try_call("player", "player.goto_page",
        serde_json::json!({"session_id": &sid, "page": 1})).unwrap_err();
    assert_eq!(err.code(), "P103");

    // 音量与暂停仍可用（mpv 会话）
    dev.call("player", "player.set_volume", serde_json::json!({"session_id": &sid, "volume": 0.8}));
    dev.call("player", "player.pause", serde_json::json!({"session_id": &sid}));
    wait_status(&dev, &sid, Duration::from_secs(2), |s| s["phase"] == "paused", "pause");
    dev.call("player", "player.stop", serde_json::json!({"session_id": &sid}));
}

#[test]
fn e4_unsupported_asset_types_p103() {
    std::env::set_var("PLAYER_VO", "null");
    let rm_dir = install_dir("rm", &rm_root(), "resources-manager", "resources-manager");
    let pl_dir = install_dir("player", &player_root(), "player", "player");
    let tmp = tempfile::tempdir().unwrap();
    let dev = boot(&rm_dir, &pl_dir, tmp.path());
    let fx = setup(&dev, tmp.path());

    // doc 非 PDF（首版仅 PDF）
    let (pid, _) = fx.get("note.txt");
    let err = dev.try_call("player", "player.play", serde_json::json!({"placement_id": pid})).unwrap_err();
    assert_eq!(err.code(), "P103", "txt 文档应 P103：{err:?}");

    // other
    let (pid, _) = fx.get("weird.xyz");
    let err = dev.try_call("player", "player.play", serde_json::json!({"placement_id": pid})).unwrap_err();
    assert_eq!(err.code(), "P103", "other 应 P103：{err:?}");

    // app → P5 前拒绝
    let (pid, _) = fx.get("setup.exe");
    let err = dev.try_call("player", "player.play", serde_json::json!({"placement_id": pid})).unwrap_err();
    assert_eq!(err.code(), "P103", "app 应 P103：{err:?}");

    // 拒绝不产生会话
    let list = dev.call("player", "player.status", serde_json::json!({}));
    assert_eq!(list["sessions"].as_array().unwrap().len(), 0, "P103 拒绝不得建会话");
}

#[test]
fn e5_ui_pages_viewer_and_control() {
    std::env::set_var("PLAYER_VO", "null");
    let rm_dir = install_dir("rm", &rm_root(), "resources-manager", "resources-manager");
    let pl_dir = install_dir("player", &player_root(), "player", "player");
    let tmp = tempfile::tempdir().unwrap();
    let dev = boot(&rm_dir, &pl_dir, tmp.path());

    // 查看器页（P1 新增）
    let page = dev.call("player", "player.ui.page", serde_json::json!({"path": "/player/viewer"}));
    let html = page["html"].as_str().expect("viewer html 直出");
    assert!(html.contains("<!DOCTYPE html>"));
    assert!(html.contains("查看器"));
    assert!(html.contains("/rpc"));
    assert!(html.contains("goto_page"), "查看器须调翻页方法");
    assert!(html.contains("'player.' + method"));

    // 控制页：字幕下拉
    let page = dev.call("player", "player.ui.page", serde_json::json!({"path": "/player/control"}));
    let html = page["html"].as_str().unwrap();
    assert!(html.contains("字幕轨"), "控制页须有字幕轨选择");
    assert!(html.contains("set_subtitle"), "控制页须调字幕方法");
}
