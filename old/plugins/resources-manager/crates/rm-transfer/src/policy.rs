//! 按需策略守卫（`docs/01` §7.6 四档；`docs/02` §4.5 求值）。
//!
//! 传输面只用到"能否作为数据源"与"能否拉回本地"两个判定。

use crate::error::TransferError;

/// 四档：`local_only` / `mirror` / `evictable` / `record_only`。
pub const MODES: [&str; 4] = ["local_only", "mirror", "evictable", "record_only"];

/// 归一化策略档；非法 → None。
pub fn parse_mode(mode: &str) -> Option<&'static str> {
    MODES.into_iter().find(|m| *m == mode)
}

/// 内容是否可作为局域网数据源（服务端 `/blob` 供数判定）。
///
/// `local_only`/`mirror`/`evictable` 在线副本可提供；`record_only` 与 `cold` 一律拒绝（F403）。
pub fn allows_serving(mode: &str) -> bool {
    match mode {
        "local_only" | "mirror" | "evictable" => true,
        _ => false,
    }
}

/// 内容是否可被拉回本地（客户端 `fetch` 守卫）。
///
/// `local_only`（只留本机）与 `record_only`（只记不存）均拒绝拉回；
/// 仅 `mirror` / `evictable` 放行（`docs/01` §7.6、`docs/03` §2.3）。
pub fn allows_fetch(mode: &str) -> bool {
    match mode {
        "mirror" | "evictable" => true,
        _ => false,
    }
}

/// fetch 前守卫：策略不允许拉回 → F403。
pub fn guard_fetch(mode: &str) -> Result<(), TransferError> {
    if allows_fetch(mode) {
        Ok(())
    } else {
        Err(TransferError::forbidden(format!(
            "按需策略 {mode} 禁止从对端拉回"
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mode_matrix() {
        // 可供数
        assert!(allows_serving("local_only"));
        assert!(allows_serving("mirror"));
        assert!(allows_serving("evictable"));
        assert!(!allows_serving("record_only"));
        assert!(!allows_serving("cold"));
        // 可拉回
        assert!(!allows_fetch("local_only"));
        assert!(allows_fetch("mirror"));
        assert!(allows_fetch("evictable"));
        assert!(!allows_fetch("record_only"));
    }

    #[test]
    fn guard_blocks_local_only() {
        assert!(guard_fetch("mirror").is_ok());
        let e = guard_fetch("local_only").unwrap_err();
        assert_eq!(e.code, "F403");
    }

    #[test]
    fn parse_mode_accepts_all_four() {
        for m in MODES {
            assert_eq!(parse_mode(m), Some(m));
        }
        assert_eq!(parse_mode("cold"), None);
        assert_eq!(parse_mode(""), None);
    }
}
