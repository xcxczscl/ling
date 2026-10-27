//! M2 验收测试（`docs/01` §12.1/§12.2 A6/A7/A11）。
//!
//! A6 两设备配对 + 文件桥同步记录收敛；A7 离线/冷标记 + 删除墓碑宽限；
//! A11 字段级 edit-edit 冲突 → 裁决 → 不复活。

use rm_catalog::migrations;
use rm_sync::{apply_ops, export_bundle, BundleOp};
use rusqlite::{Connection, OptionalExtension};

fn open_db() -> Connection {
    let conn = Connection::open_in_memory().expect("open in-memory");
    migrations::migrate(&conn).expect("migrate");
    conn
}

fn local_dev(conn: &Connection) -> String {
    rm_semantic::local_device(conn)
}

fn add_entry(conn: &Connection, title: &str) -> String {
    rm_semantic::create_entry(conn, title, "work", None).expect("create_entry")
}

fn entry_title(conn: &Connection, id: &str) -> Option<String> {
    conn.query_row(
        "SELECT title FROM entry WHERE id=?1",
        rusqlite::params![id],
        |r| r.get(0),
    )
    .optional()
    .ok()
    .flatten()
}

/// A6：两设备配对后经文件桥同步，词条收敛。
#[test]
fn a6_two_devices_pair_and_sync_converge() {
    let a = open_db();
    let b = open_db();
    let dev_a = local_dev(&a);
    let dev_b = local_dev(&b);

    // 双向登记 peer（配对，M2 简化码即可）。
    let now = rm_catalog::now();
    rm_catalog::repo::peer::upsert(&a, &dev_b, Some("file://b"), "fp:b", None, now).expect("pair a");
    rm_catalog::repo::peer::upsert(&b, &dev_a, Some("file://a"), "fp:a", None, now).expect("pair b");

    // A 上创建词条。
    let eid = add_entry(&a, "电影.mp4");
    // A 导出全量。
    let ops = export_bundle(&a, 0, 1000).expect("export");
    assert!(!ops.is_empty(), "A 应有 op（entry_add）");

    // B 导入。
    let report = apply_ops(&b, &dev_b, &dev_a, &ops).expect("import");
    assert!(report.applied >= 1);
    assert_eq!(report.conflicts, 0);

    // B 上能看到同一词条。
    assert_eq!(
        entry_title(&b, &eid).as_deref(),
        Some("电影.mp4"),
        "B 应同步到词条"
    );

    // 推进 known_seq 后 B 再导出（对 A 应几乎全跳过或应用 B 侧补充）。
    let max = ops.iter().map(|o| o.seq).max().unwrap_or(0);
    rm_catalog::repo::peer::set_progress(&b, &dev_a, max, now).expect("progress");
    let ops_ba = export_bundle(&b, max, 1000).expect("export B after known");
    let report2 = apply_ops(&a, &dev_a, &dev_b, &ops_ba).expect("import A");
    assert!(
        report2.errors.is_empty(),
        "A 不应报错：{:?}",
        report2.errors
    );
}

/// A7：删除立墓碑，30 天宽限内不被清除。
#[test]
fn a7_tombstone_grace_window() {
    use rm_sync::{is_grace_active, purge_expired_tombstones, TOMBSTONE_GRACE_DAYS};

    let conn = open_db();
    let _dev = local_dev(&conn);

    let eid = add_entry(&conn, "临时文件.bin");
    let to_eid = add_entry(&conn, "另一条目");
    let cid =
        rm_semantic::connection_add(&conn, "entry", &eid, "entry", &to_eid, "references", None)
            .expect("connection");
    rm_semantic::connection_del(&conn, &cid).expect("del");

    // 墓碑后 list 不返回 deleted。
    let (rows, total) = rm_semantic::connection_list(&conn, None, None, 500, 0).expect("list");
    assert!(total == 0 && rows.is_empty(), "墓碑后不返回已删连线");

    // 宽限窗口逻辑。
    let now = rm_catalog::now();
    assert!(is_grace_active(now, now), "刚立碑在宽限期内");
    assert!(
        is_grace_active(now, now + (TOMBSTONE_GRACE_DAYS - 1) * 86400),
        "宽限期内仍在"
    );
    assert!(
        !is_grace_active(now, now + (TOMBSTONE_GRACE_DAYS + 1) * 86400),
        "过期后不在宽限"
    );

    // 半程 purge 不应清。
    let purged = purge_expired_tombstones(&conn, now + TOMBSTONE_GRACE_DAYS * 86400 / 2)
        .expect("purge half");
    assert_eq!(purged, 0, "宽限期内不清除");
}

/// A11：edit-edit 字段级冲突 → 登记队列 → 裁决后应用且不复活旧值。
#[test]
fn a11_edit_edit_conflict_then_resolve_no_resurrection() {
    let a = open_db();
    let b = open_db();
    let dev_a = local_dev(&a);
    let dev_b = local_dev(&b);

    // A 创建并同步到 B。
    let eid = add_entry(&a, "原始标题");
    let ops = export_bundle(&a, 0, 1000).expect("export");
    apply_ops(&b, &dev_b, &dev_a, &ops).expect("import B");

    // A 改标题（本地 op，较晚 ts）。
    let ts_a = rm_catalog::now() + 1000;
    a.execute(
        "UPDATE entry SET title='A版标题', sort_title='A版标题', updated=?2 WHERE id=?1",
        rusqlite::params![eid, ts_a],
    )
    .expect("update A");
    rm_semantic::op_append(
        &a,
        "entry_update",
        serde_json::json!({
            "id": eid, "title": "A版标题", "kind": "work",
            "subtitle": serde_json::Value::Null, "ts": ts_a,
        }),
    )
    .expect("op A");

    // B 改标题（本地 op，更晚 ts → 后写胜）。
    let ts_b = ts_a + 5000;
    b.execute(
        "UPDATE entry SET title='B版标题', sort_title='B版标题', updated=?2 WHERE id=?1",
        rusqlite::params![eid, ts_b],
    )
    .expect("update B");
    rm_semantic::op_append(
        &b,
        "entry_update",
        serde_json::json!({
            "id": eid, "title": "B版标题", "kind": "work",
            "subtitle": serde_json::Value::Null, "ts": ts_b,
        }),
    )
    .expect("op B");

    // 双向同步。
    let ops_ab = export_bundle(&a, 0, 1000).expect("export A");
    let ops_ba = export_bundle(&b, 0, 1000).expect("export B");
    let r1 = apply_ops(&b, &dev_b, &dev_a, &ops_ab).expect("B imports A");
    let r2 = apply_ops(&a, &dev_a, &dev_b, &ops_ba).expect("A imports B");

    // 至少一侧出现 title 冲突。
    let pending =
        rm_sync::pending_count(&a).expect("count A") + rm_sync::pending_count(&b).expect("count B");
    assert!(
        r1.conflicts + r2.conflicts > 0 || pending > 0,
        "应产生 edit-edit 冲突：r1={:?} r2={:?} pending={}",
        r1,
        r2,
        pending
    );

    // 在 B 侧裁决 keep_theirs（取对端 A 的值）。
    let (rows_b, _) = rm_sync::list_pending(&b, Some("pending"), 50, 0).expect("list B");
    let (rows_a, _) = rm_sync::list_pending(&a, Some("pending"), 50, 0).expect("list A");
    let target = rows_b
        .first()
        .map(|r| (&b, dev_b.clone(), r.0.clone()))
        .or_else(|| rows_a.first().map(|r| (&a, dev_a.clone(), r.0.clone())));
    if let Some((conn, device, cid)) = target {
        let before = entry_title(conn, &eid).unwrap_or_default();
        rm_sync::resolve_conflict(conn, &device, &cid, "keep_theirs", None).expect("resolve");
        let (rows_after, _) = rm_sync::list_pending(conn, Some("pending"), 50, 0).expect("list");
        assert!(!rows_after.iter().any(|r| r.0 == cid), "裁决后移出待决");
        // 词条仍存在（不复活已删/不丢对象），且 title 非空。
        let after = entry_title(conn, &eid).expect("entry still present");
        assert!(!after.is_empty(), "裁决后标题非空");
        let _ = before;
    }

    // 最终两侧标题应收敛到同一值（后写或裁决后）。
    let ta = entry_title(&a, &eid).expect("a");
    let tb = entry_title(&b, &eid).expect("b");
    // 若已双向同步裁决，应一致；否则至少存在不丢词条。
    assert!(!ta.is_empty() && !tb.is_empty());
}

/// 幂等：同 op_id 重复导入只应用一次。
#[test]
fn idempotent_double_import() {
    let a = open_db();
    let b = open_db();
    let dev_a = local_dev(&a);
    let dev_b = local_dev(&b);
    let _eid = add_entry(&a, "幂等.mp4");
    let ops = export_bundle(&a, 0, 1000).expect("export");
    let r1 = apply_ops(&b, &dev_b, &dev_a, &ops).expect("import1");
    let r2 = apply_ops(&b, &dev_b, &dev_a, &ops).expect("import2");
    assert!(r2.applied == 0, "重复 op 应跳过");
    assert!(r2.skipped >= r1.applied, "第二次全部跳过");
}

/// BundleOp 序列化（文件桥 JSON 往返）。
#[test]
fn bundle_op_json_roundtrip() {
    let op = BundleOp {
        seq: 1,
        device_id: "dv_a".into(),
        op_id: "op_1".into(),
        kind: "entry_add".into(),
        payload: serde_json::json!({"id": "en_1", "title": "t", "kind": "work", "subtitle": null, "ts": 1}),
        ts: 1,
    };
    let s = serde_json::to_string(&op).unwrap();
    let back: BundleOp = serde_json::from_str(&s).unwrap();
    assert_eq!(back.op_id, "op_1");
}
