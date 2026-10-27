//! `job` 表：插件自管作业（内核无作业服务，`docs/03` §2.3）。

use rusqlite::{params, Connection, Result};

pub fn create(conn: &Connection, id: &str, kind: &str, now: i64) -> Result<()> {
    conn.execute(
        "INSERT INTO job(id, kind, state, progress_json, log, created, updated)
         VALUES(?1,?2,'running','{}','',?3,?3)",
        params![id, kind, now],
    )?;
    Ok(())
}

pub fn set_progress(conn: &Connection, id: &str, progress_json: &str, now: i64) -> Result<()> {
    conn.execute(
        "UPDATE job SET progress_json=?2, updated=?3 WHERE id=?1",
        params![id, progress_json, now],
    )?;
    Ok(())
}

pub fn append_log(conn: &Connection, id: &str, line: &str, now: i64) -> Result<()> {
    conn.execute(
        "UPDATE job SET log = log || ?2 || char(10), updated=?3 WHERE id=?1",
        params![id, line, now],
    )?;
    Ok(())
}

pub fn finish(conn: &Connection, id: &str, state: &str, now: i64) -> Result<()> {
    conn.execute(
        "UPDATE job SET state=?2, updated=?3 WHERE id=?1",
        params![id, state, now],
    )?;
    Ok(())
}

pub fn get(
    conn: &Connection,
    id: &str,
) -> Result<Option<(String, String, String, String, i64, i64)>> {
    super::optional(conn.query_row(
        "SELECT id, kind, state, progress_json, created, updated FROM job WHERE id=?1",
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

pub fn log_of(conn: &Connection, id: &str) -> Result<String> {
    super::optional(conn.query_row("SELECT log FROM job WHERE id=?1", params![id], |r| r.get(0)))
        .map(|o| o.unwrap_or_default())
}

pub fn list(
    conn: &Connection,
    kind: Option<&str>,
    limit: u32,
) -> Result<Vec<(String, String, String, String, i64, i64)>> {
    let mut out;
    match kind {
        Some(k) => {
            let mut st = conn.prepare(
                "SELECT id, kind, state, progress_json, created, updated FROM job
                 WHERE kind=?1 ORDER BY created DESC LIMIT ?2",
            )?;
            out = st
                .query_map(params![k, limit], |r| {
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
        }
        None => {
            let mut st = conn.prepare(
                "SELECT id, kind, state, progress_json, created, updated FROM job
                 ORDER BY created DESC LIMIT ?1",
            )?;
            out = st
                .query_map(params![limit], |r| {
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
        }
    }
    out.shrink_to_fit();
    Ok(out)
}
