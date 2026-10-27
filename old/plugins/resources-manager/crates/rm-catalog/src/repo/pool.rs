//! `pool` 存储池表（`docs/02` §2.5、§7.9 归纳执行）。

use rusqlite::{params, Connection, Result};

use super::optional;

/// 池行：(id, location_id, rel_path, capacity_hint, compression, last_verified, state)。
pub type PoolRow = (
    String,
    String,
    String,
    Option<i64>,
    String,
    Option<i64>,
    String,
);

pub fn create(
    conn: &Connection,
    id: &str,
    location_id: &str,
    rel_path: &str,
    capacity_hint: Option<i64>,
    compression: &str,
    now: i64,
) -> Result<()> {
    conn.execute(
        "INSERT INTO pool(id, location_id, rel_path, capacity_hint, compression, last_verified, state)
         VALUES(?1,?2,?3,?4,?5,?6,'online')
         ON CONFLICT(id) DO UPDATE SET rel_path=excluded.rel_path",
        params![id, location_id, rel_path, capacity_hint, compression, now],
    )?;
    Ok(())
}

pub fn get(conn: &Connection, id: &str) -> Result<Option<PoolRow>> {
    optional(conn.query_row(
        "SELECT id, location_id, rel_path, capacity_hint, compression, last_verified, state
         FROM pool WHERE id=?1",
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
            ))
        },
    ))
}

pub fn list(conn: &Connection) -> Result<Vec<PoolRow>> {
    let mut st = conn.prepare(
        "SELECT id, location_id, rel_path, capacity_hint, compression, last_verified, state
         FROM pool ORDER BY id",
    )?;
    let rows = st
        .query_map([], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
                r.get(6)?,
            ))
        })?
        .collect::<Result<Vec<_>>>()?;
    Ok(rows)
}

pub fn set_last_verified(conn: &Connection, id: &str, now: i64) -> Result<()> {
    conn.execute(
        "UPDATE pool SET last_verified=?2 WHERE id=?1",
        params![id, now],
    )?;
    Ok(())
}

pub fn set_state(conn: &Connection, id: &str, state: &str) -> Result<()> {
    conn.execute("UPDATE pool SET state=?2 WHERE id=?1", params![id, state])?;
    Ok(())
}

/// 池内放置数（判断空池）。
pub fn count_placements(conn: &Connection, pool_id: &str) -> Result<i64> {
    conn.query_row(
        "SELECT COUNT(*) FROM placement WHERE pool_id=?1",
        params![pool_id],
        |r| r.get(0),
    )
}

/// 池内放置的 (id, content_id, size, rel_path, state)。
pub fn placements_in(
    conn: &Connection,
    pool_id: &str,
) -> Result<Vec<(i64, String, i64, String, String)>> {
    let mut st = conn.prepare(
        "SELECT id, content_id, size, rel_path, state FROM placement
         WHERE pool_id=?1 ORDER BY id",
    )?;
    let rows = st
        .query_map(params![pool_id], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
        })?
        .collect::<Result<Vec<_>>>()?;
    Ok(rows)
}

/// 选有空间的 online 池：capacity_hint 最大的；无池返回 None。
pub fn pick_online(conn: &Connection) -> Result<Option<PoolRow>> {
    let mut rows = list(conn)?;
    rows.retain(|p| p.6 == "online");
    // capacity_hint 大者优先；无 hint 排后。
    rows.sort_by_key(|p| std::cmp::Reverse(p.3.unwrap_or(-1)));
    Ok(rows.into_iter().next())
}

/// 更新放置的 mode/pool_id/rel_path（归纳提交）。
pub fn set_placement_managed(
    conn: &Connection,
    placement_id: i64,
    pool_id: &str,
    rel_path: &str,
) -> Result<()> {
    conn.execute(
        "UPDATE placement SET mode='managed', pool_id=?2, rel_path=?3 WHERE id=?1",
        params![placement_id, pool_id, rel_path],
    )?;
    Ok(())
}

/// 取放置的 (location_id, rel_path, content_id, size, mode, state)。
pub fn placement_path_info(
    conn: &Connection,
    id: i64,
) -> Result<Option<(String, String, String, i64, String, String)>> {
    optional(conn.query_row(
        "SELECT location_id, rel_path, content_id, size, mode, state
         FROM placement WHERE id=?1",
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

/// 某位置全部在线放置的 (placement_id, content_id, size, rel_path, mode, state)。
pub fn placements_of_location(
    conn: &Connection,
    location_id: &str,
) -> Result<Vec<(i64, String, i64, String, String, String)>> {
    let mut st = conn.prepare(
        "SELECT id, content_id, size, rel_path, mode, state FROM placement
         WHERE location_id=?1 ORDER BY id",
    )?;
    let rows = st
        .query_map(params![location_id], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?))
        })?
        .collect::<Result<Vec<_>>>()?;
    Ok(rows)
}

/// 同事务内复制一条 placement 到受管模式（复制而非替换，便于删源前双份存在）。
pub fn placement_add_managed(
    conn: &Connection,
    location_id: &str,
    rel_path: &str,
    content_id: &str,
    size: i64,
    mtime: i64,
    pool_id: &str,
    state: &str,
    now: i64,
) -> Result<i64> {
    conn.execute(
        "INSERT INTO placement(location_id, rel_path, mode, pool_id, content_id, size, mtime,
                               state, first_seen, hash_state)
         VALUES(?1,?2,'managed',?3,?4,?5,?6,?7,?8,'done')",
        params![location_id, rel_path, pool_id, content_id, size, mtime, state, now],
    )?;
    conn.execute(
        "UPDATE content SET copy_count = copy_count + 1 WHERE blake3=?1",
        params![content_id],
    )?;
    Ok(conn.last_insert_rowid())
}
