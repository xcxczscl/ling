//! 隐私三档（`docs/01` §7.8.4、`docs/00` §4.6）。
//!
//! `local` 本机模型，数据不出机器；`cloud` 只上传文件名与已提取参数；
//! `hybrid`（默认）分类/关系本地、资料对齐云端。
//!
//! 档位规则按**路径前缀**存 `ai_privacy` 表；解析在 `rm_catalog::repo::suggestion::privacy_resolve`。

pub const LOCAL: &str = "local";
pub const CLOUD: &str = "cloud";
pub const HYBRID: &str = "hybrid";

pub fn tier_known(t: &str) -> bool {
    matches!(t, LOCAL | CLOUD | HYBRID)
}

/// 该档位下是否允许"出网做资料对齐/云端推理"。
pub fn allows_cloud(tier: &str) -> bool {
    matches!(tier, CLOUD | HYBRID)
}

/// 该档位下是否只允许本机。
pub fn is_local_only(tier: &str) -> bool {
    tier == LOCAL
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tiers() {
        assert!(tier_known("local"));
        assert!(tier_known("cloud"));
        assert!(tier_known("hybrid"));
        assert!(!tier_known("secret"));
        assert!(allows_cloud("hybrid"));
        assert!(allows_cloud("cloud"));
        assert!(!allows_cloud("local"));
        assert!(is_local_only("local"));
    }
}
