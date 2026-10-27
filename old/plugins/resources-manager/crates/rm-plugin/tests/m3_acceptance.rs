//! M3 验收（`docs/01` §12.3 A14–A20）：双内核（两个数据目录）环回真握手与按需传输。
//!
//! `cargo test -p rm-plugin --test m3_acceptance`

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use ling_kernel::Kernel;
use ling_sdk::Value;

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
    // 按进程隔离安装目录，避免并行测试互踩。
    let dir = root.join(format!(
        "target/debug/rmgr-plugin-install-m3-{}",
        std::process::id()
    ));
    if !dir.join("plugin.toml").is_file() {
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        fs::copy(
            root.join("plugins/resources-manager/plugin.toml"),
            dir.join("plugin.toml"),
        )
        .unwrap();
        fs::copy(
            &src,
            dir.join(format!("resources-manager{}", std::env::consts::DLL_SUFFIX)),
        )
        .unwrap();
    }
    dir
}

struct Device {
    kernel: Kernel,
    data_dir: PathBuf,
}

fn boot(root: &Path, plugin_dir: &Path) -> Device {
    let kernel = Kernel::new(root.join("ling")).unwrap();
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
    Device {
        kernel,
        data_dir,
    }
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

    fn lan_addr(&self) -> String {
        let text = fs::read_to_string(self.data_dir.join("transfer.json"))
            .expect("transfer.json 应在激活时写出");
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();
        let port = v["lan_port"].as_u64().expect("lan_port") as u16;
        format!("127.0.0.1:{port}")
    }

    fn peer_fingerprint(&self, device_id: &str) -> Option<String> {
        let conn = rusqlite::Connection::open(self.data_dir.join("catalog.db")).unwrap();
        conn.query_row(
            "SELECT fingerprint FROM peer WHERE device_id=?1",
            rusqlite::params![device_id],
            |r| r.get::<_, String>(0),
        )
        .ok()
    }

    fn transfer_row(&self, job_id: &str) -> Value {
        self.call(
            "transfer.status",
            serde_json::json!({"job_id": job_id, "page": {"limit": 10}}),
        )
    }

    fn wait_job(&self, job_id: &str, want: &str, timeout: Duration) -> Value {
        let deadline = Instant::now() + timeout;
        let mut last = Value::Null;
        while Instant::now() < deadline {
            let rows = self.transfer_row(job_id);
            last = rows.clone();
            if let Some(r) = rows["rows"].as_array().and_then(|a| a.first()) {
                if r["state"] == want {
                    return r.clone();
                }
                if want != "done" && r["state"] == "done" {
                    return r.clone();
                }
                if r["state"] == "failed" && want == "done" {
                    panic!("任务失败：{r}");
                }
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        panic!("等待 {job_id} → {want} 超时；最后状态：{last}");
    }
}

fn write_big_file(path: &Path, size: usize, seed: u8) -> String {
    if let Some(p) = path.parent() {
        fs::create_dir_all(p).unwrap();
    }
    // 可压缩性一般但足够区分的伪随机流；BLAKE3 稳定。
    let mut buf = vec![0u8; size];
    let mut x = seed as u32;
    for b in buf.iter_mut() {
        x = x.wrapping_mul(1664525).wrapping_add(1013904223);
        *b = (x >> 24) as u8;
    }
    fs::write(path, &buf).unwrap();
    blake3::hash(&buf).to_hex().to_string()
}

/// A14：8 位配对码真握手，双方 peer 表写入对方指纹。
fn a14_pair_writes_fingerprints() {
    let plugin = build_plugin_dir();
    let tmp = tempfile::tempdir().unwrap();
    let a = boot(&tmp.path().join("a"), &plugin);
    let b = boot(&tmp.path().join("b"), &plugin);

    let code = b.call("device.pairing_code", serde_json::json!({}))["code"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(code.len(), 8);
    let addr_b = b.lan_addr();
    let out = a.call("device.pair", serde_json::json!({"code": code, "addr": addr_b}));
    assert!(out["fingerprint"].as_str().unwrap().starts_with("sha256:"));
    let peer_of_b = out["device_id"].as_str().unwrap().to_string();

    // 反向：B 也配对 A（B 生成码，A 的服务端校验）。
    let code2 = a.call("device.pairing_code", serde_json::json!({}))["code"]
        .as_str()
        .unwrap()
        .to_string();
    let addr_a = a.lan_addr();
    let out2 = b.call("device.pair", serde_json::json!({"code": code2, "addr": addr_a}));
    let peer_of_a = out2["device_id"].as_str().unwrap().to_string();

    let fp_a = a.peer_fingerprint(&peer_of_b).expect("A 应写入对端指纹");
    let fp_b = b.peer_fingerprint(&peer_of_a).expect("B 应写入对端指纹");
    assert_eq!(fp_a, out["fingerprint"]);
    assert_eq!(fp_b, out2["fingerprint"]);
    assert_ne!(fp_a, fp_b, "两台设备指纹应不同");
}

/// A15：换了 identity.key 的对端再来连接 → F402，且不更新已 pin 的指纹。
fn a15_changed_identity_rejected() {
    let plugin = build_plugin_dir();
    let tmp = tempfile::tempdir().unwrap();
    let a = boot(&tmp.path().join("a"), &plugin);
    let b = boot(&tmp.path().join("b"), &plugin);

    // 首次配对（A→B）。
    let code = b.call("device.pairing_code", serde_json::json!({}))["code"]
        .as_str()
        .unwrap()
        .to_string();
    let addr_b = b.lan_addr();
    let _out = a.call("device.pair", serde_json::json!({"code": code, "addr": addr_b}));
    // B 侧对 A（dv_local）的 pin。
    let pinned_on_b = b
        .peer_fingerprint("dv_local")
        .expect("B 应已 pin A 的指纹");

    // 换掉 A 的 identity.key 并重启 A（同一数据目录）。
    a.kernel.disable("resources-manager").unwrap();
    drop(a);
    fs::write(
        tmp.path()
            .join("a/ling/plugins/resources-manager/data/identity.key"),
        [9u8; 32],
    )
    .unwrap();
    let a2 = boot(&tmp.path().join("a"), &plugin);

    // 新码再配对：B 侧 pin 了 A 的旧指纹 → F402。
    let code = b.call("device.pairing_code", serde_json::json!({}))["code"]
        .as_str()
        .unwrap()
        .to_string();
    let err = a2
        .try_call(
            "device.pair",
            serde_json::json!({"code": code, "addr": addr_b}),
        )
        .expect_err("换证书应被拒");
    assert!(
        err.code().contains("F402") || err.message().contains("指纹"),
        "期望 F402，得到 {err:?}"
    );
    // B 侧 pin 未被改写。
    assert_eq!(
        b.peer_fingerprint("dv_local").as_deref(),
        Some(pinned_on_b.as_str()),
        "F402 后 B 不应更新已 pin 指纹"
    );
}

/// A16：拉取 >12MB 文件，整文件 BLAKE3 一致，placement 已登记。
fn a16_fetch_large_file() {
    let plugin = build_plugin_dir();
    let tmp = tempfile::tempdir().unwrap();
    let a = boot(&tmp.path().join("a"), &plugin);
    let b = boot(&tmp.path().join("b"), &plugin);

    // B 上放置大文件（13 MB → 多块多连接）。
    let size = 13 * 1024 * 1024;
    let src = b.data_dir.join("files/movie.bin");
    let hash = write_big_file(&src, size, 0xA1);
    // B：登记 location + content + placement（模拟已扫描）。
    b.kernel
        .call(
            "resources-manager",
            "resources-manager.location.add",
            serde_json::json!({
                "path": b.data_dir.join("files").display().to_string(),
                "kind": "local",
            }),
        )
        .unwrap();
    let scan = b.call(
        "scan.start",
        serde_json::json!({"location_id": b.call("location.list", serde_json::json!({}))["rows"][0]["id"]}),
    );
    let job = scan["job_id"].as_str().unwrap().to_string();
    // 等扫描完成。
    for _ in 0..100 {
        std::thread::sleep(Duration::from_millis(100));
        let rows = b.call(
            "scan.stats",
            serde_json::json!({}),
        );
        if rows["files"].as_u64().unwrap_or(0) >= 1 {
            break;
        }
        let _ = job;
    }

    // 配对。
    pair_oneway(&a, &b);
    // fetch 需要本机 content 行（尺寸知识）；物理 placement 不跨设备合并。
    copy_content_row(&b, &a, &hash);

    // A：策略 mirror 才允许取回（默认 local_only 拒绝）。
    a.call(
        "transfer.policy.set",
        serde_json::json!({"scope_kind": "path_prefix", "scope_id": "/", "mode": "mirror"}),
    );
    let target = a.data_dir.join("restored/movie.bin");
    let fetch = a.call(
        "transfer.fetch",
        serde_json::json!({
            "addr": b.lan_addr(),
            "content_blake3": hash,
            "target_path": target.display().to_string(),
        }),
    );
    let job_id = fetch["job_id"].as_str().unwrap().to_string();
    let row = a.wait_job(&job_id, "done", Duration::from_secs(60));
    assert_eq!(row["offset"].as_i64().unwrap() as usize, size);

    let got = fs::read(&target).unwrap();
    assert_eq!(blake3::hash(&got).to_hex().to_string(), hash);

    // placement 已登记（incoming location）。
    let browse = a.call(
        "browse.query",
        serde_json::json!({"page": {"limit": 50}}),
    );
    let found = browse["rows"]
        .as_array()
        .map(|rows| {
            rows.iter()
                .any(|r| r["content_id"] == hash.as_str() && r["placement_id"].is_number())
        })
        .unwrap_or(false);
    assert!(found, "取回后应有 placement/browse 记录：{browse}");
}

/// A17：从确认偏移续传（不从 0 开始），最终字节一致。
fn a17_resume_from_offset() {
    let plugin = build_plugin_dir();
    let tmp = tempfile::tempdir().unwrap();
    let a = boot(&tmp.path().join("a"), &plugin);
    let b = boot(&tmp.path().join("b"), &plugin);

    let size = 8 * 1024 * 1024;
    let resume_at = 3 * 1024 * 1024;
    let src = b.data_dir.join("files/pack.bin");
    let hash = write_big_file(&src, size, 0xB2);
    register_content(&b, &src, &hash);
    pair_oneway(&a, &b);
    copy_content_row(&b, &a, &hash);
    a.call(
        "transfer.policy.set",
        serde_json::json!({"scope_kind": "path_prefix", "scope_id": "/", "mode": "mirror"}),
    );

    // 预置 .part：前 resume_at 字节正确 + transfer 行 offset=resume_at（模拟中断）。
    let bytes = fs::read(&src).unwrap();
    let target = a.data_dir.join("resume/pack.bin");
    fs::create_dir_all(target.parent().unwrap()).unwrap();
    let part = {
        let mut s = target.as_os_str().to_os_string();
        s.push(".part");
        PathBuf::from(s)
    };
    fs::write(&part, &bytes[..resume_at]).unwrap();
    let now = rm_catalog::now();
    let job_id;
    {
        let conn = rusqlite::Connection::open(a.data_dir.join("catalog.db")).unwrap();
        let id = format!("tr_test{:x}", now as u64);
        conn.execute(
            "INSERT INTO transfer(id, peer_device_id, content_blake3, target_path,
                                  offset, total, state, created, updated)
             VALUES(?1,?2,?3,?4,?5,?5,'failed',?6,?6)",
            rusqlite::params![
                id,
                b.lan_addr(),
                hash,
                target.display().to_string(),
                resume_at as i64,
                now
            ],
        )
        .unwrap();
        job_id = id;
    }
    // 重跑 fetch：应复用任务并从 resume_at 续传。
    let fetch1 = a.call(
        "transfer.fetch",
        serde_json::json!({
            "addr": b.lan_addr(),
            "content_blake3": hash,
            "target_path": target.display().to_string(),
        }),
    );
    assert_eq!(fetch1["job_id"].as_str().unwrap(), job_id, "续传应复用同一任务");
    let row = a.wait_job(&job_id, "done", Duration::from_secs(30));
    assert_eq!(row["offset"].as_i64().unwrap() as usize, size);
    let got = fs::read(&target).unwrap();
    assert_eq!(blake3::hash(&got).to_hex().to_string(), hash);
}

/// A18：cold 源 F403；改 online 后成功。
fn a18_cold_source_forbidden() {
    let plugin = build_plugin_dir();
    let tmp = tempfile::tempdir().unwrap();
    let a = boot(&tmp.path().join("a"), &plugin);
    let b = boot(&tmp.path().join("b"), &plugin);

    let size = 1024 * 64;
    let src = b.data_dir.join("files/cold.bin");
    let hash = write_big_file(&src, size, 0xC3);
    let loc_id = register_content(&b, &src, &hash);
    pair_oneway(&a, &b);
    copy_content_row(&b, &a, &hash);
    a.call(
        "transfer.policy.set",
        serde_json::json!({"scope_kind": "path_prefix", "scope_id": "/", "mode": "mirror"}),
    );

    // B：位置改 cold → A fetch 失败 F403（服务端不供数）。
    b.kernel
        .call(
            "resources-manager",
            "resources-manager.location.set_state",
            serde_json::json!({"location_id": loc_id, "state": "cold"}),
        )
        .unwrap();
    let target = a.data_dir.join("cold-out/cold.bin");
    let fetch = a.call(
        "transfer.fetch",
        serde_json::json!({
            "addr": b.lan_addr(),
            "content_blake3": hash,
            "target_path": target.display().to_string(),
        }),
    );
    let job_id = fetch["job_id"].as_str().unwrap().to_string();
    let row = a.wait_job(&job_id, "failed", Duration::from_secs(15));
    let err = row["error"].as_str().unwrap_or("").to_string();
    assert!(
        err.contains("F403") || err.contains("冷"),
        "期望 F403，得到：{err}"
    );

    // 改回 online → 用全新目标路径重取（避免残留 .part 干扰）。
    b.kernel
        .call(
            "resources-manager",
            "resources-manager.location.set_state",
            serde_json::json!({"location_id": loc_id, "state": "online"}),
        )
        .unwrap();
    std::thread::sleep(Duration::from_millis(200));
    let target2 = a.data_dir.join("cold-out2/cold.bin");
    let fetch = a.call(
        "transfer.fetch",
        serde_json::json!({
            "addr": b.lan_addr(),
            "content_blake3": hash,
            "target_path": target2.display().to_string(),
        }),
    );
    let job_id2 = fetch["job_id"].as_str().unwrap().to_string();
    let row = a.wait_job(&job_id2, "done", Duration::from_secs(30));
    if row["state"] != "done" {
        eprintln!("second fetch row: {row}");
    }
    let got = fs::read(&target2).unwrap();
    assert_eq!(
        blake3::hash(&got).to_hex().to_string(),
        hash,
        "online 后应取回一致字节"
    );
}

/// A19：配对后 sync.run 走局域网 /op/*；无 addr 仍返回 bridge 提示。
fn a19_sync_run_lan() {
    let plugin = build_plugin_dir();
    let tmp = tempfile::tempdir().unwrap();
    let a = boot(&tmp.path().join("a"), &plugin);
    let b = boot(&tmp.path().join("b"), &plugin);

    // B 上建一条记录（entry）。
    let files = b.data_dir.join("syncfiles");
    fs::create_dir_all(&files).unwrap();
    fs::write(files.join("note.txt"), b"hello-sync").unwrap();
    b.kernel
        .call(
            "resources-manager",
            "resources-manager.location.add",
            serde_json::json!({
                "path": files.display().to_string(),
                "kind": "local",
            }),
        )
        .unwrap();
    let loc = b.call("location.list", serde_json::json!({}))["rows"][0]["id"]
        .as_str()
        .unwrap()
        .to_string();
    b.call("scan.start", serde_json::json!({"location_id": loc}));
    for _ in 0..80 {
        std::thread::sleep(Duration::from_millis(100));
        if b.call("scan.stats", serde_json::json!({}))["files"].as_u64().unwrap_or(0) >= 1 {
            break;
        }
    }
    let entry = b.call(
        "entry.create",
        serde_json::json!({"title": "同步用词条", "kind": "work"}),
    );
    assert!(entry["id"].as_str().is_some());

    pair_oneway(&a, &b);

    // 无 addr 的对端（未配对但 peer 表手工插入无 addr 行）→ bridge 提示。
    // 这里 A 已有带 addr 的对端；先验证 LAN 路径。
    let sync = a.call("sync.run", serde_json::json!({}));
    assert_eq!(sync["transport"], "lan", "应走局域网：{sync}");
    assert!(sync["transport"] == "lan" || sync["applied"].as_u64().unwrap_or(0) >= 1);

    // 无 addr：插入一个无 addr 的 peer，sync.run peer_id 指向它 → bridge。
    {
        let conn = rusqlite::Connection::open(a.data_dir.join("catalog.db")).unwrap();
        conn.execute(
            "INSERT OR IGNORE INTO peer(device_id, addr, fingerprint, pubkey, known_seq, last_sync)
             VALUES('dv_offline', NULL, 'sha256:XXXX', NULL, 0, 0)",
            [],
        )
        .unwrap();
    }
    let bridge = a.call("sync.run", serde_json::json!({"peer_id": "dv_offline"}));
    assert_eq!(bridge["bridge"], "export_import", "无 addr 应走文件桥提示：{bridge}");
}

/// A20：四档策略生效；evict 仅 evictable + 有其他在线副本。
fn a20_policy_modes() {
    let plugin = build_plugin_dir();
    let tmp = tempfile::tempdir().unwrap();
    let a = boot(&tmp.path().join("a"), &plugin);
    let b = boot(&tmp.path().join("b"), &plugin);

    let size = 4096;
    let src = b.data_dir.join("files/pol.bin");
    let hash = write_big_file(&src, size, 0xD4);
    register_content(&b, &src, &hash);
    pair_oneway(&a, &b);
    copy_content_row(&b, &a, &hash);

    let target = a.data_dir.join("pol-out/pol.bin");
    let fetch_args = |mode_off: bool| {
        let mut v = serde_json::json!({
            "addr": b.lan_addr(),
            "content_blake3": hash,
            "target_path": target.display().to_string(),
        });
        if !mode_off {
            v["force"] = serde_json::json!(true);
        }
        v
    };
    let set_mode = |dev: &Device, mode: &str| {
        dev.call(
            "transfer.policy.set",
            serde_json::json!({"scope_kind": "path_prefix", "scope_id": "/", "mode": mode}),
        )
    };

    // local_only → 拒绝。
    set_mode(&a, "local_only");
    let err = a
        .try_call("transfer.fetch", fetch_args(false))
        .expect_err("local_only 应拒绝取回");
    assert_eq!(err.code(), "F403", "期望 F403，得到 {err:?}");

    // record_only → 拒绝。
    set_mode(&a, "record_only");
    let err = a
        .try_call("transfer.fetch", fetch_args(false))
        .expect_err("record_only 应拒绝取回");
    assert_eq!(err.code(), "F403");

    // mirror → 允许。
    set_mode(&a, "mirror");
    let fetch = a.call("transfer.fetch", fetch_args(false));
    let job_id = fetch["job_id"].as_str().unwrap().to_string();
    a.wait_job(&job_id, "done", Duration::from_secs(20));
    assert!(target.is_file());

    // 冗余：A 上再造一份在线副本（第二 location + 扫描），否则 evict 会因无其他副本被拦。
    let alt = a.data_dir.join("alt-copy");
    fs::create_dir_all(&alt).unwrap();
    fs::copy(&target, alt.join("pol.bin")).unwrap();
    let alt_loc = a
        .kernel
        .call(
            "resources-manager",
            "resources-manager.location.add",
            serde_json::json!({"path": alt.display().to_string(), "kind": "local"}),
        )
        .unwrap();
    let alt_loc_id = alt_loc["id"].as_str().unwrap().to_string();
    a.call("scan.start", serde_json::json!({"location_id": alt_loc_id}));
    for _ in 0..80 {
        std::thread::sleep(Duration::from_millis(50));
        if a.call("scan.stats", serde_json::json!({}))["files"]
            .as_u64()
            .unwrap_or(0)
            >= 2
        {
            break;
        }
    }

    // evictable：本机 placement 可驱逐——需其他在线副本（刚扫的 alt）。
    set_mode(&a, "evictable");
    let browse = a.call("browse.query", serde_json::json!({"page": {"limit": 50}}));
    let placement_id = browse["rows"]
        .as_array()
        .and_then(|rows| {
            rows.iter()
                .find(|r| {
                    r["content_id"] == hash.as_str()
                        && r["location_id"] != alt_loc_id.as_str()
                })
                .and_then(|r| r["placement_id"].as_i64())
        })
        .expect("应找到 incoming 上的 placement");
    let evict = a.call(
        "transfer.evict",
        serde_json::json!({"placement_ids": [placement_id]}),
    );
    assert_eq!(
        evict["evicted"].as_u64().unwrap(),
        1,
        "evictable + 有副本应驱逐：{evict}"
    );

    // mirror 档驱逐应被拦。
    set_mode(&a, "mirror");
    // 重新取回以便再测拦截（incoming 位置已被删）。
    let target2 = a.data_dir.join("pol-out2/pol.bin");
    let fetch = a.call(
        "transfer.fetch",
        serde_json::json!({
            "addr": b.lan_addr(),
            "content_blake3": hash,
            "target_path": target2.display().to_string(),
        }),
    );
    let job_id = fetch["job_id"].as_str().unwrap().to_string();
    a.wait_job(&job_id, "done", Duration::from_secs(20));
    let browse = a.call("browse.query", serde_json::json!({"page": {"limit": 50}}));
    let placement_id = browse["rows"]
        .as_array()
        .and_then(|rows| {
            rows.iter()
                .find(|r| {
                    r["content_id"] == hash.as_str()
                        && r["location_id"] != alt_loc_id.as_str()
                })
                .and_then(|r| r["placement_id"].as_i64())
        })
        .expect("再取回后应有 placement");
    let evict = a.call(
        "transfer.evict",
        serde_json::json!({"placement_ids": [placement_id]}),
    );
    assert_eq!(evict["evicted"].as_u64().unwrap(), 0);
    assert!(evict["blocked"].as_array().is_some_and(|b| !b.is_empty()));
}

// ===== helpers =====

/// A 配对 B（单向握手，写 A.peer=B）。返回后 A 可向 B fetch。
fn pair_oneway(a: &Device, b: &Device) {
    let code = b.call("device.pairing_code", serde_json::json!({}))["code"]
        .as_str()
        .unwrap()
        .to_string();
    a.call(
        "device.pair",
        serde_json::json!({"code": code, "addr": b.lan_addr()}),
    );
}

/// 在设备上登记 location + 扫描入库（同步等待）。
fn register_content(dev: &Device, file: &Path, hash: &str) -> String {
    let root = file.parent().unwrap();
    let loc = dev
        .kernel
        .call(
            "resources-manager",
            "resources-manager.location.add",
            serde_json::json!({"path": root.display().to_string(), "kind": "local"}),
        )
        .unwrap();
    let loc_id = loc["id"].as_str().unwrap().to_string();
    dev.call("scan.start", serde_json::json!({"location_id": loc_id}));
    for _ in 0..80 {
        std::thread::sleep(Duration::from_millis(50));
        if dev.call("scan.stats", serde_json::json!({}))["files"]
            .as_u64()
            .unwrap_or(0)
            >= 1
        {
            break;
        }
    }
    // 确认 content 已有。
    let conn = rusqlite::Connection::open(dev.data_dir.join("catalog.db")).ok();
    if let Some(conn) = conn {
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM content WHERE blake3=?1",
                rusqlite::params![hash],
                |r| r.get(0),
            )
            .unwrap_or(0);
        assert!(n >= 1, "扫描后 content 应有 {hash}");
    }
    loc_id
}

/// 把 content 记录（hash+size）同步进取回端目录库（fetch 需要本机 content 行取尺寸）。
/// 物理 placement 不随记录合并（物理层本地权威）；这里只补知识行。
fn copy_content_row(from: &Device, to: &Device, hash: &str) {
    let src = rusqlite::Connection::open(from.data_dir.join("catalog.db")).unwrap();
    let (size, asset): (i64, String) = src
        .query_row(
            "SELECT size, asset_type FROM content WHERE blake3=?1",
            rusqlite::params![hash],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    let dst = rusqlite::Connection::open(to.data_dir.join("catalog.db")).unwrap();
    dst.execute(
        "INSERT OR IGNORE INTO content(blake3, asset_type, size, created) VALUES(?1,?2,?3,?4)",
        rusqlite::params![hash, asset, size, rm_catalog::now()],
    )
    .unwrap();
}

#[test]
fn m3_a14_pair_writes_fingerprints() {
    a14_pair_writes_fingerprints();
}

#[test]
fn m3_a15_changed_identity_rejected() {
    a15_changed_identity_rejected();
}

#[test]
fn m3_a16_fetch_large_file() {
    a16_fetch_large_file();
}

#[test]
fn m3_a17_resume_from_offset() {
    a17_resume_from_offset();
}

#[test]
fn m3_a18_cold_source_forbidden() {
    a18_cold_source_forbidden();
}

#[test]
fn m3_a19_sync_run_lan() {
    a19_sync_run_lan();
}

#[test]
fn m3_a20_policy_modes() {
    a20_policy_modes();
}
