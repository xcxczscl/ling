//! `sync_conflict` 冲突队列（`docs/02` §4.3）：本地视角，不同步。

use rusqlite::{params, Connection, Result};

use super::optional;

#[allow(clippy::too_many_arguments)]
pub fn insert_pending(
    conn: &Connection,
    id: &str,
    object_kind: &str,
    object_id: &str,
    field: &str,
    my_value: Option<&str>,
    their_value: Option<&str>,
    my_device: &str,
    their_device: &str,
    my_ts: i64,
    their_ts: i64,
    my_source: Option<&str>,
    their_source: Option<&str>,
    conflict_kind: &str,
    discovered: i64,
) -> Result<()> {
    conn.execute(
        "INSERT INTO sync_conflict(id, object_kind, object_id, field, my_value, their_value,
            my_device, their_device, my_ts, their_ts, my_source, their_source,
            conflict_kind, state, discovered)
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,'pending',?14)
         ON CONFLICT(object_kind, object_id, field, their_device) DO UPDATE SET
           my_value=excluded.my_value, their_value=excluded.their_value,
           my_ts=excluded.my_ts, their_ts=excluded.their_ts,
           my_source=excluded.my_source, their_source=excluded.their_source,
           conflict_kind=excluded.conflict_kind,
           state=CASE WHEN sync_conflict.state='resolved' THEN 'pending' ELSE sync_conflict.state END,
           resolution=NULL, resolved_at=NULL,
           discovered=excluded.discovered",
        params![
            id,
            object_kind,
            object_id,
            field,
            my_value,
            their_value,
            my_device,
            their_device,
            my_ts,
            their_ts,
            my_source,
            their_source,
            conflict_kind,
            discovered
        ],
    )?;
    Ok(())
}

pub fn get(
    conn: &Connection,
    id: &str,
) -> Result<Option<(String, String, String, String, String)>> {
    optional(conn.query_row(
        "SELECT object_kind, object_id, field, state, conflict_kind FROM sync_conflict WHERE id=?1",
        params![id],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
    ))
}

/// 取冲突完整行（resolve 用）。
pub fn get_full(
    conn: &Connection,
    id: &str,
) -> Result<
    Option<(
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
    )>,
> {
    optional(conn.query_row(
        "SELECT object_kind, object_id, field, my_value, their_value,
                my_device, their_device, my_ts, their_ts, conflict_kind, state
         FROM sync_conflict WHERE id=?1",
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
                r.get(9)?,
                r.get(10)?,
            ))
        },
    ))
}

pub fn list(
    conn: &Connection,
    state: Option<&str>,
    limit: u32,
    offset: u32,
) -> Result<(
    Vec<(
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
    )>,
    u64,
)> {
    let st_sql = match state {
        Some(s) => format!("state='{s}'"),
        None => "state='pending'".to_string(),
    };
    let total: u64 = conn.query_row(
        &format!("SELECT COUNT(*) FROM sync_conflict WHERE {st_sql}"),
        [],
        |r| r.get(0),
    )?;
    let mut st = conn.prepare(&format!(
        "SELECT id, object_kind, object_id, field, my_value, their_value,
                my_device, their_device, my_ts, their_ts, conflict_kind, state
         FROM sync_conflict WHERE {st_sql}
         ORDER BY discovered DESC, id LIMIT ?1 OFFSET ?2"
    ))?;
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
                r.get(9)?,
                r.get(10)?,
                r.get(11)?,
            ))
        })?
        .collect::<Result<Vec<_>>>()?;
    Ok((rows, total))
}

pub fn resolve(conn: &Connection, id: &str, resolution: &str, now: i64) -> Result<usize> {
    conn.execute(
        "UPDATE sync_conflict SET state='resolved', resolution=?2, resolved_at=?3
         WHERE id=?1 AND state='pending'",
        params![id, resolution, now],
    )
}

pub fn pending_count(conn: &Connection) -> Result<u64> {
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM sync_conflict WHERE state='pending'",
        [],
        |r| r.get(0),
    )?;
    Ok(n as u64)
}
