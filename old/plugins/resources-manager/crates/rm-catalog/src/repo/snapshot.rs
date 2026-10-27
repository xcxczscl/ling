//! `snapshot` 快照表（M4，`docs/02` §6.3）。

use rusqlite::{params, Connection, Result};

use super::optional;

/// 登记一条快照（幂等：同 (location, target, created) 只算一条）。
pub fn insert(
    conn: &Connection,
    id: &str,
    location_id: &str,
    target: &str,
    created: i64,
    manifest_hash: &str,
    stats_json: &str,
) -> Result<()> {
    conn.execute(
        "INSERT OR IGNORE INTO snapshot(id, location_id, target, created, manifest_hash, stats_json)
         VALUES(?1,?2,?3,?4,?5,?6)",
        params![id, location_id, target, created, manifest_hash, stats_json],
    )?;
    Ok(())
}

pub fn get(
    conn: &Connection,
    id: &str,
) -> Result<Option<(String, String, String, i64, String, String)>> {
    optional(conn.query_row(
        "SELECT id, location_id, target, created, manifest_hash, stats_json
         FROM snapshot WHERE id=?1",
        params![id],
        |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
            ))
        },
    ))
}

/// 按位置过滤的分页列表；返回 (rows, total)。
pub fn list(
    conn: &Connection,
    location_id: Option<&str>,
    limit: u32,
    offset: u32,
) -> Result<(Vec<(String, String, String, i64, String, String)>, u64)> {
    let total: u64 = match location_id {
        Some(l) => conn.query_row(
            "SELECT COUNT(*) FROM snapshot WHERE location_id=?1",
            params![l],
            |r| r.get(0),
        )?,
        None => conn.query_row("SELECT COUNT(*) FROM snapshot", [], |r| r.get(0))?,
    };
    let rows = match location_id {
        Some(l) => {
            let mut st = conn.prepare(
                "SELECT id, location_id, target, created, manifest_hash, stats_json
                 FROM snapshot WHERE location_id=?1 ORDER BY created DESC LIMIT ?2 OFFSET ?3",
            )?;
            let out = st
                .query_map(params![l, limit, offset], |r| {
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
            out
        }
        None => {
            let mut st = conn.prepare(
                "SELECT id, location_id, target, created, manifest_hash, stats_json
                 FROM snapshot ORDER BY created DESC LIMIT ?1 OFFSET ?2",
            )?;
            let out = st
                .query_map(params![limit, offset], |r| {
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
            out
        }
    };
    Ok((rows, total))
}

/// 某目标上最近一条快照（backup.verify 缺省 snapshot_id 时用）。
pub fn latest_for_target(
    conn: &Connection,
    target: &str,
) -> Result<Option<(String, String, String, i64, String, String)>> {
    optional(conn.query_row(
        "SELECT id, location_id, target, created, manifest_hash, stats_json
         FROM snapshot WHERE target=?1 ORDER BY created DESC LIMIT 1",
        params![target],
        |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
            ))
        },
    ))
}
