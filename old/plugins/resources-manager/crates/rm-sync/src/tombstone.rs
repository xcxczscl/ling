//! 墓碑宽限期（`docs/01` §7.5：30 天）。

use rm_proto::codes;

fn err(msg: impl Into<String>) -> rm_proto::BizError {
    rm_proto::BizError::new(codes::F401, msg.into())
}

/// 宽限期天数（封档值 30）。
pub const TOMBSTONE_GRACE_DAYS: i64 = 30;

/// 墓碑 ts 是否仍在宽限期内（相对 now）。
pub fn is_grace_active(tombstone_ts: i64, now: i64) -> bool {
    now.saturating_sub(tombstone_ts) < TOMBSTONE_GRACE_DAYS * 86_400
}

/// 物理清除过期墓碑（`state=deleted` 且 first_seen 超过宽限）。
/// 返回清除条数。调用方负责事务边界。
pub fn purge_expired_tombstones(
    conn: &rusqlite::Connection,
    now: i64,
) -> Result<u64, rm_proto::BizError> {
    let cutoff = now - TOMBSTONE_GRACE_DAYS * 86_400;
    // placement 墓碑：state='deleted' 且 first_seen（用作墓碑时间）早于 cutoff。
    let n1 = conn
        .execute(
            "DELETE FROM placement WHERE state='deleted' AND first_seen < ?1",
            rusqlite::params![cutoff],
        )
        .map_err(|e| err(e.to_string()))?;
    // connection 墓碑。
    let n2 = conn
        .execute(
            "DELETE FROM connection WHERE deleted_at IS NOT NULL AND deleted_at < ?1",
            rusqlite::params![cutoff],
        )
        .map_err(|e| err(e.to_string()))?;
    Ok((n1 + n2) as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grace_window_is_30_days() {
        let now = 1_000_000_000;
        assert!(is_grace_active(now - 10 * 86_400, now));
        assert!(is_grace_active(now - 29 * 86_400, now));
        assert!(!is_grace_active(now - 31 * 86_400, now));
        assert!(!is_grace_active(now - 30 * 86_400, now));
    }
}
