//! `rm-ai`：AI 编排（`docs/01` §7.8、`docs/03` §5.4）。
//!
//! 边界：AI 只是**建议者**——所有产出进 `ai_suggestion` 表，绝不直接写业务表；
//! 分级确认（低风险可自动采纳、高风险必须人工）；隐私三档；预算上限。
//!
//! 确定性提取（§5.4.3 第 1–5 步）与语义判断（第 6 步）分层：
//! 扩展名/文件名模式是纯代码（可复现），只有剩下的才交给 Provider。

pub mod cache;
pub mod codes;
pub mod cost;
pub mod extract;
pub mod privacy;
pub mod provider;
pub mod suggest;

pub use rm_proto::codes as proto_codes;

/// 生成短建议 id：`sg_` 前缀 + 时间片（与 rm-semantic gen_id 同风格）。
pub fn gen_sg_id() -> String {
    let ts = rm_catalog::now() as u64;
    let n = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    format!("sg_{ts:x}{n:08x}")
}

/// blake3 十六进制前缀（提示词/输入指纹）。
pub fn hash_hex(s: &str) -> String {
    let h = blake3::hash(s.as_bytes());
    let mut out = String::with_capacity(16);
    for b in h.as_bytes() {
        out.push_str(&format!("{b:02x}"));
    }
    out.truncate(16);
    out
}
