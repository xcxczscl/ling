//! 仓储层：按表分文件的增删改查。

pub mod conflict;
pub mod connection;
pub mod content;
pub mod entry;
pub mod job;
pub mod op;
pub mod peer;
pub mod placement;
pub mod policy;
pub mod pool;
pub mod rule;
pub mod snapshot;
pub mod suggestion;
pub mod tag;
pub mod transfer;
pub mod version;

use rusqlite::{Connection, Result};

/// 在事务中执行 `f` 并提交。
pub fn txn<T>(conn: &mut Connection, f: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
    let tx = conn.transaction()?;
    let out = f(&tx)?;
    tx.commit()?;
    Ok(out)
}

/// `query_row` 的 NotFound → `Ok(None)` 助手。
pub fn optional<T>(r: Result<T>) -> Result<Option<T>> {
    match r {
        Ok(v) => Ok(Some(v)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e),
    }
}
