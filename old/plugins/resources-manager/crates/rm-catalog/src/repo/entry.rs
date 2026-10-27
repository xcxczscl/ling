//! `entry` 表：词条 CRUD 与浏览。

use rusqlite::{params, Connection, Result};

use super::optional;

pub fn create(
    conn: &Connection,
    id: &str,
    kind: &str,
    title: &str,
    subtitle: Option<&str>,
    now: i64,
) -> Result<()> {
    conn.execute(
        "INSERT INTO entry(id, kind, title, sort_title, subtitle, created, updated)
         VALUES(?1,?2,?3,?3,?4,?5,?5)",
        params![id, kind, title, subtitle, now],
    )?;
    Ok(())
}

pub fn get(
    conn: &Connection,
    id: &str,
) -> Result<Option<(String, String, String, Option<String>, i64, i64)>> {
    optional(conn.query_row(
        "SELECT id, kind, title, subtitle, created, updated FROM entry WHERE id=?1",
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

/// 列词条：可按 kind / 标题前缀过滤，分页。
pub fn list(
    conn: &Connection,
    kind: Option<&str>,
    q: Option<&str>,
    limit: u32,
    offset: u32,
) -> Result<(Vec<(String, String, String, Option<String>, i64, i64)>, u64)> {
    let mut where_parts = vec!["1=1".into()];
    let mut binds: Vec<String> = Vec::new();
    if let Some(k) = kind {
        where_parts.push(format!("kind = ?{}", binds.len() + 1));
        binds.push(k.to_string());
    }
    if let Some(query) = q {
        where_parts.push(format!("title LIKE ?{} || '%'", binds.len() + 1));
        binds.push(query.to_string());
    }
    let w = where_parts.join(" AND ");
    let total: u64 = conn.query_row(
        &format!("SELECT COUNT(*) FROM entry WHERE {w}"),
        rusqlite::params_from_iter(binds.iter()),
        |r| r.get(0),
    )?;
    let mut st = conn.prepare(&format!(
        "SELECT id, kind, title, subtitle, created, updated FROM entry
         WHERE {w} ORDER BY sort_title LIMIT ?{} OFFSET ?{}",
        binds.len() + 1,
        binds.len() + 2
    ))?;
    binds.push(limit.to_string());
    binds.push(offset.to_string());
    let rows = st
        .query_map(rusqlite::params_from_iter(binds.iter()), |r| {
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
    Ok((rows, total))
}

pub fn add_alias(conn: &Connection, entry_id: &str, alias: &str) -> Result<()> {
    conn.execute(
        "INSERT OR IGNORE INTO entry_alias(entry_id, alias) VALUES(?1,?2)",
        params![entry_id, alias],
    )?;
    Ok(())
}

pub fn aliases_of(conn: &Connection, entry_id: &str) -> Result<Vec<String>> {
    let mut st = conn.prepare("SELECT alias FROM entry_alias WHERE entry_id=?1 ORDER BY alias")?;
    let rows = st
        .query_map(params![entry_id], |r| r.get(0))?
        .collect::<Result<Vec<_>>>()?;
    Ok(rows)
}

/// 外部资料（`docs/02` §3.3）：资料站拉取的简介/封面等，带来源地址 + 抓取时间。
pub fn ext_upsert(
    conn: &Connection,
    entry_id: &str,
    source: &str,
    external_id: &str,
    url: Option<&str>,
    fetched_at: i64,
) -> Result<()> {
    conn.execute(
        "INSERT INTO entry_ext(entry_id, source, external_id, url, fetched_at)
         VALUES(?1,?2,?3,?4,?5)
         ON CONFLICT(entry_id, source) DO UPDATE SET
           external_id=excluded.external_id, url=excluded.url, fetched_at=excluded.fetched_at",
        params![entry_id, source, external_id, url, fetched_at],
    )?;
    Ok(())
}

/// 按来源取外部资料（source, external_id, url, fetched_at）。
pub fn ext_of(
    conn: &Connection,
    entry_id: &str,
) -> Result<Vec<(String, String, Option<String>, Option<i64>)>> {
    let mut st = conn.prepare(
        "SELECT source, external_id, url, fetched_at FROM entry_ext WHERE entry_id=?1 ORDER BY source",
    )?;
    let rows = st
        .query_map(params![entry_id], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
        })?
        .collect::<Result<Vec<_>>>()?;
    Ok(rows)
}
