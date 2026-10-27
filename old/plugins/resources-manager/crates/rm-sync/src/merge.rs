//! 操作流水导出与应用合并（`docs/01` §7.5 合并规则）。
//!
//! 幂等：按 `op_id` 去重。位置状态只由所属设备改写；删除走墓碑。

use rm_proto::codes;

use crate::conflict;
use crate::oplog;

fn err(msg: impl Into<String>) -> rm_proto::BizError {
    rm_proto::BizError::new(codes::F401, msg.into())
}

/// 导出的一条 op（与 `docs/03` §5.1 载荷对齐）。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct BundleOp {
    pub seq: i64,
    pub device_id: String,
    pub op_id: String,
    pub kind: String,
    pub payload: serde_json::Value,
    pub ts: i64,
}

/// 一次应用的结果。
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct MergeReport {
    pub applied: u64,
    pub skipped: u64,
    pub conflicts: u64,
    /// 错误的应用条数（载荷损坏等）；这些 op 不更新 known_seq 之前的进度时由调用方决定。
    pub errors: Vec<String>,
}

/// 导出 seq > after_peer_known 的流水（供发给对端）。
pub fn export_bundle(
    conn: &rusqlite::Connection,
    after_seq: i64,
    limit: u32,
) -> Result<Vec<BundleOp>, rm_proto::BizError> {
    rm_catalog::repo::op::export_after(conn, after_seq, limit.clamp(1, 5000))
        .map_err(|e| err(e.to_string()))?
        .into_iter()
        .map(|(seq, device_id, op_id, kind, payload, ts)| {
            Ok(BundleOp {
                seq,
                device_id,
                op_id,
                kind,
                payload: serde_json::from_str(&payload)
                    .unwrap_or(serde_json::Value::String(payload)),
                ts,
            })
        })
        .collect()
}

/// 应用对端流水。`self_device` 是本机编号。
///
/// 合并规则（`docs/01` §7.5）：
/// - 同 op_id → 跳过（幂等）
/// - 内容指纹相同 → 同一件事
/// - 位置状态只由所属设备上报
/// - entry/version 字段级 edit-edit → 冲突队列 + 自动取较晚 ts
/// - 删除（placement_del / connection_del）立墓碑
pub fn apply_ops(
    conn: &rusqlite::Connection,
    self_device: &str,
    peer_device: &str,
    ops: &[BundleOp],
) -> Result<MergeReport, rm_proto::BizError> {
    let mut report = MergeReport::default();
    let now = rm_catalog::now();
    for op in ops {
        if rm_catalog::repo::op::exists(conn, &op.op_id).map_err(|e| err(e.to_string()))? {
            report.skipped += 1;
            continue;
        }
        match apply_one(conn, self_device, peer_device, op, now) {
            Ok(ConflictNote::None) => report.applied += 1,
            Ok(ConflictNote::Conflict) => {
                report.applied += 1;
                report.conflicts += 1;
            }
            Err(e) => {
                report.errors.push(format!("{}: {}", op.op_id, e.message));
            }
        }
    }
    Ok(report)
}

enum ConflictNote {
    None,
    Conflict,
}

fn apply_one(
    conn: &rusqlite::Connection,
    _self_device: &str,
    peer_device: &str,
    op: &BundleOp,
    now: i64,
) -> Result<ConflictNote, rm_proto::BizError> {
    // 先登记本机流水（保留原始 op_id，后续再导出时天然带去第三方）。
    // 注意：应用对端 op 时也写入本地 op 表（payload 原样），保证转发合并。
    // 为避免无限回流，应用时直接 INSERT 且不改 device_id。
    // 幂等已由 exists 检查。
    let _ = rm_catalog::repo::op::append(
        conn,
        &op.device_id,
        &op.op_id,
        &op.kind,
        &op.payload.to_string(),
        op.ts,
    )
    .map_err(|e| err(e.to_string()))?;

    match op.kind.as_str() {
        "entry_add" | "entry_update" => apply_entry(conn, peer_device, op, now),
        "version_add" => apply_version(conn, op),
        "connection_add" => apply_connection_add(conn, op),
        "connection_del" => apply_connection_del(conn, op),
        "placement_del" => apply_placement_del(conn, op),
        "state_change" => apply_state_change(conn, op),
        "tag_assign" => apply_tag_assign(conn, op),
        // content_add / location_add / device_add：M2 合并最小集——存在则跳过，不存在则不强制建（物理层本地权威）。
        "content_add" | "location_add" | "device_add" | "version_update" | "tag_add"
        | "instance_add" | "instance_del" | "placement_add" | "rule_change"
        | "conflict_resolve" => Ok(ConflictNote::None),
        other => Err(err(format!("未知操作种类：{other}"))),
    }
}

fn apply_entry(
    conn: &rusqlite::Connection,
    peer_device: &str,
    op: &BundleOp,
    _now: i64,
) -> Result<ConflictNote, rm_proto::BizError> {
    let p = &op.payload;
    let id = p["id"]
        .as_str()
        .ok_or_else(|| err("entry payload 缺 id"))?
        .to_string();
    let title = p["title"].as_str().unwrap_or("").to_string();
    let kind = p["kind"].as_str().unwrap_or("work").to_string();
    let subtitle = p["subtitle"].as_str().map(str::to_string);
    let their_ts = p["ts"].as_i64().unwrap_or(op.ts);

    let existing: Option<(String, String, Option<String>, i64)> =
        rm_catalog::repo::optional(conn.query_row(
            "SELECT title, kind, subtitle, updated FROM entry WHERE id=?1",
            rusqlite::params![id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        ))
        .map_err(|e| err(e.to_string()))?;

    match existing {
        None => {
            // 新词条：直接落地。
            conn.execute(
                "INSERT INTO entry(id, kind, title, sort_title, subtitle, created, updated)
                 VALUES(?1,?2,?3,?3,?4,?5,?5)",
                rusqlite::params![id, kind, title, subtitle, their_ts],
            )
            .map_err(|e| err(e.to_string()))?;
            Ok(ConflictNote::None)
        }
        Some((my_title, my_kind, my_subtitle, my_ts)) => {
            // 字段级比对：title / kind / subtitle。
            let mut conflicts = 0u32;
            if my_title != title && my_ts != their_ts {
                conflict::register_edit_edit(
                    conn,
                    "entry",
                    &id,
                    "title",
                    Some(&serde_json::json!(my_title).to_string()),
                    Some(&serde_json::json!(title).to_string()),
                    peer_device,
                    my_ts,
                    their_ts,
                )?;
                conflicts += 1;
            }
            if my_kind != kind && my_ts != their_ts {
                conflict::register_edit_edit(
                    conn,
                    "entry",
                    &id,
                    "kind",
                    Some(&serde_json::json!(my_kind).to_string()),
                    Some(&serde_json::json!(kind).to_string()),
                    peer_device,
                    my_ts,
                    their_ts,
                )?;
                conflicts += 1;
            }
            let sub_eq = my_subtitle.as_deref() == subtitle.as_deref();
            if !sub_eq && my_ts != their_ts {
                conflict::register_edit_edit(
                    conn,
                    "entry",
                    &id,
                    "subtitle",
                    my_subtitle
                        .as_deref()
                        .map(|s| serde_json::json!(s).to_string())
                        .as_deref()
                        .or(Some("null")),
                    subtitle
                        .as_deref()
                        .map(|s| serde_json::json!(s).to_string())
                        .as_deref()
                        .or(Some("null")),
                    peer_device,
                    my_ts,
                    their_ts,
                )?;
                conflicts += 1;
            }
            // 自动合并：较晚 ts 胜（无冲突字段各取所长；有冲突时同样取晚，败方进队列）。
            if their_ts >= my_ts {
                conn.execute(
                    "UPDATE entry SET title=?2, kind=?3, subtitle=?4, sort_title=?2, updated=?5
                     WHERE id=?1",
                    rusqlite::params![id, title, kind, subtitle, their_ts],
                )
                .map_err(|e| err(e.to_string()))?;
            }
            if conflicts > 0 {
                Ok(ConflictNote::Conflict)
            } else {
                Ok(ConflictNote::None)
            }
        }
    }
}

fn apply_version(
    conn: &rusqlite::Connection,
    op: &BundleOp,
) -> Result<ConflictNote, rm_proto::BizError> {
    let p = &op.payload;
    let id = p["id"]
        .as_str()
        .ok_or_else(|| err("version payload 缺 id"))?;
    let entry_id = p["entry_id"]
        .as_str()
        .ok_or_else(|| err("version payload 缺 entry_id"))?;
    // 词条不在本地则跳过（物理层本地权威；词条 op 会随后到）。
    let entry_ok: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM entry WHERE id=?1",
            rusqlite::params![entry_id],
            |r| r.get(0),
        )
        .map_err(|e| err(e.to_string()))?;
    if entry_ok == 0 {
        return Ok(ConflictNote::None);
    }
    let title = p["title"].as_str().unwrap_or("");
    let ordinal = p["ordinal"].as_i64().unwrap_or(1);
    let edition = p["edition"].as_str();
    let resolution = p["resolution"].as_i64();
    let codec = p["codec"].as_str();
    let language = p["language"].as_str();
    let source = p["source"].as_str().unwrap_or("sync");
    let created = p["created"].as_i64().unwrap_or(op.ts);
    conn.execute(
        "INSERT INTO version(id, entry_id, title, ordinal, edition, resolution, codec,
                             language, source, created, updated)
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?10)
         ON CONFLICT(id) DO NOTHING",
        rusqlite::params![
            id, entry_id, title, ordinal, edition, resolution, codec, language, source, created
        ],
    )
    .map_err(|e| err(e.to_string()))?;
    Ok(ConflictNote::None)
}

fn apply_connection_add(
    conn: &rusqlite::Connection,
    op: &BundleOp,
) -> Result<ConflictNote, rm_proto::BizError> {
    let p = &op.payload;
    let id = p["id"]
        .as_str()
        .ok_or_else(|| err("connection payload 缺 id"))?
        .to_string();
    let from_kind = p["from_kind"].as_str().ok_or_else(|| err("缺 from_kind"))?;
    let from_id = p["from_id"].as_str().ok_or_else(|| err("缺 from_id"))?;
    let to_kind = p["to_kind"].as_str().ok_or_else(|| err("缺 to_kind"))?;
    let to_id = p["to_id"].as_str().ok_or_else(|| err("缺 to_id"))?;
    let ctype = p["type"].as_str().ok_or_else(|| err("缺 type"))?;
    let ordinal = p["ordinal"].as_i64();
    let attrs = p["attrs_json"].to_string();
    let source = p["source"].as_str().unwrap_or("sync");
    let confidence = p["confidence"].as_f64().unwrap_or(1.0);
    let created = p["created"].as_i64().unwrap_or(op.ts);
    rm_catalog::repo::connection::upsert_live(
        conn, &id, from_kind, from_id, to_kind, to_id, ctype, ordinal, &attrs, source, confidence,
        created,
    )
    .map_err(|e| err(e.to_string()))?;
    Ok(ConflictNote::None)
}

fn apply_connection_del(
    conn: &rusqlite::Connection,
    op: &BundleOp,
) -> Result<ConflictNote, rm_proto::BizError> {
    let id = op.payload["id"]
        .as_str()
        .ok_or_else(|| err("connection_del 缺 id"))?;
    // 墓碑（幂等：已删则 no-op）。
    let _ =
        rm_catalog::repo::connection::tombstone(conn, id, op.ts).map_err(|e| err(e.to_string()))?;
    Ok(ConflictNote::None)
}

fn apply_placement_del(
    conn: &rusqlite::Connection,
    op: &BundleOp,
) -> Result<ConflictNote, rm_proto::BizError> {
    let placement_id = op.payload["placement_id"]
        .as_i64()
        .ok_or_else(|| err("placement_del 缺 placement_id"))?;
    // 本地放置标记为 deleted（不物理删，宽限期内可审）。
    let n = conn
        .execute(
            "UPDATE placement SET state='deleted' WHERE id=?1 AND state!='deleted'",
            rusqlite::params![placement_id],
        )
        .map_err(|e| err(e.to_string()))?;
    if n > 0 {
        // copy_count 同事务维护：有效放置减一。
        let cid: Option<String> = rm_catalog::repo::optional(conn.query_row(
            "SELECT content_id FROM placement WHERE id=?1",
            rusqlite::params![placement_id],
            |r| r.get(0),
        ))
        .map_err(|e| err(e.to_string()))?;
        if let Some(cid) = cid {
            conn.execute(
                "UPDATE content SET copy_count = MAX(0, copy_count - 1) WHERE blake3=?1",
                rusqlite::params![cid],
            )
            .map_err(|e| err(e.to_string()))?;
        }
    }
    Ok(ConflictNote::None)
}

fn apply_state_change(
    conn: &rusqlite::Connection,
    op: &BundleOp,
) -> Result<ConflictNote, rm_proto::BizError> {
    // 位置状态只由所属设备改写：若 op.device_id 不是该 location 的所属设备，忽略。
    let location_id = op.payload["location_id"]
        .as_str()
        .ok_or_else(|| err("state_change 缺 location_id"))?;
    let state = op.payload["state"]
        .as_str()
        .ok_or_else(|| err("state_change 缺 state"))?;
    let owner: Option<String> = rm_catalog::repo::optional(conn.query_row(
        "SELECT device_id FROM location WHERE id=?1",
        rusqlite::params![location_id],
        |r| r.get(0),
    ))
    .map_err(|e| err(e.to_string()))?;
    if let Some(owner) = owner {
        if owner != op.device_id {
            // 别的设备无权改写本机位置状态。
            return Ok(ConflictNote::None);
        }
        conn.execute(
            "UPDATE location SET state=?2 WHERE id=?1",
            rusqlite::params![location_id, state],
        )
        .map_err(|e| err(e.to_string()))?;
    }
    Ok(ConflictNote::None)
}

fn apply_tag_assign(
    conn: &rusqlite::Connection,
    op: &BundleOp,
) -> Result<ConflictNote, rm_proto::BizError> {
    let target_kind = op.payload["target_kind"]
        .as_str()
        .ok_or_else(|| err("tag_assign 缺 target_kind"))?;
    let target_id = op.payload["target_id"]
        .as_str()
        .ok_or_else(|| err("tag_assign 缺 target_id"))?;
    let tag_id = op.payload["tag_id"]
        .as_str()
        .ok_or_else(|| err("tag_assign 缺 tag_id"))?;
    // 标签或目标不存在则跳过（随后到的 tag_add / entry_add 会补）。
    let tag_ok: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM tag WHERE id=?1",
            rusqlite::params![tag_id],
            |r| r.get(0),
        )
        .unwrap_or(0);
    if tag_ok == 0 {
        return Ok(ConflictNote::None);
    }
    match target_kind {
        "entry" => {
            let _ = rm_catalog::repo::tag::entry_tag_apply(
                conn,
                target_id,
                tag_id,
                "sync",
                1.0,
                Some(&op.device_id),
                op.ts,
            );
        }
        "version" => {
            let _ = rm_catalog::repo::tag::version_tag_apply(
                conn,
                target_id,
                tag_id,
                "sync",
                1.0,
                Some(&op.device_id),
                op.ts,
            );
        }
        _ => {}
    }
    Ok(ConflictNote::None)
}

/// 便捷：生成本机一条 entry 快照 op（供测试与 CLI 构造对端流水）。
pub fn snapshot_entry_op(
    conn: &rusqlite::Connection,
    device_id: &str,
    entry_id: &str,
) -> Result<BundleOp, rm_proto::BizError> {
    let row: (String, String, Option<String>, i64) = conn
        .query_row(
            "SELECT title, kind, subtitle, updated FROM entry WHERE id=?1",
            rusqlite::params![entry_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .map_err(|e| err(format!("词条不存在：{entry_id} {e}")))?;
    let payload = serde_json::json!({
        "id": entry_id,
        "title": row.0,
        "kind": row.1,
        "subtitle": row.2,
        "ts": row.3,
    });
    let op_id = oplog::gen_op_id();
    let ts = rm_catalog::now();
    Ok(BundleOp {
        seq: 0,
        device_id: device_id.to_string(),
        op_id,
        kind: "entry_update".into(),
        payload,
        ts,
    })
}

/// 便捷：连接快照 op。
pub fn snapshot_connection_op(
    conn: &rusqlite::Connection,
    device_id: &str,
    connection_id: &str,
) -> Result<BundleOp, rm_proto::BizError> {
    let row = rm_catalog::repo::connection::get(conn, connection_id)
        .map_err(|e| err(e.to_string()))?
        .ok_or_else(|| err(format!("关系不存在：{connection_id}")))?;
    let (id, from_kind, from_id, to_kind, to_id, ctype, ordinal, created) = row;
    let payload = serde_json::json!({
        "id": id,
        "from_kind": from_kind,
        "from_id": from_id,
        "to_kind": to_kind,
        "to_id": to_id,
        "type": ctype,
        "ordinal": ordinal,
        "attrs_json": "{}",
        "source": "manual",
        "confidence": 1.0,
        "created": created,
    });
    Ok(BundleOp {
        seq: 0,
        device_id: device_id.to_string(),
        op_id: oplog::gen_op_id(),
        kind: "connection_add".into(),
        payload,
        ts: rm_catalog::now(),
    })
}

/// 便捷：版本快照 op。
pub fn snapshot_version_op(
    conn: &rusqlite::Connection,
    device_id: &str,
    version_id: &str,
) -> Result<BundleOp, rm_proto::BizError> {
    #[allow(clippy::type_complexity)]
    let (entry_id, title, ordinal, edition, resolution, codec, language, source, created): (
        String,
        String,
        i64,
        Option<String>,
        Option<i64>,
        Option<String>,
        Option<String>,
        String,
        i64,
    ) = conn
        .query_row(
            "SELECT entry_id, title, ordinal, edition, resolution, codec, language, source, created
             FROM version WHERE id=?1",
            rusqlite::params![version_id],
            |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                    r.get(6)?,
                    r.get(7)?,
                    r.get(8)?,
                ))
            },
        )
        .map_err(|e| err(format!("版本不存在：{version_id} {e}")))?;
    let payload = serde_json::json!({
        "id": version_id,
        "entry_id": entry_id,
        "title": title,
        "ordinal": ordinal,
        "edition": edition,
        "resolution": resolution,
        "codec": codec,
        "language": language,
        "source": source,
        "created": created,
    });
    Ok(BundleOp {
        seq: 0,
        device_id: device_id.to_string(),
        op_id: oplog::gen_op_id(),
        kind: "version_add".into(),
        payload,
        ts: rm_catalog::now(),
    })
}
