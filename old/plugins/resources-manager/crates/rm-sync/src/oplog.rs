//! 本地变更写入操作流水（同步唯一载体，`docs/02` §4.1）。

use rm_proto::codes;

fn err(msg: impl Into<String>) -> rm_proto::BizError {
    rm_proto::BizError::new(codes::F401, msg.into())
}

/// 生成全局唯一 op_id（幂等键）。前缀 `op_` + 时间 + 随机。
pub fn gen_op_id() -> String {
    let ts = rm_catalog::now() as u64;
    let n = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    format!("op_{ts:x}{n:08x}")
}

/// 追加一条本地 op。payload 已序列化。
pub fn append_local(
    conn: &rusqlite::Connection,
    device_id: &str,
    kind: &str,
    payload: serde_json::Value,
) -> Result<i64, rm_proto::BizError> {
    let op_id = gen_op_id();
    let ts = rm_catalog::now();
    rm_catalog::repo::op::append(conn, device_id, &op_id, kind, &payload.to_string(), ts)
        .map_err(|e| err(e.to_string()))
}
