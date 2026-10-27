//! `version` / `version_file` / `instance` 表。

use rusqlite::{params, Connection, Result};

use super::optional;

pub fn add(
    conn: &Connection,
    id: &str,
    entry_id: &str,
    title: &str,
    ordinal: i64,
    edition: Option<&str>,
    resolution: Option<i64>,
    codec: Option<&str>,
    language: Option<&str>,
    source: &str,
    now: i64,
) -> Result<()> {
    conn.execute(
        "INSERT INTO version(id, entry_id, title, ordinal, edition, resolution, codec,
                             language, source, state, created, updated)
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,'draft',?10,?10)",
        params![id, entry_id, title, ordinal, edition, resolution, codec, language, source, now],
    )?;
    Ok(())
}

pub fn link_file(
    conn: &Connection,
    version_id: &str,
    rel_path: &str,
    content_blake3: &str,
    size: i64,
    role: &str,
) -> Result<()> {
    conn.execute(
        "INSERT INTO version_file(version_id, rel_path, content_blake3, size, role)
         VALUES(?1,?2,?3,?4,?5)",
        params![version_id, rel_path, content_blake3, size, role],
    )?;
    Ok(())
}

pub fn files_of(conn: &Connection, version_id: &str) -> Result<Vec<(String, String, i64, String)>> {
    let mut st = conn.prepare(
        "SELECT rel_path, content_blake3, size, role FROM version_file
         WHERE version_id=?1 ORDER BY rel_path",
    )?;
    let rows = st
        .query_map(params![version_id], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
        })?
        .collect::<Result<Vec<_>>>()?;
    Ok(rows)
}

/// 版本清单是否引用了某内容（安全删除检查 4，`docs/01` §7.3）。
pub fn referenced_content(conn: &Connection, content_id: &str) -> Result<bool> {
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM version_file WHERE content_blake3=?1",
        params![content_id],
        |r| r.get(0),
    )?;
    Ok(n > 0)
}

pub fn versions_of(
    conn: &Connection,
    entry_id: &str,
) -> Result<Vec<(String, String, i64, String, Option<i64>, Option<String>)>> {
    let mut st = conn.prepare(
        "SELECT id, title, ordinal, state, resolution, codec FROM version
         WHERE entry_id=?1 ORDER BY ordinal",
    )?;
    let rows = st
        .query_map(params![entry_id], |r| {
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

pub fn instance_register(
    conn: &Connection,
    id: &str,
    version_id: &str,
    location_id: &str,
    path: &str,
    now: i64,
) -> Result<()> {
    conn.execute(
        "INSERT INTO instance(id, version_id, location_id, path, state, matched_by, created)
         VALUES(?1,?2,?3,?4,'unverified','manual',?5)",
        params![id, version_id, location_id, path, now],
    )?;
    Ok(())
}

pub fn instance_get(conn: &Connection, id: &str) -> Result<Option<String>> {
    optional(
        conn.query_row("SELECT id FROM instance WHERE id=?1", params![id], |r| {
            r.get(0)
        }),
    )
}
