//! `connection` 关系表（`docs/02` §3.11）。

use rusqlite::{params, Connection, Result};

use super::optional;

/// 关系类型白名单（`docs/02` §3.11）。
pub const CONNECTION_TYPES: [&str; 10] = [
    "part_of",
    "contains",
    "sequel_of",
    "same_as",
    "alias_of",
    "variant_of",
    "derived_from",
    "references",
    "created_by",
    "influenced_by",
];

pub fn type_known(t: &str) -> bool {
    CONNECTION_TYPES.contains(&t)
}

/// 建立关系。唯一约束冲突 → Err。
pub fn add(
    conn: &Connection,
    id: &str,
    from_kind: &str,
    from_id: &str,
    to_kind: &str,
    to_id: &str,
    ctype: &str,
    ordinal: Option<i64>,
    source: &str,
    confidence: f64,
    now: i64,
) -> Result<()> {
    conn.execute(
        "INSERT INTO connection(id, from_kind, from_id, to_kind, to_id, type, ordinal,
                                source, confidence, created)
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
        params![id, from_kind, from_id, to_kind, to_id, ctype, ordinal, source, confidence, now],
    )?;
    Ok(())
}

/// 取关系（含已墓碑行时 deleted_at 非空）。
pub fn get(
    conn: &Connection,
    id: &str,
) -> Result<
    Option<(
        String,
        String,
        String,
        String,
        String,
        String,
        Option<i64>,
        i64,
    )>,
> {
    optional(conn.query_row(
        "SELECT id, from_kind, from_id, to_kind, to_id, type, ordinal, created
         FROM connection WHERE id=?1 AND deleted_at IS NULL",
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
            ))
        },
    ))
}

/// 列关系（可按 from/to 过滤；不含墓碑）。
pub fn list(
    conn: &Connection,
    from: Option<(&str, &str)>,
    to: Option<(&str, &str)>,
    limit: u32,
    offset: u32,
) -> Result<(
    Vec<(
        String,
        String,
        String,
        String,
        String,
        String,
        Option<i64>,
        i64,
    )>,
    u64,
)> {
    let mut where_parts = vec!["deleted_at IS NULL".to_string()];
    let mut binds: Vec<String> = Vec::new();
    if let Some((k, id)) = from {
        where_parts.push(format!(
            "from_kind=?{} AND from_id=?{}",
            binds.len() + 1,
            binds.len() + 2
        ));
        binds.push(k.to_string());
        binds.push(id.to_string());
    }
    if let Some((k, id)) = to {
        where_parts.push(format!(
            "to_kind=?{} AND to_id=?{}",
            binds.len() + 1,
            binds.len() + 2
        ));
        binds.push(k.to_string());
        binds.push(id.to_string());
    }
    let where_sql = where_parts.join(" AND ");
    let total: u64 = conn.query_row(
        &format!("SELECT COUNT(*) FROM connection WHERE {where_sql}"),
        rusqlite::params_from_iter(binds.iter()),
        |r| r.get(0),
    )?;
    let sql = format!(
        "SELECT id, from_kind, from_id, to_kind, to_id, type, ordinal, created
         FROM connection WHERE {where_sql} ORDER BY created, id LIMIT ?{} OFFSET ?{}",
        binds.len() + 1,
        binds.len() + 2
    );
    let mut all_binds = binds.clone();
    all_binds.push(limit.to_string());
    all_binds.push(offset.to_string());
    let mut st = conn.prepare(&sql)?;
    let rows = st
        .query_map(rusqlite::params_from_iter(all_binds.iter()), |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
                r.get(6)?,
                r.get(7)?,
            ))
        })?
        .collect::<Result<Vec<_>>>()?;
    Ok((rows, total))
}

/// 立墓碑（软删）。
pub fn tombstone(conn: &Connection, id: &str, now: i64) -> Result<usize> {
    conn.execute(
        "UPDATE connection SET deleted_at=?2 WHERE id=?1 AND deleted_at IS NULL",
        params![id, now],
    )
}

/// 同步合并用：按唯一键取（含墓碑）。
pub fn find_unique(
    conn: &Connection,
    from_kind: &str,
    from_id: &str,
    to_kind: &str,
    to_id: &str,
    ctype: &str,
) -> Result<Option<(String, Option<i64>, String, f64, i64, Option<i64>, String)>> {
    optional(conn.query_row(
        "SELECT id, ordinal, source, confidence, created, deleted_at, attrs_json
         FROM connection
         WHERE from_kind=?1 AND from_id=?2 AND to_kind=?3 AND to_id=?4 AND type=?5",
        params![from_kind, from_id, to_kind, to_id, ctype],
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

/// 同步合并用：插入/更新为未删状态（恢复或新建）。
pub fn upsert_live(
    conn: &Connection,
    id: &str,
    from_kind: &str,
    from_id: &str,
    to_kind: &str,
    to_id: &str,
    ctype: &str,
    ordinal: Option<i64>,
    attrs_json: &str,
    source: &str,
    confidence: f64,
    created: i64,
) -> Result<()> {
    conn.execute(
        "INSERT INTO connection(id, from_kind, from_id, to_kind, to_id, type, ordinal,
                                attrs_json, source, confidence, created, deleted_at)
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,NULL)
         ON CONFLICT(from_kind, from_id, to_kind, to_id, type) DO UPDATE SET
           id=excluded.id, ordinal=excluded.ordinal, attrs_json=excluded.attrs_json,
           source=excluded.source, confidence=excluded.confidence,
           created=excluded.created, deleted_at=NULL",
        params![
            id, from_kind, from_id, to_kind, to_id, ctype, ordinal, attrs_json, source, confidence,
            created
        ],
    )?;
    Ok(())
}
