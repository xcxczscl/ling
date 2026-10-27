//! M1 验收（`docs/01` §12.1）：A2/A3/A4/A5/A8/A9 领域级集成。
//! 扫描 → 分层哈希 → 查重 → 词条/标签 → 规则贴标。

use std::sync::atomic::AtomicBool;

use rm_catalog::repo::{placement, version};
use rm_proto::msg::ClassifyRule;

fn seed_location(conn: &rusqlite::Connection, id: &str, root: &str) {
    let _ = conn.execute(
        "INSERT OR IGNORE INTO device VALUES('dv_local','本机','linux','fp','online',0)",
        [],
    );
    let _ = conn.execute(
        "INSERT OR IGNORE INTO location VALUES(?1,'dv_local',?2,'local','online',NULL)",
        rusqlite::params![id, root],
    );
}

#[test]
fn m1_scan_dedup_entry_tag_classify_flow() {
    let mut conn = rm_catalog::open_memory().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    // A2: 两个相同内容、不同路径 → 扫描后形成重复组（copy_count=2）。
    std::fs::write(root.join("dup_a.bin"), b"same-bytes").unwrap();
    std::fs::write(root.join("dup_b.bin"), b"same-bytes").unwrap();
    std::fs::write(root.join("unique.bin"), b"other-bytes").unwrap();
    seed_location(&conn, "loc1", root.to_str().unwrap());

    let cancel = AtomicBool::new(false);
    let mut prog = |_p: &rm_scan::ScanProgress| {};
    let out = rm_scan::scan_location(
        &mut conn,
        rm_scan::ScanOptions {
            location_id: "loc1",
            root,
            full: false,
            batch_size: 100,
            ignores: &[],
        },
        &cancel,
        &mut prog,
    )
    .unwrap();
    assert_eq!(out.placements_new, 3);
    assert_eq!(out.progress.files_hashed, 3);

    // A3: 重复组检测。
    let (groups, total_groups) = rm_dedup::dup_groups(&conn, None, 10, 0).unwrap();
    assert_eq!(total_groups, 1);
    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0].copy_count, 2);
    assert_eq!(groups[0].placements.len(), 2);
    let wasted = groups[0].size * (groups[0].copy_count - 1);
    assert_eq!(wasted, b"same-bytes".len() as i64);

    // 安全删除：唯一在线且未校验副本 → 拦截。
    let pid: i64 = conn
        .query_row(
            "SELECT id FROM placement WHERE rel_path='unique.bin'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let plan = rm_dedup::plan_delete(&conn, &[pid], false).unwrap();
    assert!(matches!(
        plan.verdict,
        rm_dedup::DeleteVerdict::Blocked { .. }
    ));

    // A8: 词条 + 版本 + 文件清单链接。
    let entry =
        rm_semantic::create_entry(&conn, "示例影片", "work", Some("一部测试用影片")).unwrap();
    let version_id = rm_semantic::add_version(
        &conn,
        &entry,
        "1080p",
        1,
        Some("蓝光"),
        Some(1080),
        Some("hevc"),
        Some("zh"),
        Some("scan"),
    )
    .unwrap();
    let dup_hash: String = conn
        .query_row(
            "SELECT content_id FROM placement WHERE rel_path='dup_a.bin'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let dup_size: i64 = conn
        .query_row(
            "SELECT size FROM placement WHERE rel_path='dup_a.bin'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    version::link_file(&conn, &version_id, "dup_a.bin", &dup_hash, dup_size, "main").unwrap();

    // A9: 标签域 + 应用。
    let tag = rm_semantic::ensure_tag(&conn, "视频", "subject").unwrap();
    rm_semantic::apply_tag(&conn, "entry", &entry, &tag).unwrap();
    let tags = rm_catalog::repo::tag::entry_tags(&conn, &entry).unwrap();
    assert!(tags.iter().any(|(_, name, _, _)| name == "视频"));

    // A4: 规则导入并执行 → 版本文件被贴标。
    let rules = vec![ClassifyRule {
        id: "r_mv".into(),
        priority: 10,
        match_spec: serde_json::json!({"path_contains": "dup_"}),
        action: serde_json::json!({"add_tag": "重复源", "domain": "subject"}),
        enabled: true,
    }];
    rm_classify::import_rules(&conn, &rules).unwrap();
    let run = rm_classify::run_rules(&conn, None).unwrap();
    assert!(run.rules_fired >= 1);
    assert!(run.tags_applied >= 1);
    let vtag_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM version_tag vt JOIN tag t ON t.id=vt.tag_id WHERE t.name='重复源' AND vt.version_id=?1",
            rusqlite::params![version_id],
            |r| r.get(0),
        )
        .unwrap();
    assert!(vtag_count >= 1);

    // A5 派生：stats 与 browse。
    let (files, bytes, contents) = placement::scan_totals(&conn).unwrap();
    assert_eq!(files, 3);
    assert_eq!(contents, 2); // dup + unique
    assert!(bytes > 0);
    let (rows, total) = placement::browse(&conn, Some("loc1"), None, 100, 0).unwrap();
    assert_eq!(total, 3);
    assert_eq!(rows.len(), 3);
}
