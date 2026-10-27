//! 记录同步 LAN 客户端（`docs/03` §5.1：`sync.run` 改走 `/op/*`）。
//!
//! 只做传输：导出/导入 op 流水由调用方（rm-sync）在本地库上完成，
//! 这里负责把 BundleOp 数组安全地送到对端（或从对端取回）。

use std::sync::Arc;

use crate::error::TransferError;
use crate::http;
use crate::identity::Identity;
use crate::protocol::{now_ts, op_sign_payload};
use crate::server::sign_headers;

/// 一次导入结果（与 `rm_sync::merge::MergeReport` 字段对齐）。
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct ImportReport {
    pub applied: u64,
    pub skipped: u64,
    pub conflicts: u64,
}

/// 拉取对端流水：`GET /op/export?after=&limit=`，返回 op 数组。
pub fn pull_ops(
    addr: &str,
    identity: &Arc<Identity>,
    device_id: &str,
    after: i64,
    limit: u32,
) -> Result<Vec<serde_json::Value>, TransferError> {
    let ts = now_ts();
    let payload = op_sign_payload("export", b"", ts);
    let headers = sign_headers(identity, device_id, &payload);
    let path = format!("/op/export?after={after}&limit={limit}");
    let (status, body) = http::http_bytes(addr, "GET", &path, &headers, None)?;
    if status != 200 {
        return Err(http_error(status, &body, "拉取对端流水失败"));
    }
    let v: serde_json::Value = serde_json::from_slice(&body)
        .map_err(|_| TransferError::proto("op 导出响应不是合法 JSON"))?;
    let ops = v["ops"]
        .as_array()
        .cloned()
        .ok_or_else(|| TransferError::proto("op 导出响应缺 ops 数组"))?;
    Ok(ops)
}

/// 推送本地流水：`POST /op/import`，返回应用报告。
pub fn push_ops(
    addr: &str,
    identity: &Arc<Identity>,
    device_id: &str,
    ops: &[serde_json::Value],
) -> Result<ImportReport, TransferError> {
    let body = serde_json::json!({ "ops": ops }).to_string();
    let ts = now_ts();
    let payload = op_sign_payload("import", body.as_bytes(), ts);
    let headers = sign_headers(identity, device_id, &payload);
    let (status, resp) = http::http_bytes(addr, "POST", "/op/import", &headers, Some(body.as_bytes()))?;
    if status != 200 {
        return Err(http_error(status, &resp, "推送本地流水失败"));
    }
    let v: serde_json::Value = serde_json::from_slice(&resp)
        .map_err(|_| TransferError::proto("op 导入响应不是合法 JSON"))?;
    Ok(ImportReport {
        applied: v["applied"].as_u64().unwrap_or(0),
        skipped: v["skipped"].as_u64().unwrap_or(0),
        conflicts: v["conflicts"].as_u64().unwrap_or(0),
    })
}

fn http_error(status: u16, body: &[u8], fallback: &str) -> TransferError {
    let msg = serde_json::from_slice::<serde_json::Value>(body)
        .ok()
        .and_then(|v| v["error"]["message"].as_str().map(str::to_string))
        .unwrap_or_else(|| format!("{fallback}：HTTP {status}"));
    TransferError::new(rm_proto::codes::F404, msg)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::Identity;

    #[test]
    fn import_report_defaults_zero() {
        let r = ImportReport::default();
        assert_eq!(r.applied, 0);
        assert_eq!(r.skipped, 0);
        assert_eq!(r.conflicts, 0);
    }

    #[test]
    fn pull_to_unreachable_addr_is_f404() {
        let id = Arc::new(Identity::from_seed([7u8; 32]));
        // 保留地址端口未监听 → 连接失败 → F404。
        let err = pull_ops("127.0.0.1:1", &id, "dev-a", 0, 100).unwrap_err();
        assert_eq!(err.code, "F404");
    }
}
