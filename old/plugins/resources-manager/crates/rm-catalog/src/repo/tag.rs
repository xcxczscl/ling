//! `tag` / `tag_domain` / `entry_tag` / `version_tag`：分域标签与贴附。

use rusqlite::{params, Connection, Result};

use super::optional;

/// 预置域（`docs/02` §3.7）。
pub const BUILTIN_DOMAINS: [(&str, &str, &str); 5] = [
    ("subject", "题材", "entry"),
    ("format", "文件格式", "version"),
    ("quality", "画质", "version"),
    ("source", "来源", "version"),
    ("status", "处理状态", "either"),
];

pub fn ensure_builtin_domains(conn: &Connection) -> Result<()> {
    for (id, name, subject) in BUILTIN_DOMAINS {
        conn.execute(
            "INSERT OR IGNORE INTO tag_domain(id, name, builtin, default_subject)
             VALUES(?1,?2,1,?3)",
            params![id, name, subject],
        )?;
    }
    Ok(())
}

pub fn domain_list(conn: &Connection) -> Result<Vec<(String, String, i64, String)>> {
    let mut st =
        conn.prepare("SELECT id, name, builtin, default_subject FROM tag_domain ORDER BY id")?;
    let rows = st
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
        .collect::<Result<Vec<_>>>()?;
    Ok(rows)
}

pub fn tag_upsert(
    conn: &Connection,
    id: &str,
    parent_id: Option<&str>,
    domain: &str,
    name: &str,
    now: i64,
) -> Result<()> {
    conn.execute(
        "INSERT INTO tag(id, parent_id, domain, name, created) VALUES(?1,?2,?3,?4,?5)
         ON CONFLICT(parent_id, name) DO UPDATE SET domain=excluded.domain",
        params![id, parent_id, domain, name, now],
    )?;
    Ok(())
}

pub fn tag_get(
    conn: &Connection,
    id: &str,
) -> Result<Option<(String, Option<String>, String, String)>> {
    optional(conn.query_row(
        "SELECT id, parent_id, domain, name FROM tag WHERE id=?1",
        params![id],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
    ))
}

pub fn tag_list(
    conn: &Connection,
    domain: Option<&str>,
) -> Result<Vec<(String, Option<String>, String, String)>> {
    let out = match domain {
        Some(d) => {
            let mut st = conn.prepare(
                "SELECT id, parent_id, domain, name FROM tag WHERE domain=?1 ORDER BY name",
            )?;
            let mapped = st.query_map(params![d], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
            })?;
            mapped.collect::<Result<Vec<_>>>()?
        }
        None => {
            let mut st =
                conn.prepare("SELECT id, parent_id, domain, name FROM tag ORDER BY domain, name")?;
            let mapped = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?;
            mapped.collect::<Result<Vec<_>>>()?
        }
    };
    Ok(out)
}

pub fn entry_tag_apply(
    conn: &Connection,
    entry_id: &str,
    tag_id: &str,
    source: &str,
    confidence: f64,
    applied_by: Option<&str>,
    now: i64,
) -> Result<()> {
    conn.execute(
        "INSERT INTO entry_tag(entry_id, tag_id, source, confidence, applied_by, created)
         VALUES(?1,?2,?3,?4,?5,?6)
         ON CONFLICT(entry_id, tag_id) DO NOTHING",
        params![entry_id, tag_id, source, confidence, applied_by, now],
    )?;
    Ok(())
}

/// 撤销：删除指定来源的词条贴附（如 source='ai'）。
pub fn entry_tag_revoke(conn: &Connection, entry_id: &str, tag_id: &str, source: &str) -> Result<usize> {
    conn.execute(
        "DELETE FROM entry_tag WHERE entry_id=?1 AND tag_id=?2 AND source=?3",
        params![entry_id, tag_id, source],
    )
}

/// 撤销：删除指定来源的全部词条贴附（revoke by source）。
pub fn entry_tag_revoke_by_source(conn: &Connection, source: &str) -> Result<usize> {
    conn.execute(
        "DELETE FROM entry_tag WHERE source=?1",
        params![source],
    )
}

/// 撤销：删除指定来源的版本贴附。
pub fn version_tag_revoke_by_source(conn: &Connection, source: &str) -> Result<usize> {
    conn.execute(
        "DELETE FROM version_tag WHERE source=?1",
        params![source],
    )
}

pub fn version_tag_apply(
    conn: &Connection,
    version_id: &str,
    tag_id: &str,
    source: &str,
    confidence: f64,
    applied_by: Option<&str>,
    now: i64,
) -> Result<()> {
    conn.execute(
        "INSERT INTO version_tag(version_id, tag_id, source, confidence, applied_by, created)
         VALUES(?1,?2,?3,?4,?5,?6)
         ON CONFLICT(version_id, tag_id) DO NOTHING",
        params![version_id, tag_id, source, confidence, applied_by, now],
    )?;
    Ok(())
}

/// 词条的标签（带域名与标签名）。
pub fn entry_tags(
    conn: &Connection,
    entry_id: &str,
) -> Result<Vec<(String, String, String, String)>> {
    let mut st = conn.prepare(
        "SELECT t.id, t.name, t.domain, et.source FROM entry_tag et
         JOIN tag t ON t.id = et.tag_id WHERE et.entry_id=?1 ORDER BY t.domain, t.name",
    )?;
    let rows = st
        .query_map(params![entry_id], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
        })?
        .collect::<Result<Vec<_>>>()?;
    Ok(rows)
}

/// 版本的标签（带域名与标签名）。
pub fn version_tag_list(
    conn: &Connection,
    version_id: &str,
) -> Result<Vec<(String, String, String, String)>> {
    let mut st = conn.prepare(
        "SELECT t.id, t.name, t.domain, vt.source FROM version_tag vt
         JOIN tag t ON t.id = vt.tag_id WHERE vt.version_id=?1 ORDER BY t.domain, t.name",
    )?;
    let rows = st
        .query_map(params![version_id], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
        })?
        .collect::<Result<Vec<_>>>()?;
    Ok(rows)
}

/// 派生标签：从版本贴附聚合（不落 entry_tag，`docs/02` §3.10）。
pub fn derived_entry_tags(
    conn: &Connection,
    entry_id: &str,
) -> Result<Vec<(String, String, String)>> {
    let mut st = conn.prepare(
        "SELECT DISTINCT t.id, t.name, t.domain FROM version_tag vt
         JOIN version v ON v.id = vt.version_id
         JOIN tag t ON t.id = vt.tag_id
         WHERE v.entry_id=?1 ORDER BY t.domain, t.name",
    )?;
    let rows = st
        .query_map(params![entry_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<Result<Vec<_>>>()?;
    Ok(rows)
}

/// 按标签筛词条：tag 参数是标签 id。
pub fn entries_with_tag(conn: &Connection, tag_id: &str) -> Result<Vec<String>> {
    let mut st = conn.prepare("SELECT entry_id FROM entry_tag WHERE tag_id=?1")?;
    let rows = st
        .query_map(params![tag_id], |r| r.get(0))?
        .collect::<Result<Vec<_>>>()?;
    Ok(rows)
}
