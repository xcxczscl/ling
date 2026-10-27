//! 流交付验收（`docs/03` §2.6 契约落地；player P0 的数据面前置）。
//!
//! `cargo test -p rm-plugin --test stream_acceptance -- --test-threads=1`
//!
//! 覆盖：
//! - stream.open 契约形态（stream_id/url/content_len/seekable/idle_ttl）
//! - GET 整流 / Range 206 / HEAD / 403 坏 token
//! - 放置离线 → 断流；get_local_path 正常与 F501/F503
//! - zstd 内容：Range 解压透明

use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

use ling_sdk::Value;

// ===== 与 m3/m5_acceptance 相同的最小内核引导 =====

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

fn build_plugin_dir() -> PathBuf {
    let root = workspace_root();
    let status = std::process::Command::new(env!("CARGO"))
        .args(["build", "-p", "resources-manager"])
        .current_dir(&root)
        .status()
        .expect("build resources-manager");
    assert!(status.success());
    let lib = format!(
        "{}resources_manager{}",
        std::env::consts::DLL_PREFIX,
        std::env::consts::DLL_SUFFIX
    );
    let src = root.join("target/debug").join(&lib);
    assert!(src.is_file(), "cdylib 未找到：{}", src.display());
    let dir = root.join(format!(
        "target/debug/rmgr-plugin-install-stream-{}",
        std::process::id()
    ));
    if !dir.join("plugin.toml").is_file() {
        let _ = fs_remove(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::copy(
            root.join("plugins/resources-manager/plugin.toml"),
            dir.join("plugin.toml"),
        )
        .unwrap();
        std::fs::copy(
            &src,
            dir.join(format!("resources-manager{}", std::env::consts::DLL_SUFFIX)),
        )
        .unwrap();
    }
    dir
}

fn fs_remove(p: &Path) -> std::io::Result<()> {
    std::fs::remove_dir_all(p)
}

struct Device {
    kernel: ling_kernel::Kernel,
    #[allow(dead_code)]
    data_dir: PathBuf,
}

fn boot(root: &Path, plugin_dir: &Path) -> Device {
    let kernel = ling_kernel::Kernel::new(root.join("ling")).unwrap();
    if !kernel
        .installed_plugins()
        .iter()
        .any(|p| p == "resources-manager")
    {
        kernel.install(plugin_dir).unwrap();
    }
    if !kernel.activated_plugins().iter().any(|p| p == "resources-manager") {
        kernel.load("resources-manager").unwrap();
    }
    let data_dir = root
        .join("ling")
        .join("plugins")
        .join("resources-manager")
        .join("data");
    Device { kernel, data_dir }
}

impl Device {
    fn call(&self, method: &str, args: Value) -> Value {
        self.kernel
            .call("resources-manager", &format!("resources-manager.{method}"), args)
            .unwrap_or_else(|e| panic!("{method} 失败：{e:?}"))
    }

    fn try_call(&self, method: &str, args: Value) -> Result<Value, ling_sdk::CallError> {
        self.kernel
            .call("resources-manager", &format!("resources-manager.{method}"), args)
    }

    fn wait_job(&self, job_id: &str) -> Value {
        for _ in 0..300 {
            let j = self.call("job.list", serde_json::json!({}));
            if let Some(rows) = j["rows"].as_array() {
                if let Some(row) = rows.iter().find(|r| r["id"] == job_id) {
                    if row["state"] == "done" || row["state"] == "failed" {
                        let log = self
                            .call("job.log", serde_json::json!({"job_id": job_id}))
                            .to_string();
                        let mut out = row.clone();
                        out["log"] = Value::String(log);
                        return out;
                    }
                }
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        panic!("作业未完成：{job_id}");
    }
}

/// 原始 HTTP 小客户端：返回 (status, headers小写键值, body)——需要看响应头（Content-Range 等）。
fn raw_http(addr: &str, request_head: &str) -> (u16, Vec<(String, String)>, Vec<u8>) {
    let mut stream = TcpStream::connect(addr).expect("connect stream server");
    stream.write_all(request_head.as_bytes()).unwrap();
    // 读到头部结束。
    let mut buf = Vec::new();
    let mut tmp = [0u8; 4096];
    let head_end = loop {
        let n = stream.read(&mut tmp).expect("read head");
        assert!(n > 0, "连接在头结束前关闭");
        buf.extend_from_slice(&tmp[..n]);
        if let Some(pos) = find_sub(&buf, b"\r\n\r\n") {
            break pos;
        }
    };
    let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
    let mut rest = buf[head_end + 4..].to_vec();

    let mut lines = head.split("\r\n");
    let status: u16 = lines
        .next()
        .unwrap()
        .split_whitespace()
        .nth(1)
        .unwrap()
        .parse()
        .unwrap();
    let mut headers = Vec::new();
    let mut content_length: Option<usize> = None;
    for l in lines {
        if let Some((k, v)) = l.split_once(':') {
            let k = k.trim().to_ascii_lowercase();
            let v = v.trim().to_string();
            if k == "content-length" {
                content_length = v.parse().ok();
            }
            headers.push((k, v));
        }
    }
    // 读满 body（Connection: close）。
    if let Some(cl) = content_length {
        while rest.len() < cl {
            let n = stream.read(&mut tmp).expect("read body");
            if n == 0 {
                break;
            }
            rest.extend_from_slice(&tmp[..n]);
        }
        rest.truncate(cl);
    } else {
        loop {
            let n = stream.read(&mut tmp).unwrap_or(0);
            if n == 0 {
                break;
            }
            rest.extend_from_slice(&tmp[..n]);
        }
    }
    (status, headers, rest)
}

fn find_sub(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

fn header<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str())
}

/// 建库 + 登记 + 扫描，返回 (Device, placement_id, 原始字节)。
fn setup_scanned(plugin: &Path, root: &Path) -> (Device, i64, Vec<u8>) {
    let dev = boot(root, plugin);

    // 源文件：32 KiB 可预测字节。
    let src_dir = root.join("src");
    std::fs::create_dir_all(&src_dir).unwrap();
    let payload: Vec<u8> = (0..32 * 1024).map(|i| (i % 251) as u8).collect();
    std::fs::write(src_dir.join("movie.bin"), &payload).unwrap();

    dev.call(
        "location.add",
        serde_json::json!({"path": src_dir.display().to_string(), "kind": "local", "label": "stream-src"}),
    );
    let loc = dev.call("location.list", serde_json::json!({}))["rows"][0]["id"]
        .as_str()
        .unwrap()
        .to_string();
    let scan = dev.call("scan.start", serde_json::json!({"location_id": loc}));
    let job = scan["job_id"].as_str().unwrap().to_string();
    let row = dev.wait_job(&job);
    assert_eq!(row["state"], "done", "扫描失败：{row}");

    let browse = dev.call("browse.query", serde_json::json!({"page": {"limit": 50}}));
    let pid = browse["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["rel_path"] == "movie.bin")
        .expect("browse 应含 movie.bin")["placement_id"]
        .as_i64()
        .unwrap();
    (dev, pid, payload)
}

fn url_parts(url: &str) -> (String, String) {
    // http://127.0.0.1:PORT/s/st_x/TOKEN
    let rest = url.strip_prefix("http://").expect("http url");
    let (host, path) = rest.split_once('/').expect("url host+path");
    (host.to_string(), format!("/{path}"))
}

// ===== 用例 =====

#[test]
fn s1_stream_open_get_range_head_forbidden() {
    let plugin = build_plugin_dir();
    let tmp = tempfile::tempdir().unwrap();
    let (dev, pid, payload) = setup_scanned(&plugin, &tmp.path());

    // stream.open 契约形态。
    let opened = dev.call("stream.open", serde_json::json!({"placement_id": pid, "purpose": "play"}));
    let url = opened["url"].as_str().unwrap().to_string();
    assert!(url.starts_with("http://127.0.0.1:"), "url 必须回环：{url}");
    assert!(url.contains(&format!("/s/{}/", opened["stream_id"].as_str().unwrap())));
    assert_eq!(opened["content_len"].as_u64().unwrap(), payload.len() as u64);
    assert_eq!(opened["seekable"], true);
    assert_eq!(opened["idle_ttl_sec"].as_u64().unwrap(), 600);
    let (host, path) = url_parts(&url);

    // GET 整流。
    let (st, hdrs, body) = raw_http(
        &host,
        &format!("GET {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n"),
    );
    assert_eq!(st, 200);
    assert_eq!(body, payload, "整流字节必须一致");
    assert_eq!(header(&hdrs, "content-length").unwrap(), payload.len().to_string());
    assert_eq!(header(&hdrs, "accept-ranges"), Some("bytes"));

    // Range 206（seek 语义）。
    let (st, hdrs, body) = raw_http(
        &host,
        &format!(
            "GET {path} HTTP/1.1\r\nHost: {host}\r\nRange: bytes=100-199\r\nConnection: close\r\n\r\n"
        ),
    );
    assert_eq!(st, 206);
    assert_eq!(body, payload[100..200].to_vec());
    assert_eq!(
        header(&hdrs, "content-range"),
        Some(format!("bytes 100-199/{}", payload.len()).as_str())
    );

    // 开区间 Range bytes=a-
    let (st, _hdrs, body) = raw_http(
        &host,
        &format!(
            "GET {path} HTTP/1.1\r\nHost: {host}\r\nRange: bytes={}-\r\nConnection: close\r\n\r\n",
            payload.len() - 50
        ),
    );
    assert_eq!(st, 206);
    assert_eq!(body, payload[payload.len() - 50..].to_vec());

    // HEAD：200 + Content-Length，无 body。
    let (st, hdrs, body) = raw_http(
        &host,
        &format!("HEAD {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n"),
    );
    assert_eq!(st, 200);
    assert!(body.is_empty(), "HEAD 不带 body");
    assert_eq!(header(&hdrs, "content-length").unwrap(), payload.len().to_string());

    // 坏 token → 403 固定文案。
    let bad_path = path.rsplit('/').next().map(|tok| {
        format!("/s/st_deadbeef/{tok}00")
    });
    let (st, _h, body) = raw_http(
        &host,
        &format!(
            "GET {} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n",
            bad_path.unwrap()
        ),
    );
    assert_eq!(st, 403);
    assert_eq!(String::from_utf8_lossy(&body), "F502 invalid or expired token");

    // 越界 Range → 416。
    let (st, _h, _b) = raw_http(
        &host,
        &format!(
            "GET {path} HTTP/1.1\r\nHost: {host}\r\nRange: bytes={}-{}\r\nConnection: close\r\n\r\n",
            payload.len() + 10,
            payload.len() + 20
        ),
    );
    assert_eq!(st, 416);
}

#[test]
fn s2_offline_breaks_stream_and_get_local_path_contract() {
    let plugin = build_plugin_dir();
    let tmp = tempfile::tempdir().unwrap();
    let (dev, pid, payload) = setup_scanned(&plugin, &tmp.path());

    let opened = dev.call("stream.open", serde_json::json!({"placement_id": pid}));
    let url = opened["url"].as_str().unwrap().to_string();
    let (host, path) = url_parts(&url);

    // get_local_path：plain 在线 → 路径可读且字节一致。
    let lp = dev.call("get_local_path", serde_json::json!({"placement_id": pid}));
    let p = PathBuf::from(lp["path"].as_str().unwrap());
    assert_eq!(std::fs::read(&p).unwrap(), payload);

    // 位置离线 → 断流（每次请求实时查）。
    let loc = dev.call("location.list", serde_json::json!({}))["rows"][0]["id"]
        .as_str()
        .unwrap()
        .to_string();
    dev.call("location.set_state", serde_json::json!({"location_id": loc, "state": "offline"}));

    let (st, _h, body) = raw_http(
        &host,
        &format!("GET {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n"),
    );
    assert_eq!(st, 403, "离线必须断流：{}", String::from_utf8_lossy(&body));

    // get_local_path → F501。
    let err = dev
        .try_call("get_local_path", serde_json::json!({"placement_id": pid}))
        .expect_err("离线必须 F501");
    assert_eq!(err.code(), "F501");

    // 恢复在线。
    dev.call("location.set_state", serde_json::json!({"location_id": loc, "state": "online"}));

    // 不存在的 placement → F501（开流与路径均拒绝）。
    let err = dev
        .try_call("stream.open", serde_json::json!({"placement_id": 999_999}))
        .expect_err("不存在应 F501");
    assert_eq!(err.code(), "F501");
    let err = dev
        .try_call("get_local_path", serde_json::json!({"placement_id": "999999"}))
        .expect_err("字符串 id 也不应存在");
    assert_eq!(err.code(), "F501");
}

#[test]
fn s3_zstd_content_served_transparently() {
    let plugin = build_plugin_dir();
    let tmp = tempfile::tempdir().unwrap();
    let (dev, pid, payload) = setup_scanned(&plugin, &tmp.path());

    // 压缩该放置（enc → zstd，盘上变压缩字节）。
    let job = dev.call(
        "compress.run",
        serde_json::json!({"placement_ids": [pid], "mode": "always"}),
    );
    let row = dev.wait_job(job["job_id"].as_str().unwrap());
    assert_eq!(row["state"], "done", "压缩失败：{row}");

    // get_local_path → F503（压缩形态不支持路径交付）。
    let err = dev
        .try_call("get_local_path", serde_json::json!({"placement_id": pid}))
        .expect_err("压缩必须 F503");
    assert_eq!(err.code(), "F503");

    // stream.open 仍然可用（契约：压缩对消费者透明）。
    let opened = dev.call("stream.open", serde_json::json!({"placement_id": pid}));
    assert_eq!(
        opened["content_len"].as_u64().unwrap(),
        payload.len() as u64,
        "content_len 必须是逻辑（解压后）长度"
    );
    let url = opened["url"].as_str().unwrap().to_string();
    let (host, path) = url_parts(&url);

    // 整流 = 解压后原字节。
    let (st, _h, body) = raw_http(
        &host,
        &format!("GET {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n"),
    );
    assert_eq!(st, 200);
    assert_eq!(body, payload, "zstd 整流必须等于原始逻辑字节");

    // Range 也按逻辑字节切片。
    let (st, hdrs, body) = raw_http(
        &host,
        &format!(
            "GET {path} HTTP/1.1\r\nHost: {host}\r\nRange: bytes=1000-1999\r\nConnection: close\r\n\r\n"
        ),
    );
    assert_eq!(st, 206);
    assert_eq!(body, payload[1000..2000].to_vec());
    assert_eq!(
        header(&hdrs, "content-range"),
        Some(format!("bytes 1000-1999/{}", payload.len()).as_str())
    );
}
