//! 配对客户端（`docs/03` §5.2 握手发起端）。

use std::sync::Arc;

use crate::error::TransferError;
use crate::http;
use crate::identity::{fingerprint_of, verify, Identity};
use crate::protocol::{
    from_hex, now_ts, pair_client_payload, pair_server_payload, to_hex, ts_ok,
};

/// 对端身份信息（握手结果）。
pub struct PairedPeer {
    pub device_id: String,
    pub name: String,
    pub platform: String,
    pub fingerprint: String,
    pub pubkey_hex: String,
}

/// 读对端 `/hello`（配对前核对身份，可选步骤）。
pub fn hello(addr: &str) -> Result<(String, String), TransferError> {
    let (status, body) = http::http_bytes(addr, "GET", "/hello", &[], None)?;
    if status != 200 {
        return Err(TransferError::unreachable(format!(
            "对端 /hello 返回 HTTP {status}"
        )));
    }
    let v: serde_json::Value = serde_json::from_slice(&body)
        .map_err(|_| TransferError::proto("/hello 响应不是合法 JSON"))?;
    let device_id = v["device_id"]
        .as_str()
        .ok_or_else(|| TransferError::proto("/hello 缺 device_id"))?
        .to_string();
    let fingerprint = v["fingerprint"]
        .as_str()
        .ok_or_else(|| TransferError::proto("/hello 缺 fingerprint"))?
        .to_string();
    Ok((device_id, fingerprint))
}

/// 发起配对：向对端 `POST /pair`，校验响应签名，返回对端身份。
///
/// 已 pin 的指纹由调用方核对（传入 `pinned: Option<&str>`，不一致 → F402）。
pub fn pair(
    addr: &str,
    code: &str,
    identity: &Identity,
    local_device_id: &str,
    local_name: &str,
    local_platform: &str,
    pinned: Option<&str>,
) -> Result<PairedPeer, TransferError> {
    if code.len() != 8 || !code.bytes().all(|b| b.is_ascii_digit()) {
        return Err(TransferError::bad_code("配对码必须是 8 位数字"));
    }
    let ts = now_ts();
    let pubkey_hex = to_hex(&identity.public_key());
    let payload = pair_client_payload(code, local_device_id, &pubkey_hex, ts);
    let sig = to_hex(&identity.sign(payload.as_bytes()));
    let body_json = serde_json::json!({
        "code": code,
        "device_id": local_device_id,
        "name": local_name,
        "platform": local_platform,
        "pubkey": pubkey_hex,
        "ts": ts,
        "sig": sig,
    });
    let body = body_json.to_string();
    let (status, resp) = http::http_bytes(addr, "POST", "/pair", &[], Some(body.as_bytes()))?;
    if status != 200 {
        let msg = extract_error(&resp).unwrap_or_else(|| format!("HTTP {status}"));
        return Err(classify_pair_error(&msg));
    }

    let v: serde_json::Value = serde_json::from_slice(&resp)
        .map_err(|_| TransferError::proto("配对响应不是合法 JSON"))?;
    let peer_id = v["device_id"]
        .as_str()
        .ok_or_else(|| TransferError::proto("配对响应缺 device_id"))?
        .to_string();
    let peer_name = v["name"].as_str().unwrap_or("").to_string();
    let peer_platform = v["platform"].as_str().unwrap_or("").to_string();
    let peer_fp = v["fingerprint"]
        .as_str()
        .ok_or_else(|| TransferError::proto("配对响应缺 fingerprint"))?
        .to_string();
    let peer_pk_hex = v["pubkey"]
        .as_str()
        .ok_or_else(|| TransferError::proto("配对响应缺 pubkey"))?
        .to_string();
    let peer_ts = v["ts"].as_i64().unwrap_or(0);
    let peer_sig_hex = v["sig"].as_str().unwrap_or("");

    if !ts_ok(peer_ts) {
        return Err(TransferError::fingerprint_mismatch("对端响应时间戳过期"));
    }
    let peer_pk = from_hex(&peer_pk_hex)
        .ok_or_else(|| TransferError::fingerprint_mismatch("对端 pubkey 非法"))?;
    let peer_sig = from_hex(peer_sig_hex)
        .ok_or_else(|| TransferError::fingerprint_mismatch("对端 sig 非法"))?;
    let resp_payload = pair_server_payload(&peer_id, &peer_pk_hex, peer_ts);
    if !verify(&peer_pk, resp_payload.as_bytes(), &peer_sig) {
        return Err(TransferError::fingerprint_mismatch("对端响应签名验证失败"));
    }
    // 公钥推导指纹必须与自报一致。
    let derived = fingerprint_of(&peer_pk);
    if derived != peer_fp {
        return Err(TransferError::fingerprint_mismatch(format!(
            "对端自报指纹 {peer_fp} 与公钥推导 {derived} 不一致"
        )));
    }
    // 已 pin 指纹核对。
    if let Some(p) = pinned {
        if p != peer_fp {
            return Err(TransferError::fingerprint_mismatch(format!(
                "对端指纹与已登记不一致：已 pin {p}，本次 {peer_fp}"
            )));
        }
    }

    Ok(PairedPeer {
        device_id: peer_id,
        name: peer_name,
        platform: peer_platform,
        fingerprint: peer_fp,
        pubkey_hex: peer_pk_hex,
    })
}

fn extract_error(body: &[u8]) -> Option<String> {
    let v: serde_json::Value = serde_json::from_slice(body).ok()?;
    Some(v["error"]["message"].as_str()?.to_string())
}

fn classify_pair_error(msg: &str) -> TransferError {
    if msg.contains("配对码") || msg.contains("签名验证失败") {
        TransferError::bad_code(msg)
    } else if msg.contains("指纹") {
        TransferError::fingerprint_mismatch(msg)
    } else {
        TransferError::new(rm_proto::codes::F401, msg)
    }
}

/// 便捷入口：需要 `Arc<Identity>` 时用这个。
pub fn pair_with_arc(
    addr: &str,
    code: &str,
    identity: &Arc<Identity>,
    local_device_id: &str,
    local_name: &str,
    local_platform: &str,
    pinned: Option<&str>,
) -> Result<PairedPeer, TransferError> {
    pair(
        addr,
        code,
        identity.as_ref(),
        local_device_id,
        local_name,
        local_platform,
        pinned,
    )
}
