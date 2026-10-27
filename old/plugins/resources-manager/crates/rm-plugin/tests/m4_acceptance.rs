//! M4 验收：段式备份/恢复/校验 + zstd + 整理执行（`docs/01` §12.1/§7.7/§7.9）。
//!
//! 覆盖：
//! - 段式往返字节一致
//! - 增量跳过已存在内容
//! - 校验报"正常/不一致/缺失"
//! - 恢复目录与源 BLAKE3 一致
//! - zstd 判定阈值
//! - 占位加密往返可逆
//! - pool.ingest 整理执行 + F801 空池校验

use std::fs;
use std::path::{Path, PathBuf};

use ling_sdk::Value;

// ===== 与 m3_acceptance 相同的最小内核引导 =====

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
        "target/debug/rmgr-plugin-install-m4-{}",
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
    kernel: ling_kernel::Kernel,
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
        for _ in 0..200 {
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
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        panic!("作业未完成：{job_id}");
    }

    fn catalog_conn(&self) -> rusqlite::Connection {
        rusqlite::Connection::open(self.data_dir.join("catalog.db")).unwrap()
    }
}

fn tmp_root(tag: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("rm-m4-{tag}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    root
}

fn write_file(path: &Path, data: &[u8]) {
    if let Some(p) = path.parent() {
        fs::create_dir_all(p).unwrap();
    }
    fs::write(path, data).unwrap();
}

/// 注册一个已扫描完成的内容 + 放置（直接写库，跳过 scan）。
fn register_content(
    conn: &rusqlite::Connection,
    location_id: &str,
    rel: &str,
    data: &[u8],
    mtime: i64,
) -> i64 {
    let hash = blake3::hash(data).to_hex().to_string();
    let now = rm_catalog::now();
    conn.execute(
        "INSERT OR IGNORE INTO content(blake3, asset_type, size, created)
         VALUES(?1,'other',?2,?3)",
        rusqlite::params![hash, data.len() as i64, now],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO placement(location_id, rel_path, mode, content_id, size, mtime,
                               state, first_seen, hash_state)
         VALUES(?1,?2,'scattered',?3,?4,?5,'online',?6,'done')",
        rusqlite::params![
            location_id,
            rel,
            hash,
            data.len() as i64,
            mtime,
            now
        ],
    )
    .unwrap();
    conn.last_insert_rowid()
}

fn add_location(dev: &Device, root: &Path, label: &str) -> String {
    fs::create_dir_all(root).unwrap();
    let out = dev.call(
        "location.add",
        serde_json::json!({"path": root.display().to_string(), "label": label}),
    );
    out["id"]
        .as_str()
        .expect("location id")
        .to_string()
}

// ===== 领域级测试（不依赖内核） =====

#[test]
fn m4_domain_segment_roundtrip_incremental_verify_restore() {
    use rm_backup::{backup_restore, backup_run, backup_verify, BackupItem};
    let d = tempfile::tempdir().unwrap();
    let src = d.path().join("src");
    fs::create_dir_all(&src).unwrap();
    // 可压内容 + 不可压内容。
    let compressible = vec![b'A'; 2_000_000];
    let incompressible: Vec<u8> = (0..100_000u32).map(|i| ((i * 167 + 31) % 256) as u8).collect();
    let mut items = Vec::new();
    for data in [&compressible, &incompressible] {
        let h = blake3::hash(data).to_hex().to_string();
        let p = src.join(&h);
        fs::write(&p, data).unwrap();
        items.push(BackupItem {
            blake3: h,
            path: p,
            size: data.len() as u64,
        });
    }
    let target = d.path().join("bk");
    let st = backup_run(&target, "dv_a", "lc_h", &items, None, 1_760_000_000, &mut |_, _| {})
        .unwrap();
    assert_eq!(st.files, 2);
    assert_eq!(st.new_contents, 2);
    // manifest 存在且字段齐全。
    let man = rm_backup::format::read_manifest(&target).unwrap();
    assert_eq!(man.enc, "none");
    assert!(man.seg_size >= 128 * 1024 * 1024);

    // 增量：再跑全部跳过。
    let items2: Vec<_> = items
        .iter()
        .map(|i| rm_backup::BackupItem {
            blake3: i.blake3.clone(),
            path: i.path.clone(),
            size: i.size,
        })
        .collect();
    let st2 = backup_run(
        &target,
        "dv_a",
        "lc_h",
        &items2,
        None,
        1_760_000_100,
        &mut |_, _| {},
    )
    .unwrap();
    assert_eq!(st2.new_contents, 0);
    assert_eq!(st2.skipped, 2);

    // 全量校验。
    let rep = backup_verify(&target, true, None, &mut |_, _| {}).unwrap();
    assert_eq!(rep.ok, 2);
    assert_eq!(rep.mismatch, 0);
    assert_eq!(rep.missing, 0);

    // 恢复后 BLAKE3 一致。
    let out = d.path().join("restore");
    backup_restore(&target, &out, None, None, &mut |_, _| {}).unwrap();
    for data in [&compressible, &incompressible] {
        let h = blake3::hash(data).to_hex().to_string();
        assert_eq!(fs::read(out.join(&h)).unwrap(), *data);
    }
}

#[test]
fn m4_domain_zstd_threshold_and_placeholder() {
    use rm_backup::{backup_restore, backup_run, backup_verify, BackupItem};
    use rm_store::compress::sample_file;
    let d = tempfile::tempdir().unwrap();
    let src = d.path().join("src");
    fs::create_dir_all(&src).unwrap();
    let zeros = vec![0u8; 3_000_000];
    let p = src.join("zeros");
    fs::write(&p, &zeros).unwrap();
    let sample = sample_file(&p).unwrap();
    assert!(sample.worth, "全零应值得压：ratio={}", sample.ratio);

    let h = blake3::hash(&zeros).to_hex().to_string();
    let target = d.path().join("bk");
    let item = BackupItem {
        blake3: h.clone(),
        path: p,
        size: zeros.len() as u64,
    };
    backup_run(&target, "dv", "lc", &[item], Some("pw"), 2, &mut |_, _| {}).unwrap();
    let man = rm_backup::format::read_manifest(&target).unwrap();
    assert_eq!(man.enc, "placeholder");
    // 索引里该条应是 zstd=true（试压值得）。
    let entries = rm_backup::format::read_index(&target).unwrap();
    assert!(entries.iter().any(|e| e.zstd), "可压内容应以 zstd 落盘");

    // 无口令 → F606；有口令校验通过；恢复字节一致。
    let err = backup_verify(&target, true, None, &mut |_, _| {}).unwrap_err();
    assert_eq!(err.code, "F606");
    backup_verify(&target, true, Some("pw"), &mut |_, _| {}).unwrap();
    let out = d.path().join("out");
    backup_restore(&target, &out, None, Some("pw"), &mut |_, _| {}).unwrap();
    assert_eq!(fs::read(out.join(&h)).unwrap(), zeros);
}

// ===== 插件级验收（A21–A27 风格，用 Kernel） =====

#[test]
fn m4_plugin_backup_verify_restore_list() {
    let plugin_dir = build_plugin_dir();
    let root = tmp_root("bk");
    let dev = boot(&root, &plugin_dir);
    let data_root = root.join("media");
    let loc = add_location(&dev, &data_root, "media");
    write_file(&data_root.join("a.bin"), b"hello backup world");
    write_file(&data_root.join("sub/b.bin"), &[7u8; 500_000]);

    // 直接写库登记内容（跳过扫描，聚焦备份链路）。
    {
        let conn = dev.catalog_conn();
        register_content(&conn, &loc, "a.bin", b"hello backup world", 1);
        register_content(&conn, &loc, "sub/b.bin", &[7u8; 500_000], 1);
    }

    let target = root.join("bk_target").display().to_string();
    let run = dev.call(
        "backup.run",
        serde_json::json!({"location_id": loc, "target": target}),
    );
    let job_id = run["job_id"].as_str().unwrap().to_string();
    let row = dev.wait_job(&job_id);
    assert_eq!(row["state"], "done", "backup job log={}", row["log"]);

    // snapshot.list 非空。
    let snaps = dev.call("snapshot.list", serde_json::json!({}));
    let total = snaps["total"].as_u64().unwrap();
    assert!(total >= 1, "snapshot.list total={total}");
    let snap_id = snaps["rows"][0]["id"].as_str().unwrap().to_string();

    // backup.list 返回同一快照。
    let bks = dev.call("backup.list", serde_json::json!({}));
    assert!(bks["total"].as_u64().unwrap() >= 1);

    // verify full → done。
    let vjob = dev
        .call(
            "backup.verify",
            serde_json::json!({"target": target, "full": true}),
        )["job_id"]
        .as_str()
        .unwrap()
        .to_string();
    let vrow = dev.wait_job(&vjob);
    assert_eq!(vrow["state"], "done", "verify log={}", vrow["log"]);

    // restore 到 out 目录，内容 BLAKE3 一致。
    let out_dir = root.join("restore").display().to_string();
    let rjob = dev
        .call(
            "backup.restore",
            serde_json::json!({
                "target": target,
                "snapshot_id": snap_id,
                "out_dir": out_dir,
            }),
        )["job_id"]
        .as_str()
        .unwrap()
        .to_string();
    let rrow = dev.wait_job(&rjob);
    assert_eq!(rrow["state"], "done", "restore log={}", rrow["log"]);
    let h1 = blake3::hash(b"hello backup world").to_hex().to_string();
    assert_eq!(
        fs::read(Path::new(&out_dir).join(&h1)).unwrap(),
        b"hello backup world"
    );
}

#[test]
fn m4_plugin_pool_ingest_and_remove() {
    let plugin_dir = build_plugin_dir();
    let root = tmp_root("ingest");
    let dev = boot(&root, &plugin_dir);
    let data_root = root.join("media");
    let loc = add_location(&dev, &data_root, "media");
    let payload = b"ingest me into pool";
    write_file(&data_root.join("loose/file.bin"), payload);

    let pid = {
        let conn = dev.catalog_conn();
        register_content(&conn, &loc, "loose/file.bin", payload, 1)
    };

    // 建池。
    dev.call(
        "pool.create",
        serde_json::json!({
            "location_id": loc,
            "rel_path": "pool/main",
            "compression": "auto",
            "id": "pl_main",
        }),
    );

    // 归纳。
    let job = dev
        .call(
            "pool.ingest",
            serde_json::json!({"placement_ids": [pid], "pool_id": "pl_main"}),
        )["job_id"]
        .as_str()
        .unwrap()
        .to_string();
    let row = dev.wait_job(&job);
    assert_eq!(
        row["state"], "done",
        "ingest state={} log={}",
        row["state"], row["log"]
    );

    // 源文件已删；池内有文件；placement 变 managed。
    assert!(!data_root.join("loose/file.bin").exists(), "源应被删");
    let h = blake3::hash(payload).to_hex().to_string();
    let pool_file = data_root
        .join("pool/main")
        .join(&h[0..2])
        .join(&h[2..4])
        .join(&h);
    assert!(pool_file.is_file(), "池内应有文件：{}", pool_file.display());
    assert_eq!(fs::read(&pool_file).unwrap(), payload);

    {
        // 重开连接，确保读到插件已提交的写。
        let conn = dev.catalog_conn();
        let modes: Vec<String> = {
            let mut st = conn
                .prepare("SELECT mode FROM placement WHERE content_id=?1")
                .unwrap();
            st.query_map([&h], |r| r.get::<_, String>(0))
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap()
        };
        assert!(
            modes.iter().any(|m| m == "managed"),
            "modes={modes:?} job_log={}",
            row["log"]
        );
    }

    // 非空池 remove → F801。
    let err = dev
        .try_call("pool.remove", serde_json::json!({"pool_id": "pl_main"}))
        .unwrap_err();
    assert!(
        err.to_string().contains("F801") || err.to_string().contains("池非空"),
        "err={err}"
    );

    // pool.list 返回该池。
    let lists = dev.call("pool.list", serde_json::json!({}));
    assert!(lists["rows"]
        .as_array()
        .unwrap()
        .iter()
        .any(|r| r["id"] == "pl_main"));
}

#[test]
fn m4_plugin_compress_sample_and_run() {
    let plugin_dir = build_plugin_dir();
    let root = tmp_root("cmp");
    let dev = boot(&root, &plugin_dir);
    let data_root = root.join("media");
    let loc = add_location(&dev, &data_root, "media");
    let zeros = vec![0u8; 2_000_000];
    write_file(&data_root.join("zeros.bin"), &zeros);
    let pid = {
        let conn = dev.catalog_conn();
        register_content(&conn, &loc, "zeros.bin", &zeros, 1)
    };

    let sample = dev.call(
        "compress.sample",
        serde_json::json!({"placement_ids": [pid]}),
    );
    let rows = sample["rows"].as_array().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["worth"], true, "sample={sample}");

    let job = dev
        .call(
            "compress.run",
            serde_json::json!({"placement_ids": [pid], "mode": "always"}),
        )["job_id"]
        .as_str()
        .unwrap()
        .to_string();
    let row = dev.wait_job(&job);
    assert_eq!(
        row["state"], "done",
        "compress state={} log={}",
        row["state"], row["log"]
    );

    // content.enc 变 zstd；文件变小。
    let h = blake3::hash(&zeros).to_hex().to_string();
    {
        let conn = dev.catalog_conn();
        let (enc,): (String,) = conn
            .query_row("SELECT enc FROM content WHERE blake3=?1", [&h], |r| {
                Ok((r.get(0)?,))
            })
            .unwrap();
        assert_eq!(enc, "zstd", "job_log={}", row["log"]);
    }

    // content.enc 变 zstd；文件变小。
    let h = blake3::hash(&zeros).to_hex().to_string();
    {
        let conn = dev.catalog_conn();
        let (enc,): (String,) = conn
            .query_row("SELECT enc FROM content WHERE blake3=?1", [&h], |r| {
                Ok((r.get(0)?,))
            })
            .unwrap();
        assert_eq!(enc, "zstd", "job_log={}", row["log"]);
    }
    let fmeta = fs::metadata(data_root.join("zeros.bin")).unwrap();
    assert!(fmeta.len() < zeros.len() as u64 / 10, "zstd 后应显著变小");

    // mode=off 解回 plain。
    let job2 = dev
        .call(
            "compress.run",
            serde_json::json!({"placement_ids": [pid], "mode": "off"}),
        )["job_id"]
        .as_str()
        .unwrap()
        .to_string();
    let row2 = dev.wait_job(&job2);
    assert_eq!(row2["state"], "done");
    assert_eq!(fs::read(data_root.join("zeros.bin")).unwrap(), zeros);
}
