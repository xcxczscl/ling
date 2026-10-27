//! `rm-ai` 本地错误码（F7xx 族，`docs/03` §2.7）。

use rm_proto::BizError;

pub fn err(code: &str, msg: impl Into<String>) -> BizError {
    BizError::new(code, msg.into())
}

pub const F701: &str = "F701"; // 建议不存在
pub const F702: &str = "F702"; // 建议状态不允许该操作
pub const F703: &str = "F703"; // AI 预算耗尽
pub const F704: &str = "F704"; // 提供方不可用/调用失败
pub const F705: &str = "F705"; // 隐私档位禁止该出网行为
pub const F706: &str = "F706"; // 目标对象不存在

pub fn not_found(id: &str) -> BizError {
    err(F701, format!("建议不存在：{id}"))
}

pub fn bad_state(id: &str, state: &str) -> BizError {
    err(F702, format!("建议 {id} 当前状态 {state}，不允许该操作"))
}

pub fn budget_exhausted() -> BizError {
    err(F703, "AI 预算耗尽（模型调用次数已达上限）")
}

pub fn provider_unavailable(detail: impl std::fmt::Display) -> BizError {
    err(F704, format!("模型提供方不可用：{detail}"))
}

pub fn privacy_blocked(tier: &str, action: &str) -> BizError {
    err(F705, format!("隐私档位 {tier} 禁止操作：{action}"))
}

pub fn target_missing(kind: &str, id: &str) -> BizError {
    err(F706, format!("{kind} 不存在：{id}"))
}
