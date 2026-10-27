//! 冲突队列：登记 edit-edit / delete-edit，裁决生成更晚的新 op（A-16）。

use rm_proto::codes;

use crate::oplog;

fn err(msg: impl Into<String>) -> rm_proto::BizError {
    rm_proto::BizError::new(codes::F401, msg.into())
}

fn gen_conflict_id() -> String {
    let ts = rm_catalog::now() as u64;
    let n = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    format!("cf_{ts:x}{n:08x}")
}

/// 登记一条 edit-edit 冲突（幂等：同 object+field+their_device 只一条 pending）。
#[allow(clippy::too_many_arguments)]
pub fn register_edit_edit(
    conn: &rusqlite::Connection,
    object_kind: &str,
    object_id: &str,
    field: &str,
    my_value: Option<&str>,
    their_value: Option<&str>,
    their_device: &str,
    my_ts: i64,
    their_ts: i64,
) -> Result<(), rm_proto::BizError> {
    let my_device: String = conn
        .query_row(
            "SELECT id FROM device ORDER BY CASE WHEN id='dv_local' THEN 0 ELSE 1 END, id LIMIT 1",
            [],
            |r| r.get(0),
        )
        .unwrap_or_else(|_| "dv_local".into());
    let id = gen_conflict_id();
    rm_catalog::repo::conflict::insert_pending(
        conn,
        &id,
        object_kind,
        object_id,
        field,
        my_value,
        their_value,
        &my_device,
        their_device,
        my_ts,
        their_ts,
        Some("manual"),
        Some("sync"),
        "edit_edit",
        rm_catalog::now(),
    )
    .map_err(|e| err(e.to_string()))?;
    let _ = conn.execute(
        "UPDATE sync_conflict SET my_source='manual', their_source='sync' WHERE id=?1",
        rusqlite::params![id],
    );
    Ok(())
}

/// 登记 delete-edit（本机删、对端改；或反之）。
#[allow(clippy::too_many_arguments)]
pub fn register_delete_edit(
    conn: &rusqlite::Connection,
    object_kind: &str,
    object_id: &str,
    my_deleted: bool,
    my_value: Option<&str>,
    their_value: Option<&str>,
    their_device: &str,
    my_ts: i64,
    their_ts: i64,
) -> Result<(), rm_proto::BizError> {
    let my_device: String = conn
        .query_row(
            "SELECT id FROM device ORDER BY CASE WHEN id='dv_local' THEN 0 ELSE 1 END, id LIMIT 1",
            [],
            |r| r.get(0),
        )
        .unwrap_or_else(|_| "dv_local".into());
    let field = "__tombstone__";
    let id = gen_conflict_id();
    rm_catalog::repo::conflict::insert_pending(
        conn,
        &id,
        object_kind,
        object_id,
        field,
        my_value,
        their_value,
        &my_device,
        their_device,
        my_ts,
        their_ts,
        Some("manual"),
        Some("sync"),
        if my_deleted {
            "delete_edit"
        } else {
            "edit_delete"
        },
        rm_catalog::now(),
    )
    .map_err(|e| err(e.to_string()))?;
    Ok(())
}

/// 冲突行：(id, object_kind, object_id, field, my_value, their_value,
/// my_device, their_device, my_ts, their_ts, conflict_kind, state)。
pub type ConflictRow = (
    String,
    String,
    String,
    String,
    Option<String>,
    Option<String>,
    String,
    String,
    i64,
    i64,
    String,
    String,
);

/// 列待决（或指定状态）冲突。
pub fn list_pending(
    conn: &rusqlite::Connection,
    state: Option<&str>,
    limit: u32,
    offset: u32,
) -> Result<(Vec<ConflictRow>, u64), rm_proto::BizError> {
    rm_catalog::repo::conflict::list(conn, state, limit, offset).map_err(|e| err(e.to_string()))
}

pub fn pending_count(conn: &rusqlite::Connection) -> Result<u64, rm_proto::BizError> {
    rm_catalog::repo::conflict::pending_count(conn).map_err(|e| err(e.to_string()))
}

/// 裁决：生成**时间戳更晚**的新 op（`kind=conflict_resolve`），对端旧值永远打不过。
pub fn resolve_conflict(
    conn: &rusqlite::Connection,
    device_id: &str,
    conflict_id: &str,
    choice: &str,
    manual_value: Option<serde_json::Value>,
) -> Result<(), rm_proto::BizError> {
    let full = rm_catalog::repo::conflict::get_full(conn, conflict_id)
        .map_err(|e| err(e.to_string()))?
        .ok_or_else(|| err(format!("冲突不存在：{conflict_id}")))?;
    let (
        object_kind,
        object_id,
        field,
        my_value,
        their_value,
        _my_dev,
        _their_dev,
        _my_ts,
        _their_ts,
        _ck,
        state,
    ) = full;
    if state != "pending" {
        return Err(err(format!("冲突已裁决：{conflict_id}")));
    }
    let now = rm_catalog::now();
    // 胜方值。
    let winner: Option<serde_json::Value> = match choice {
        "keep_mine" => my_value.and_then(|v| serde_json::from_str(&v).ok()),
        "keep_theirs" => their_value.and_then(|v| serde_json::from_str(&v).ok()),
        "manual" => manual_value,
        "restore_delete" => None,
        other => return Err(err(format!("未知裁决：{other}"))),
    };
    // 写回对象 + 生成更晚的新 op。
    match object_kind.as_str() {
        "entry" => {
            if let Some(v) = &winner {
                // 胜方值可能是整对象（旧路径）或字段级 JSON 标量（edit-edit）。
                let (mut title, mut kind, mut subtitle) = (String::new(), "work".to_string(), None);
                if let Some(obj) = v.as_object() {
                    title = obj
                        .get("title")
                        .and_then(|x| x.as_str())
                        .unwrap_or("")
                        .to_string();
                    kind = obj
                        .get("kind")
                        .and_then(|x| x.as_str())
                        .unwrap_or("work")
                        .to_string();
                    subtitle = obj.get("subtitle").cloned().and_then(|x| {
                        if x.is_null() {
                            None
                        } else {
                            x.as_str().map(str::to_string)
                        }
                    });
                } else {
                    // 字段级：按 field 写回。
                    match field.as_str() {
                        "title" => {
                            title = v.as_str().unwrap_or("").to_string();
                        }
                        "kind" => {
                            kind = v.as_str().unwrap_or("work").to_string();
                        }
                        "subtitle" => {
                            subtitle = if v.is_null() {
                                None
                            } else {
                                v.as_str().map(str::to_string)
                            };
                        }
                        _ => {}
                    }
                }
                // 保留未冲突字段：读现值覆盖缺省。
                let cur: (String, String, Option<String>) = conn
                    .query_row(
                        "SELECT title, kind, subtitle FROM entry WHERE id=?1",
                        rusqlite::params![object_id],
                        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                    )
                    .map_err(|e| err(e.to_string()))?;
                if title.is_empty() {
                    title = cur.0;
                }
                if kind == "work" && field != "kind" {
                    kind = cur.1;
                }
                if subtitle.is_none() && field != "subtitle" {
                    subtitle = cur.2;
                }
                conn.execute(
                    "UPDATE entry SET title=?2, kind=?3, subtitle=?4, sort_title=?2, updated=?5
                     WHERE id=?1",
                    rusqlite::params![object_id, title, kind, subtitle, now],
                )
                .map_err(|e| err(e.to_string()))?;
            } else if choice == "restore_delete" {
                // 撤销删除：无删除语义的 entry 仅更新时间戳；此处仅记 op。
            }
            let payload = serde_json::json!({
                "conflict_id": conflict_id,
                "object_kind": object_kind,
                "object_id": object_id,
                "field": field,
                "choice": choice,
                "winner": winner,
            });
            oplog::append_local(conn, device_id, "conflict_resolve", payload)?;
            // 同时把获胜值作为 entry_update 再写一条，确保对端合并取晚。
            if winner.is_some() {
                let row: (String, String, Option<String>) = conn
                    .query_row(
                        "SELECT title, kind, subtitle FROM entry WHERE id=?1",
                        rusqlite::params![object_id],
                        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                    )
                    .map_err(|e| err(e.to_string()))?;
                let update = serde_json::json!({
                    "id": object_id,
                    "title": row.0,
                    "kind": row.1,
                    "subtitle": row.2,
                    "ts": now,
                });
                oplog::append_local(conn, device_id, "entry_update", update)?;
            }
        }
        "connection" => {
            if choice == "restore_delete" {
                let _ = rm_catalog::repo::connection::tombstone(conn, &object_id, now);
                oplog::append_local(
                    conn,
                    device_id,
                    "connection_del",
                    serde_json::json!({"id": object_id}),
                )?;
            } else if let Some(v) = &winner {
                let id = v["id"].as_str().unwrap_or(&object_id).to_string();
                let payload = serde_json::json!({
                    "id": id,
                    "from_kind": v["from_kind"].as_str().unwrap_or("entry"),
                    "from_id": v["from_id"].as_str().unwrap_or(""),
                    "to_kind": v["to_kind"].as_str().unwrap_or("entry"),
                    "to_id": v["to_id"].as_str().unwrap_or(""),
                    "type": v["type"].as_str().unwrap_or("related"),
                    "ordinal": v["ordinal"],
                    "attrs_json": "{}",
                    "source": "manual",
                    "confidence": 1.0,
                    "created": v["created"].as_i64().unwrap_or(now),
                });
                oplog::append_local(conn, device_id, "connection_add", payload)?;
            }
        }
        other => {
            return Err(err(format!("暂不支持裁决对象：{other}")));
        }
    }
    rm_catalog::repo::conflict::resolve(conn, conflict_id, choice, now)
        .map_err(|e| err(e.to_string()))?;
    Ok(())
}
