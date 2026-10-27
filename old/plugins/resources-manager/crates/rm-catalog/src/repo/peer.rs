//! `peer` 对端表（`docs/02` §4.2）。

use rusqlite::{params, Connection, Result};

use super::optional;

/// 登记/更新对端（配对时固定指纹与公钥；重复配对同设备更新 addr）。
pub fn upsert(
    conn: &Connection,
    device_id: &str,
    addr: Option<&str>,
    fingerprint: &str,
    pubkey_hex: Option<&str>,
    now: i64,
) -> Result<()> {
    conn.execute(
        "INSERT INTO peer(device_id, addr, fingerprint, pubkey, known_seq, last_sync)
         VALUES(?1,?2,?3,?4,0,0)
         ON CONFLICT(device_id) DO UPDATE SET
           addr=COALESCE(excluded.addr, peer.addr),
           fingerprint=excluded.fingerprint,
           pubkey=COALESCE(excluded.pubkey, peer.pubkey)",
        params![device_id, addr, fingerprint, pubkey_hex],
    )?;
    let _ = now;
    Ok(())
}

/// 读对端公钥（hex）；未配对或缺公钥返回 None。
pub fn pubkey(conn: &Connection, device_id: &str) -> Result<Option<String>> {
    optional(conn.query_row(
        "SELECT COALESCE(pubkey, '') FROM peer WHERE device_id=?1",
        params![device_id],
        |r| r.get::<_, String>(0),
    ))
    .map(|v| v.filter(|s| !s.is_empty()))
}

pub fn get(
    conn: &Connection,
    device_id: &str,
) -> Result<Option<(String, Option<String>, String, i64, i64)>> {
    optional(conn.query_row(
        "SELECT device_id, addr, fingerprint, known_seq, last_sync FROM peer WHERE device_id=?1",
        params![device_id],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
    ))
}

pub fn list(conn: &Connection) -> Result<Vec<(String, Option<String>, String, i64, i64)>> {
    let mut st = conn.prepare(
        "SELECT device_id, addr, fingerprint, known_seq, last_sync FROM peer ORDER BY device_id",
    )?;
    let rows = st
        .query_map([], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
        })?
        .collect::<Result<Vec<_>>>()?;
    Ok(rows)
}

/// 更新同步进度（known_seq / last_sync）。
pub fn set_progress(
    conn: &Connection,
    device_id: &str,
    known_seq: i64,
    last_sync: i64,
) -> Result<()> {
    conn.execute(
        "UPDATE peer SET known_seq=?2, last_sync=?3 WHERE device_id=?1",
        params![device_id, known_seq, last_sync],
    )?;
    Ok(())
}

pub fn forget(conn: &Connection, device_id: &str) -> Result<usize> {
    conn.execute("DELETE FROM peer WHERE device_id=?1", params![device_id])
}
