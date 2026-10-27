//! `placement_policy` 按需策略表（`docs/02` §4.5；四档语义见 `docs/01` §7.6）。

use rusqlite::{params, Connection, Result};

use super::optional;

/// 合法策略档；未识别的值在 `parse_mode` 返回 None。
pub const MODES: [&str; 4] = ["local_only", "mirror", "evictable", "record_only"];

/// 校验并归一化策略档；非法返回 None。
pub fn parse_mode(mode: &str) -> Option<&'static str> {
    MODES.into_iter().find(|m| *m == mode)
}

/// 校验作用域种类。
pub fn parse_scope_kind(kind: &str) -> Option<&'static str> {
    ["path_prefix", "tag"].into_iter().find(|k| *k == kind)
}

/// 新策略编号。
pub fn new_id(now: i64) -> String {
    format!("pl_{now:x}")
}

/// 写入/覆盖一个作用域的策略（唯一约束 scope_kind+scope_id）。返回 id。
pub fn set(
    conn: &Connection,
    scope_kind: &str,
    scope_id: &str,
    mode: &str,
    priority: i64,
    now: i64,
) -> Result<String> {
    if let Some(id) = optional(conn.query_row(
        "SELECT id FROM placement_policy WHERE scope_kind=?1 AND scope_id=?2",
        params![scope_kind, scope_id],
        |r| r.get::<_, String>(0),
    ))? {
        conn.execute(
            "UPDATE placement_policy SET mode=?2, priority=?3 WHERE id=?1",
            params![id, mode, priority],
        )?;
        return Ok(id);
    }
    let id = new_id(now);
    conn.execute(
        "INSERT INTO placement_policy(id, scope_kind, scope_id, mode, priority, created)
         VALUES(?1,?2,?3,?4,?5,?6)",
        params![id, scope_kind, scope_id, mode, priority, now],
    )?;
    Ok(id)
}

pub fn del(conn: &Connection, id: &str) -> Result<usize> {
    conn.execute("DELETE FROM placement_policy WHERE id=?1", params![id])
}

/// 全部策略：(id, scope_kind, scope_id, mode, priority)。
pub fn list(conn: &Connection) -> Result<Vec<(String, String, String, String, i64)>> {
    let mut st = conn.prepare(
        "SELECT id, scope_kind, scope_id, mode, priority FROM placement_policy ORDER BY priority DESC, scope_kind, scope_id",
    )?;
    let rows = st
        .query_map([], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
            ))
        })?
        .collect::<Result<Vec<_>>>()?;
    Ok(rows)
}

/// 对某条放置求值策略档（`docs/02` §4.5 求值顺序）。
///
/// - `path_prefix`：`placement` 绝对路径以 `scope_id` 开头即匹配；
/// - `tag`：该内容经 `version_file → version_tag` 能找到 `scope_id` 标签即匹配；
/// - 候选按 `priority` 大者胜；同分时 `tag` 先于 `path_prefix`，再按 `scope_id` 字典序；
/// - 无匹配 → 默认 `local_only`。
pub fn eval_for_placement(
    conn: &Connection,
    placement_id: i64,
    abs_path: &str,
    content_blake3: &str,
) -> Result<String> {
    let mut best: Option<(i64, u8, String, String)> = None; // (prio, kind_rank, scope_id, mode)

    let mut st = conn.prepare(
        "SELECT scope_id, mode, priority FROM placement_policy WHERE scope_kind='path_prefix'",
    )?;
    let prefixes = st
        .query_map([], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, i64>(2)?))
        })?
        .collect::<Result<Vec<_>>>()?;
    for (scope_id, mode, prio) in prefixes {
        if abs_path.starts_with(&scope_id) {
            consider(&mut best, prio, 1, scope_id, mode);
        }
    }

    let mut st = conn.prepare(
        "SELECT pp.scope_id, pp.mode, pp.priority
         FROM placement_policy pp
         JOIN version_tag vt ON vt.tag_id = pp.scope_id
         JOIN version_file vf ON vf.version_id = vt.version_id
         WHERE pp.scope_kind='tag' AND vf.content_blake3 = ?1",
    )?;
    let tags = st
        .query_map(params![content_blake3], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, i64>(2)?))
        })?
        .collect::<Result<Vec<_>>>()?;
    for (scope_id, mode, prio) in tags {
        consider(&mut best, prio, 0, scope_id, mode);
    }

    let _ = placement_id; // 预留：未来按放置级 pin 覆盖
    Ok(best.map(|(_, _, _, m)| m).unwrap_or_else(|| "local_only".to_string()))
}

/// 候选比较：`priority` 大者胜；同分时 `tag`（kind_rank=0）先于 `path_prefix`（1）；
/// 再同分按 `scope_id` 字典序小者胜。
fn consider(
    best: &mut Option<(i64, u8, String, String)>,
    prio: i64,
    kind_rank: u8,
    scope_id: String,
    mode: String,
) {
    let wins = match best {
        None => true,
        Some((bp, br, bs, _)) => match prio.cmp(bp) {
            std::cmp::Ordering::Greater => true,
            std::cmp::Ordering::Less => false,
            std::cmp::Ordering::Equal => match kind_rank.cmp(br) {
                std::cmp::Ordering::Less => true,
                std::cmp::Ordering::Greater => false,
                std::cmp::Ordering::Equal => scope_id < *bs,
            },
        },
    };
    if wins {
        *best = Some((prio, kind_rank, scope_id, mode));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mode_parse() {
        assert_eq!(parse_mode("mirror"), Some("mirror"));
        assert_eq!(parse_mode("nope"), None);
        assert_eq!(parse_scope_kind("tag"), Some("tag"));
        assert_eq!(parse_scope_kind("x"), None);
    }
}
