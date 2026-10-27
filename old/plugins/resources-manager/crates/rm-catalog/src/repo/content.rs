//! `content` 表：内容不可变；`copy_count` 物化列与 placement 增删同事务维护。

use rusqlite::{params, Connection, Result};

use super::optional;

/// 插入内容（已存在则跳过——指纹相同即同一条）。返回是否新插入。
pub fn insert_if_absent(
    conn: &Connection,
    blake3: &str,
    size: i64,
    asset_type: &str,
    created: i64,
) -> Result<bool> {
    let n = conn.execute(
        "INSERT OR IGNORE INTO content(blake3, asset_type, size, created) VALUES(?1,?2,?3,?4)",
        params![blake3, asset_type, size, created],
    )?;
    Ok(n > 0)
}

/// 返回 (size, asset_type, copy_count)。
pub fn get(conn: &Connection, blake3: &str) -> Result<Option<(i64, String, i64)>> {
    optional(conn.query_row(
        "SELECT size, asset_type, copy_count FROM content WHERE blake3=?1",
        params![blake3],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    ))
}

pub fn set_verified_level(conn: &Connection, blake3: &str, level: &str) -> Result<()> {
    conn.execute(
        "UPDATE content SET verified_level=?2 WHERE blake3=?1",
        params![blake3, level],
    )?;
    Ok(())
}

/// copy_count 全量重算（doctor 校正漂移，`docs/02` §8.3）。返回修正行数。
pub fn recompute_copy_count(conn: &Connection) -> Result<usize> {
    conn.execute(
        "UPDATE content SET copy_count = (
           SELECT COUNT(*) FROM placement WHERE placement.content_id = content.blake3
         )",
        [],
    )
}
