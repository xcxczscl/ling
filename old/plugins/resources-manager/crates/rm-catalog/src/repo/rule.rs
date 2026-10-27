//! `rule` 表：分类规则（`docs/02` §6.1）。

use rusqlite::{params, Connection, Result};

pub fn replace_all(conn: &Connection, rules: &[(String, i64, String, String, bool)]) -> Result<()> {
    conn.execute("DELETE FROM rule", [])?;
    for (id, priority, match_json, action_json, enabled) in rules {
        conn.execute(
            "INSERT INTO rule(id, priority, match_json, action_json, enabled)
             VALUES(?1,?2,?3,?4,?5)",
            params![id, priority, match_json, action_json, *enabled as i64],
        )?;
    }
    Ok(())
}

pub fn list(conn: &Connection) -> Result<Vec<(String, i64, String, String, bool)>> {
    let mut st = conn.prepare(
        "SELECT id, priority, match_json, action_json, enabled FROM rule ORDER BY priority, id",
    )?;
    let rows = st
        .query_map([], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get::<_, i64>(4)? != 0,
            ))
        })?
        .collect::<Result<Vec<_>>>()?;
    Ok(rows)
}
