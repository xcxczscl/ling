//! `op` 操作流水（`docs/02` §4.1）：只增不减，同步唯一载体。

use rusqlite::{params, Connection, Result};

/// 追加一条 op。返回 seq。
pub fn append(
    conn: &Connection,
    device_id: &str,
    op_id: &str,
    kind: &str,
    payload: &str,
    ts: i64,
) -> Result<i64> {
    conn.execute(
        "INSERT INTO op(device_id, op_id, kind, payload, ts) VALUES(?1,?2,?3,?4,?5)",
        params![device_id, op_id, kind, payload, ts],
    )?;
    Ok(conn.last_insert_rowid())
}

/// 已存在该 op_id？（幂等）
pub fn exists(conn: &Connection, op_id: &str) -> Result<bool> {
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM op WHERE op_id=?1",
        params![op_id],
        |r| r.get(0),
    )?;
    Ok(n > 0)
}

/// 导出：seq > after_seq 的前 limit 条，按 seq 升序。
pub fn export_after(
    conn: &Connection,
    after_seq: i64,
    limit: u32,
) -> Result<Vec<(i64, String, String, String, String, i64)>> {
    let mut st = conn.prepare(
        "SELECT seq, device_id, op_id, kind, payload, ts FROM op
         WHERE seq > ?1 ORDER BY seq LIMIT ?2",
    )?;
    let rows = st
        .query_map(params![after_seq, limit as i64], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
            ))
        })?
        .collect::<Result<Vec<_>>>()?;
    Ok(rows)
}

/// 最大 seq。
pub fn max_seq(conn: &Connection) -> Result<i64> {
    let n: i64 = conn
        .query_row("SELECT COALESCE(MAX(seq),0) FROM op", [], |r| r.get(0))
        .unwrap_or(0);
    Ok(n)
}

/// 本地未同步给某对端的条数（seq > peer.known_seq）。
pub fn pending_for_peer(conn: &Connection, known_seq: i64) -> Result<u64> {
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM op WHERE seq > ?1",
        params![known_seq],
        |r| r.get(0),
    )?;
    Ok(n as u64)
}
