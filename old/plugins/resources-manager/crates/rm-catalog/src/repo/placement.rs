//! `placement` / `location` / `device`：物理层登记与 `copy_count` 同事务维护。

use rusqlite::{params, Connection, Result};

use super::optional;

// ===== device =====

pub fn device_upsert(
    conn: &Connection,
    id: &str,
    name: &str,
    platform: &str,
    fingerprint: &str,
    now: i64,
) -> Result<()> {
    conn.execute(
        "INSERT INTO device(id, name, platform, fingerprint, last_seen, state)
         VALUES(?1,?2,?3,?4,?5,'online')
         ON CONFLICT(id) DO UPDATE SET name=excluded.name, last_seen=excluded.last_seen",
        params![id, name, platform, fingerprint, now],
    )?;
    Ok(())
}

pub fn device_list(conn: &Connection) -> Result<Vec<(String, String, String, String, i64)>> {
    let mut st =
        conn.prepare("SELECT id, name, platform, state, last_seen FROM device ORDER BY id")?;
    let rows = st
        .query_map([], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
        })?
        .collect::<Result<Vec<_>>>()?;
    Ok(rows)
}

// ===== location =====

pub fn location_add(
    conn: &Connection,
    id: &str,
    device_id: &str,
    root_path: &str,
    kind: &str,
    label: Option<&str>,
) -> Result<()> {
    conn.execute(
        "INSERT INTO location(id, device_id, root_path, kind, state, label)
         VALUES(?1,?2,?3,?4,'online',?5)
         ON CONFLICT(id) DO UPDATE SET label=excluded.label",
        params![id, device_id, root_path, kind, label],
    )?;
    Ok(())
}

pub fn location_get(
    conn: &Connection,
    id: &str,
) -> Result<Option<(String, String, String, String, Option<String>)>> {
    optional(conn.query_row(
        "SELECT id, root_path, kind, state, label FROM location WHERE id=?1",
        params![id],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
    ))
}

pub fn location_set_state(conn: &Connection, id: &str, state: &str) -> Result<usize> {
    conn.execute(
        "UPDATE location SET state=?2 WHERE id=?1",
        params![id, state],
    )
}

pub fn location_list(
    conn: &Connection,
) -> Result<Vec<(String, String, String, String, Option<String>)>> {
    let mut st =
        conn.prepare("SELECT id, root_path, kind, state, label FROM location ORDER BY id")?;
    let rows = st
        .query_map([], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
        })?
        .collect::<Result<Vec<_>>>()?;
    Ok(rows)
}

// ===== placement =====

/// 按 (location_id, rel_path) 查已有放置。
pub fn placement_find(
    conn: &Connection,
    location_id: &str,
    rel_path: &str,
) -> Result<Option<(i64, String, i64, i64, Option<i64>)>> {
    // (id, content_id, size, mtime, inode)
    optional(conn.query_row(
        "SELECT id, content_id, size, mtime, inode FROM placement
         WHERE location_id=?1 AND rel_path=?2",
        params![location_id, rel_path],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
    ))
}

/// 新增放置并在同事务内 copy_count+1。返回 placement id。
pub fn placement_add(
    conn: &Connection,
    location_id: &str,
    rel_path: &str,
    content_id: &str,
    size: i64,
    mtime: i64,
    inode: Option<i64>,
    state: &str,
    now: i64,
) -> Result<i64> {
    conn.execute(
        "INSERT INTO placement(location_id, rel_path, mode, content_id, size, mtime, inode,
                               state, first_seen, hash_state)
         VALUES(?1,?2,'scattered',?3,?4,?5,?6,?7,?8,'done')",
        params![
            location_id,
            rel_path,
            content_id,
            size,
            mtime,
            inode,
            state,
            now
        ],
    )?;
    conn.execute(
        "UPDATE content SET copy_count = copy_count + 1 WHERE blake3=?1",
        params![content_id],
    )?;
    Ok(conn.last_insert_rowid())
}

/// 更新已有放置的元数据（增量重扫命中：mtime/size 变了但路径同）。
pub fn placement_refresh(
    conn: &Connection,
    id: i64,
    content_id: &str,
    size: i64,
    mtime: i64,
    inode: Option<i64>,
) -> Result<()> {
    let old: String = conn.query_row(
        "SELECT content_id FROM placement WHERE id=?1",
        params![id],
        |r| r.get(0),
    )?;
    if old != content_id {
        conn.execute(
            "UPDATE content SET copy_count = copy_count - 1 WHERE blake3=?1",
            params![old],
        )?;
        conn.execute(
            "UPDATE content SET copy_count = copy_count + 1 WHERE blake3=?1",
            params![content_id],
        )?;
    }
    conn.execute(
        "UPDATE placement SET content_id=?2, size=?3, mtime=?4, inode=?5, state='online'
         WHERE id=?1",
        params![id, content_id, size, mtime, inode],
    )?;
    Ok(())
}

/// 删除放置并同事务 copy_count-1；返回删除前 content_id。
pub fn placement_delete(conn: &Connection, id: i64) -> Result<Option<String>> {
    let content: Option<String> = optional(conn.query_row(
        "SELECT content_id FROM placement WHERE id=?1",
        params![id],
        |r| r.get(0),
    ))?;
    if let Some(cid) = &content {
        conn.execute("DELETE FROM placement WHERE id=?1", params![id])?;
        conn.execute(
            "UPDATE content SET copy_count = copy_count - 1 WHERE blake3=?1",
            params![cid],
        )?;
    }
    Ok(content)
}

pub fn placement_get(
    conn: &Connection,
    id: i64,
) -> Result<
    Option<(
        i64,
        String,
        String,
        String,
        i64,
        i64,
        Option<i64>,
        String,
        i64,
        Option<i64>,
        i64,
        String,
        String,
    )>,
> {
    // id, location_id, rel_path, content_id, size, mtime, inode, state, first_seen,
    // last_verified, pinned, hash_state, mode
    optional(conn.query_row(
        "SELECT id, location_id, rel_path, content_id, size, mtime, inode, state,
                first_seen, last_verified, pinned, hash_state, mode
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
                r.get(6)?,
                r.get(7)?,
                r.get(8)?,
                r.get(9)?,
                r.get(10)?,
                r.get(11)?,
                r.get(12)?,
            ))
        },
    ))
}

/// 某内容的全部放置：(id, location_id, rel_path, state, pinned, inode, last_verified)。
pub fn placements_of_content(
    conn: &Connection,
    content_id: &str,
) -> Result<Vec<(i64, String, String, String, bool, Option<i64>, Option<i64>)>> {
    let mut st = conn.prepare(
        "SELECT id, location_id, rel_path, state, pinned, inode, last_verified
         FROM placement WHERE content_id=?1 ORDER BY id",
    )?;
    let rows = st
        .query_map(params![content_id], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get::<_, i64>(4)? != 0,
                r.get(5)?,
                r.get(6)?,
            ))
        })?
        .collect::<Result<Vec<_>>>()?;
    Ok(rows)
}

/// 可对外提供数据源的放置：本机设备 + placement/location 双 `online` + 内容未压缩。
///
/// `cold`/`offline`/`missing` 一律排除（`docs/03` §5.2 冷限制；`docs/01` §7.6）。
/// 返回 `(placement_id, 绝对路径, size)`，按 id 升序。
pub fn serving_paths_for_content(
    conn: &Connection,
    device_id: &str,
    content_id: &str,
) -> Result<Vec<(i64, String, i64)>> {
    let mut st = conn.prepare(
        "SELECT p.id, l.root_path || '/' || p.rel_path, p.size
         FROM placement p
         JOIN location l ON l.id = p.location_id
         JOIN content c ON c.blake3 = p.content_id
         WHERE p.content_id = ?1
           AND l.device_id = ?2
           AND p.state = 'online'
           AND l.state = 'online'
           AND c.enc = 'plain'
         ORDER BY p.id",
    )?;
    let rows = st
        .query_map(params![content_id, device_id], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })?
        .collect::<Result<Vec<_>>>()?;
    Ok(rows)
}

/// 硬链接合并后，把被合并放置指向保留放置的 inode（保留行不动）。
pub fn placement_merge_inode(
    conn: &Connection,
    merged_ids: &[i64],
    keep_inode: i64,
    keep_content: &str,
) -> Result<usize> {
    let mut n = 0;
    for id in merged_ids {
        let old: String = conn.query_row(
            "SELECT content_id FROM placement WHERE id=?1",
            params![id],
            |r| r.get(0),
        )?;
        conn.execute(
            "UPDATE placement SET inode=?2, content_id=?3 WHERE id=?1",
            params![id, keep_inode, keep_content],
        )?;
        if old != keep_content {
            conn.execute(
                "UPDATE content SET copy_count = copy_count - 1 WHERE blake3=?1",
                params![old],
            )?;
            conn.execute(
                "UPDATE content SET copy_count = copy_count + 1 WHERE blake3=?1",
                params![keep_content],
            )?;
        }
        n += 1;
    }
    Ok(n)
}

pub fn placement_set_last_verified(conn: &Connection, id: i64, ts: i64) -> Result<()> {
    conn.execute(
        "UPDATE placement SET last_verified=?2 WHERE id=?1",
        params![id, ts],
    )?;
    Ok(())
}

/// 浏览查询（`docs/03` browse.query）：返回 (rows, total)。
pub fn browse(
    conn: &Connection,
    location_id: Option<&str>,
    path_prefix: Option<&str>,
    limit: u32,
    offset: u32,
) -> Result<(
    Vec<(i64, String, String, String, i64, String, String, String)>,
    u64,
)> {
    let mut where_parts: Vec<String> = vec!["1=1".into()];
    let mut binds: Vec<String> = Vec::new();
    if let Some(l) = location_id {
        where_parts.push(format!("p.location_id = ?{}", binds.len() + 1));
        binds.push(l.to_string());
    }
    if let Some(prefix) = path_prefix {
        where_parts.push(format!("p.rel_path LIKE ?{} || '%'", binds.len() + 1));
        binds.push(prefix.to_string());
    }
    let w = where_parts.join(" AND ");
    let total: u64 = conn.query_row(
        &format!("SELECT COUNT(*) FROM placement p WHERE {w}"),
        rusqlite::params_from_iter(binds.iter()),
        |r| r.get(0),
    )?;
    let mut st = conn.prepare(&format!(
        "SELECT p.id, p.rel_path, p.location_id, p.content_id, p.size, c.asset_type,
                p.state, p.mode
         FROM placement p JOIN content c ON c.blake3 = p.content_id
         WHERE {w} ORDER BY p.rel_path LIMIT ?{} OFFSET ?{}",
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
                r.get(6)?,
                r.get(7)?,
            ))
        })?
        .collect::<Result<Vec<_>>>()?;
    Ok((rows, total))
}

/// 扫描统计：文件数、总字节、内容条数。
pub fn scan_totals(conn: &Connection) -> Result<(u64, u64, u64)> {
    conn.query_row(
        "SELECT COUNT(*), COALESCE(SUM(size),0),
                (SELECT COUNT(*) FROM content)
         FROM placement",
        [],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )
}

/// 重复组数与可回收字节（copy_count>1 的内容）。
pub fn dup_totals(conn: &Connection) -> Result<(u64, i64)> {
    conn.query_row(
        "SELECT COUNT(*), COALESCE(SUM(size * (copy_count - 1)), 0)
         FROM content WHERE copy_count > 1",
        [],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )
}
