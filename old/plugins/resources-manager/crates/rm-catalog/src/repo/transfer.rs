//! `transfer` 传输任务表（`docs/02` §4.4；断点续传的落库依据）。

use rusqlite::{params, Connection, Result};

use super::optional;

/// (id, peer_device_id, content_blake3, target_path, offset, total, state, error, updated)
pub type TransferRow = (
    String,
    String,
    String,
    String,
    i64,
    i64,
    String,
    Option<String>,
    i64,
);

/// 新任务编号。
pub fn new_id(now: i64) -> String {
    let n = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    format!("tr_{now:x}{n:08x}")
}

/// 登记/取回任务：同 (content, target) 幂等，返回 (id, 已确认 offset, 当前 state)。
///
/// 已存在任务时只把 state 推回 `queued`（供续传），**不重置 offset**。
pub fn upsert(
    conn: &Connection,
    peer_device_id: &str,
    content_blake3: &str,
    target_path: &str,
    now: i64,
) -> Result<(String, i64, String)> {
    if let Some((id, offset, state)) = optional(conn.query_row(
        "SELECT id, offset, state FROM transfer WHERE content_blake3=?1 AND target_path=?2",
        params![content_blake3, target_path],
        |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?, r.get::<_, String>(2)?)),
    ))? {
        if state != "done" {
            conn.execute(
                "UPDATE transfer SET state='queued', peer_device_id=?2, updated=?3 WHERE id=?1",
                params![id, peer_device_id, now],
            )?;
            return Ok((id, offset, "queued".to_string()));
        }
        return Ok((id, offset, state));
    }
    let id = new_id(now);
    conn.execute(
        "INSERT INTO transfer(id, peer_device_id, content_blake3, target_path, offset, total,
                              state, created, updated)
         VALUES(?1,?2,?3,?4,0,0,'queued',?5,?5)",
        params![id, peer_device_id, content_blake3, target_path, now],
    )?;
    Ok((id, 0, "queued".to_string()))
}

pub fn get(conn: &Connection, id: &str) -> Result<Option<TransferRow>> {
    optional(conn.query_row(
        "SELECT id, peer_device_id, content_blake3, target_path, offset, total, state, error, updated
         FROM transfer WHERE id=?1",
        params![id],
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
    ))
}

/// 推进确认偏移与总字节（仅 running/queued 任务）。
pub fn set_progress(
    conn: &Connection,
    id: &str,
    offset: i64,
    total: i64,
    now: i64,
) -> Result<()> {
    conn.execute(
        "UPDATE transfer SET offset=?2, total=?3, updated=?4 WHERE id=?1",
        params![id, offset, total, now],
    )?;
    Ok(())
}

/// 置任务状态；`err` 为失败原因（含错误码），其余状态清空。
pub fn set_state(
    conn: &Connection,
    id: &str,
    state: &str,
    err: Option<&str>,
    now: i64,
) -> Result<()> {
    conn.execute(
        "UPDATE transfer SET state=?2, error=?3, updated=?4 WHERE id=?1",
        params![id, state, err, now],
    )?;
    Ok(())
}

/// 任务列表：`job_id` 缺省时按更新时间倒序分页。返回 (rows, total)。
pub fn list(
    conn: &Connection,
    job_id: Option<&str>,
    limit: u32,
    offset: u32,
) -> Result<(Vec<TransferRow>, u64)> {
    if let Some(id) = job_id {
        let row = get(conn, id)?;
        let total = if row.is_some() { 1 } else { 0 };
        return Ok((row.into_iter().collect(), total));
    }
    let total: u64 = conn.query_row("SELECT COUNT(*) FROM transfer", [], |r| r.get(0))?;
    let mut st = conn.prepare(
        "SELECT id, peer_device_id, content_blake3, target_path, offset, total, state, error, updated
         FROM transfer ORDER BY updated DESC LIMIT ?1 OFFSET ?2",
    )?;
    let rows = st
        .query_map(params![limit as i64, offset as i64], |r| {
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
        })?
        .collect::<Result<Vec<_>>>()?;
    Ok((rows, total))
}
