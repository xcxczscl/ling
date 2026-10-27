//! 建议领域逻辑：分级确认、风险分级、阈值、状态机（`docs/01` §7.8.2–7.8.3）。
//!
//! - 低风险（tag/metadata）：置信度 ≥ 阈值（默认 0.85）可**自动采纳**；
//! - 高风险（connection/entry_create/entry_match/version_split）：一律 pending，必须人工；
//! - 禁止：删除/去重/备份决策永远不经此处（安全规则纯代码，`docs/01` §7.8.3）。

use rm_proto::BizError;

use crate::codes;

/// 建议种类 → 风险级别。
pub fn risk_of(kind: &str) -> Risk {
    match kind {
        "tag" | "metadata" => Risk::Low,
        "connection" | "entry_create" | "entry_match" | "version_split" => Risk::High,
        _ => Risk::High, // 未知种类按高风险处理（fail-safe）
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Risk {
    Low,
    High,
}

/// 自动采纳阈值（`docs/03` §5.4.2：默认 0.85）。
pub const AUTO_ACCEPT_THRESHOLD: f64 = 0.85;

/// 判断一条建议是否可自动采纳：低风险 且 confidence ≥ 阈值。
pub fn auto_acceptable(kind: &str, confidence: f64) -> bool {
    risk_of(kind) == Risk::Low && confidence >= AUTO_ACCEPT_THRESHOLD
}

/// apply 时校验状态：只能 apply pending。
pub fn ensure_pending(state: &str, id: &str) -> Result<(), BizError> {
    if state == "pending" {
        Ok(())
    } else {
        Err(codes::bad_state(id, state))
    }
}

/// 建议种类白名单（`docs/02` §5.1）。
pub const KINDS: [&str; 6] = [
    "tag",
    "connection",
    "entry_match",
    "entry_create",
    "version_split",
    "metadata",
];

pub fn kind_known(k: &str) -> bool {
    KINDS.contains(&k)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn risk_levels() {
        assert_eq!(risk_of("tag"), Risk::Low);
        assert_eq!(risk_of("metadata"), Risk::Low);
        assert_eq!(risk_of("connection"), Risk::High);
        assert_eq!(risk_of("entry_create"), Risk::High);
        assert_eq!(risk_of("mystery"), Risk::High); // fail-safe
    }

    #[test]
    fn auto_accept_threshold() {
        assert!(auto_acceptable("tag", 0.90));
        assert!(!auto_acceptable("tag", 0.80)); // 低于阈值
        assert!(!auto_acceptable("connection", 0.99)); // 高风险永不自动
        assert!(!auto_acceptable("entry_create", 1.0));
    }
}
