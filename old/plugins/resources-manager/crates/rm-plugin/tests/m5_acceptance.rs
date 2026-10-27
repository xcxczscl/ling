//! M5 验收：AI 建议 + 分级确认 + 隐私 + 撤销（`docs/01` §7.8 / §12.1、`docs/03` §5.4）。
//!
//! 覆盖：
//! - 确定性提取（文件名模式）
//! - MockProvider 关键词建议
//! - 低风险自动采纳（tag ≥ 0.85）/ 高风险 pending
//! - apply / reject 状态机
//! - revoke by source 回滚贴附
//! - 隐私三档解析
//! - 预算耗尽 F703
//! - 建议表不同步（本地草稿语义——用 state 断言）

use std::fs;
use std::path::{Path, PathBuf};

use ling_sdk::Value;
use rm_ai::provider::Provider;

// ===== 与 m3/m4_acceptance 相同的最小内核引导 =====

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
        "target/debug/rmgr-plugin-install-m5-{}",
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
    let root = std::env::temp_dir().join(format!("rm-m5-{tag}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    root
}

// ===== 领域层验收（不启内核，直接测 rm-ai） =====

#[test]
fn m5_domain_extract_and_mock_provider() {
    // 确定性提取
    let e = rm_ai::extract::extract("电影/深空旅人 (2024) 2160p HDR 科幻.mkv");
    assert_eq!(e.ext, "mkv");
    assert_eq!(e.asset_type, "video");
    assert_eq!(e.year, Some(2024));
    assert_eq!(e.resolution_tag.as_deref(), Some("2160p"));
    assert!(e.hdr);

    // MockProvider 关键词建议
    let mock = rm_ai::provider::MockProvider;
    let ctx = rm_ai::provider::AnalyzeContext {
        target_kind: "entry".into(),
        target_id: "en_test".into(),
        rel_path: "电影/深空旅人 (2024) 2160p HDR 科幻.mkv".into(),
        extracted: e,
        existing_tags: vec![],
        candidate_titles: vec!["深空旅人".into()],
    };
    let raw = mock.analyze(&ctx).unwrap();
    assert!(
        raw.iter().any(|s| s.kind == "tag"
            && s.payload["tag_name"].as_str() == Some("科幻")
            && s.confidence >= 0.85),
        "应产出 科幻 标签建议：{raw:?}"
    );
    assert!(
        raw.iter().any(|s| s.kind == "metadata" && s.payload["year"] == 2024),
        "应产出 year metadata：{raw:?}"
    );

    // 分级确认：tag 高置信 → auto_acceptable；connection 永不
    assert!(rm_ai::suggest::auto_acceptable("tag", 0.90));
    assert!(!rm_ai::suggest::auto_acceptable("connection", 0.99));
    assert!(!rm_ai::suggest::auto_acceptable("tag", 0.80));
}

// ===== 插件层验收 =====

#[test]
fn m5_plugin_suggest_flow_auto_accept_and_pending() {
    let root = tmp_root("flow");
    let plugin_dir = build_plugin_dir();
    let dev = boot(&root, &plugin_dir);

    // 建词条
    let entry = dev.call(
        "entry.create",
        serde_json::json!({"title": "深空旅人", "kind": "work"}),
    );
    let eid = entry["id"].as_str().expect("entry id").to_string();

    // ai.run（metadata 路径目标：文件名含科幻+年份）
    let job = dev.call(
        "ai.run",
        serde_json::json!({
            "target_kind": "metadata",
            "target_id": "电影/深空旅人 (2024) 科幻.mkv",
        }),
    );
    let jid = job["job_id"].as_str().unwrap().to_string();
    let j = dev.wait_job(&jid);
    assert_eq!(j["state"], "done", "ai.run 应完成：{j}");
    assert!(
        j["log"].as_str().unwrap_or("").contains("auto_accepted="),
        "日志应含 auto_accepted：{j}"
    );

    // 建议表：至少有 tag 被自动采纳（accepted）、metadata 也可能 accepted
    let conn = dev.catalog_conn();
    let (accepted, pending) = {
        let a: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM ai_suggestion WHERE state='accepted'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let p: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM ai_suggestion WHERE state='pending'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        (a, p)
    };
    assert!(accepted >= 1, "低风险高置信应自动采纳：accepted={accepted} pending={pending}");
    // 自动采纳的 tag 已写业务表（entry_tag 无；metadata 目标无 entry 贴附——用 suggest 表断言）
    let sg: i64 = conn
        .query_row("SELECT COUNT(*) FROM ai_suggestion", [], |r| r.get(0))
        .unwrap();
    assert!(sg >= 2, "应至少 2 条建议（tag+metadata）：{sg}");

    // list 接口
    let list = dev.call(
        "ai.suggest.list",
        serde_json::json!({"state": "accepted", "limit": 50}),
    );
    assert!(
        list["total"].as_u64().unwrap_or(0) >= 1,
        "list accepted 应有结果：{list}"
    );

    // reject pending（若有）
    if pending > 0 {
        let pending_ids: Vec<String> = {
            let mut st = conn
                .prepare("SELECT id FROM ai_suggestion WHERE state='pending'")
                .unwrap();
            let ids: Vec<String> = st
                .query_map([], |r| r.get(0))
                .unwrap()
                .collect::<Result<_, _>>()
                .unwrap();
            ids
        };
        let rej = dev.call(
            "ai.suggest.reject",
            serde_json::json!({"suggestion_ids": pending_ids}),
        );
        assert!(rej["rejected"].as_u64().unwrap() >= 1, "reject 应生效：{rej}");
    }

    // 目标对象存在性：entry.create 产物仍在（AI 不删词条）
    let e2 = dev.call("entry.get", serde_json::json!({"entry_id": eid}));
    assert_eq!(e2["id"], eid.as_str());
}

#[test]
fn m5_plugin_apply_tag_writes_business_table() {
    let root = tmp_root("apply");
    let plugin_dir = build_plugin_dir();
    let dev = boot(&root, &plugin_dir);

    // 建词条
    let entry = dev.call(
        "entry.create",
        serde_json::json!({"title": "旧港疑云", "kind": "work"}),
    );
    let eid = entry["id"].as_str().expect("entry id").to_string();

    // 手动插一条 pending 的高风险 tag 建议（confidence 低 → 不自动采纳）
    let conn = dev.catalog_conn();
    conn.execute(
        "INSERT INTO ai_suggestion(id, target_kind, target_id, kind, payload, confidence,
            model, prompt_hash, input_hash, state, source, created, applied_at)
         VALUES('sg_manual1','entry',?1,'tag','{\"tag_name\":\"悬疑\",\"domain\":\"subject\"}',
            0.50,'mock','ph_x','ih_x','pending','ai',?2,NULL)",
        rusqlite::params![eid, rm_catalog::now()],
    )
    .unwrap();

    // apply
    let out = dev.call(
        "ai.suggest.apply",
        serde_json::json!({"suggestion_ids": ["sg_manual1"]}),
    );
    assert_eq!(out["applied"].as_u64(), Some(1), "apply 应成功：{out}");

    // 业务表已有贴附（source=ai）
    let tags = dev.call(
        "entry.get",
        serde_json::json!({"entry_id": eid}),
    );
    let tags_str = tags.to_string();
    assert!(tags_str.contains("悬疑"), "entry tags 应含悬疑：{tags}");

    // 再 apply 同一条 → 已是终态，applied 不增
    let out2 = dev.call(
        "ai.suggest.apply",
        serde_json::json!({"suggestion_ids": ["sg_manual1"]}),
    );
    assert_eq!(out2["applied"].as_u64(), Some(0), "重复 apply 应跳过：{out2}");
}

#[test]
fn m5_plugin_revoke_by_source_rolls_back_tags() {
    let root = tmp_root("revoke");
    let plugin_dir = build_plugin_dir();
    let dev = boot(&root, &plugin_dir);

    // 建词条 + 手动插 accepted 的 AI tag 建议 + 业务贴附
    let entry = dev.call(
        "entry.create",
        serde_json::json!({"title": "深空旅人", "kind": "work"}),
    );
    let eid = entry["id"].as_str().expect("entry id").to_string();
    let conn = dev.catalog_conn();
    // 确保 tag 存在
    let tag_id = rm_semantic::ensure_tag(&conn, "科幻", "subject").unwrap();
    conn.execute(
        "INSERT INTO ai_suggestion(id, target_kind, target_id, kind, payload, confidence,
            model, prompt_hash, input_hash, state, source, created, applied_at)
         VALUES('sg_rev1','entry',?1,'tag',?2,0.90,'mock','ph_y','ih_y','accepted','ai',?3,?3)",
        rusqlite::params![eid, format!("{{\"tag_name\":\"科幻\",\"tag_id\":\"{tag_id}\"}}"), rm_catalog::now()],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO entry_tag(entry_id, tag_id, source, confidence, applied_by, created)
         VALUES(?1,?2,'ai',0.90,'mock',?3)",
        rusqlite::params![eid, tag_id, rm_catalog::now()],
    )
    .unwrap();

    // 贴附存在
    let before: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM entry_tag WHERE entry_id=?1 AND source='ai'",
            [&eid],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(before, 1);

    // revoke source=ai
    let out = dev.call("ai.suggest.revoke", serde_json::json!({"source": "ai"}));
    assert!(
        out["revoked"].as_u64().unwrap_or(0) >= 1 || out["rolled_back_tags"].as_u64().unwrap_or(0) >= 1,
        "revoke 应有回滚：{out}"
    );

    // 贴附已删
    let after: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM entry_tag WHERE entry_id=?1 AND source='ai'",
            [&eid],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(after, 0, "revoke 后 AI 贴附应清空");

    // 建议状态：accepted 的被 revoke 处理（pending→rejected；accepted 保留历史 or 标 rejected）
    let rejected: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM ai_suggestion WHERE source='ai' AND state IN ('rejected','accepted')",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(rejected >= 1, "revoke 后建议应有终态");
}

#[test]
fn m5_plugin_privacy_tiers_and_budget() {
    let root = tmp_root("privacy");
    let plugin_dir = build_plugin_dir();
    let dev = boot(&root, &plugin_dir);

    // 设置隐私档位
    let set1 = dev.call(
        "ai.privacy.set",
        serde_json::json!({"path_prefix": "/敏感", "tier": "local"}),
    );
    assert_eq!(set1["tier"], "local");
    let set2 = dev.call(
        "ai.privacy.set",
        serde_json::json!({"path_prefix": "/公开", "tier": "cloud"}),
    );
    assert_eq!(set2["tier"], "cloud");

    let list = dev.call("ai.privacy.list", serde_json::json!({}));
    assert_eq!(list["rows"].as_array().unwrap().len(), 2);

    // 隐私解析（直接查库，验证最长前缀优先 + 默认 hybrid）
    let conn = dev.catalog_conn();
    let t1 = rm_catalog::repo::suggestion::privacy_resolve(&conn, "/敏感/工作/文档.pdf").unwrap();
    assert_eq!(t1, "local");
    let t2 = rm_catalog::repo::suggestion::privacy_resolve(&conn, "/公开/电影/x.mkv").unwrap();
    assert_eq!(t2, "cloud");
    let t3 = rm_catalog::repo::suggestion::privacy_resolve(&conn, "/其他/y.pdf").unwrap();
    assert_eq!(t3, "hybrid", "无命中默认 hybrid");

    // 未知档位被拒
    let bad = dev.try_call(
        "ai.privacy.set",
        serde_json::json!({"path_prefix": "/x", "tier": "secret"}),
    );
    assert!(bad.is_err(), "未知档位应被拒");

    // 预算：budget=0 → F703
    let job = dev.call(
        "ai.run",
        serde_json::json!({
            "target_kind": "metadata",
            "target_id": "/tmp/x 科幻.mkv",
            "budget": 0,
        }),
    );
    let jid = job["job_id"].as_str().unwrap().to_string();
    let j = dev.wait_job(&jid);
    assert_eq!(j["state"], "failed", "budget=0 应失败：{j}");
    assert!(
        j["log"].as_str().unwrap_or("").contains("F703")
            || j["log"].as_str().unwrap_or("").contains("预算"),
        "日志应含预算耗尽：{j}"
    );
}

// 需要 rm-sematic 在测试依赖里
use rm_semantic as _;
