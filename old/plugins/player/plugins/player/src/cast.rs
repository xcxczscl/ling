//! P3/P4 投屏编排（`docs/01` §5.12 `player.cast` / §5.13 / §5.3.2；附录 B #42/#45/#50/#53）：
//! 选定会话 → 独立开 rm 流 → **解析投送模式**（override 或按接收器宣称/内容编码/带宽
//! 自动选择，#50）→ 起代理路由（A 字节转发 / B·C 转码分块流）→ POST 对端 `/cast` → 事件广播。
//!
//! - 会话须 mpv 会话（viewer → **P103**）；mode ∈ {缺省=auto, "A", "B", "C"}，
//!   `"D"`（电视协议占位）/未知值 → **P100**；
//! - 未知接收器/对端无响应 → **P201**；接收器 `accepted:false` → **P202**；
//! - 本机无对应重编码器（B/C 前置探测）→ **P104**；
//! - 并行镜像：本机会话不受影响，对端从流起点起播（进度联动属 P7，#45）；
//! - 重投先停旧投（`cast.ended reason=replaced`）；会话 stop 自动结束（`reason=stopped`）。

use std::net::UdpSocket;
use std::sync::Arc;
use std::time::Duration;

use ling_sdk::Context;
use serde_json::Value;

use crate::discovery::{self, ReceiverInfo};
use crate::error::{from_rm_err, PlayerError};
use crate::proxy;
use crate::router::{auto_select_mode, BandwidthEstimate};
use crate::shcmd;
use crate::PlayerState;

/// 一条进行中的投屏（会话 id → 目标与流路由）。
#[derive(Clone, Debug)]
pub struct CastState {
    pub receiver_id: String,
    pub stream_id: String,
    /// 实际投送模式（auto 解析后的 A/B/C）。
    pub mode: String,
}

/// mode 入参解析（§5.3.2：缺省 = 自动路由；附录 B #45/#50）：
/// `None` → 自动；`Some("A"|"B"|"C")` → 用户强制；`"D"`/未知 → P100。
pub fn parse_mode(args: &Value) -> Result<Option<String>, PlayerError> {
    match args.get("mode").map(Value::as_str) {
        None => Ok(None),                                  // 未指定 → 自动
        Some(Some("")) => Ok(None),                        // 空串 → 视为未指定
        Some(None) => Err(PlayerError::bad_param("mode 须为字符串（auto/A/B/C）")),
        Some(Some(m)) if m == "A" || m == "B" || m == "C" => Ok(Some(m.to_string())),
        Some(Some(m)) if m == "D" => Err(PlayerError::bad_param(
            "模式 D（电视协议 Sink）P4 仅接口占位，未实现（§5.4）",
        )),
        Some(Some(m)) => Err(PlayerError::bad_param(format!(
            "未知投送模式：{m}（可用：auto/A/B/C）"
        ))),
    }
}

/// 内容视频编码探测（ffprobe 取首条视频流 codec_name；失败 → None）。
/// 供自动路由判定「对端是否支持当前编码」（§5.3.2 target.supports_codec）。
pub fn probe_content_codec(url: &str) -> Option<String> {
    let out = shcmd::run(
        &[
            "ffprobe", "-v", "quiet",
            "-select_streams", "v:0",
            "-show_entries", "stream=codec_name",
            "-of", "default=nw=1:nk=1",
            url,
        ],
        Duration::from_secs(10),
    )
    .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if s.is_empty() { None } else { Some(s) }
}

/// 发送端可达地址：按路由表选到对端的本机 IP（UDP connect 只查路由不发包）。
fn sender_host_for(peer_ip: &str) -> Result<String, PlayerError> {
    let sock = UdpSocket::bind("0.0.0.0:0")
        .map_err(|e| PlayerError::target_unreachable(format!("获取本机地址失败：{e}")))?;
    sock.connect((peer_ip, 1))
        .map_err(|e| PlayerError::target_unreachable(format!("对端不可路由：{peer_ip}（{e}）")))?;
    let local = sock
        .local_addr()
        .map_err(|e| PlayerError::target_unreachable(format!("获取本机地址失败：{e}")))?;
    Ok(local.ip().to_string())
}

fn http_client() -> Result<reqwest::blocking::Client, PlayerError> {
    reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .map_err(|e| PlayerError::target_unreachable(format!("HTTP 客户端失败：{e}")))
}

/// `player.cast`：投屏（编排全流程）。
pub fn cast(ctx: &Context, state: &Arc<PlayerState>, args: &Value) -> Result<Value, PlayerError> {
    let sid = crate::str_field(args, "session_id")?;
    let session = state.sessions.get(&sid)?; // P401
    if session.is_viewer() {
        return Err(PlayerError::unsupported_asset("查看器会话不可投屏"));
    }
    let mode_override = parse_mode(args)?;
    let target_device = crate::str_field(args, "target_device")?;

    // 重投：先结束旧投屏（收路由/停对端/发 ended=replaced）。
    end_cast(ctx, state, &sid, "replaced");

    let rx = discovery::get(&target_device)
        .ok_or_else(|| PlayerError::target_unreachable(format!("未知接收器：{target_device}")))?;

    // Source：独立开流（不复用本机会话的 mpv 句柄，附录 B #45）。
    let placement_id = session.status()["placement_id"]
        .as_i64()
        .ok_or_else(|| PlayerError::content_unavailable("会话缺 placement_id"))?;
    let opened = ctx
        .call(
            "resources-manager",
            "resources-manager.stream.open",
            serde_json::json!({ "placement_id": placement_id, "purpose": "cast" }),
        )
        .map_err(|e| from_rm_err(&e))?;
    let rm_url = opened["url"]
        .as_str()
        .ok_or_else(|| PlayerError::content_unavailable("stream.open 未返回 url"))?
        .to_string();
    let stream_id = opened["stream_id"].as_str().unwrap_or("").to_string();
    if stream_id.is_empty() {
        return Err(PlayerError::content_unavailable("stream.open 未返回 stream_id"));
    }

    // 路由解析（§5.3.2）：用户强制 > 自动（接收器宣称 ↔ 内容编码 ↔ 固定带宽估值）。
    let content_codec = probe_content_codec(&rm_url).unwrap_or_default();
    let mode: &'static str = match mode_override.as_deref() {
        Some("A") => "A",
        Some("B") => "B",
        Some("C") => "C",
        _ => auto_select_mode(&rx.codecs, &content_codec, &BandwidthEstimate::fixed()),
    };
    // B/C 前置探测：本机无对应编码器 → P104（比对端播到一半失败更早暴露）。
    if mode == "B" || mode == "C" {
        proxy::ensure_transcode_capable(mode)?;
    }

    // 代理：确保在跑 + 注册路由（A 字节转发 / B·C 转码；坏路径一律摘路由）。
    let port = proxy::ensure_proxy()?;
    let host = sender_host_for(&rx.ip)?;
    let token = proxy::new_token();
    match mode {
        "A" => proxy::add_route(&stream_id, &rm_url, &token),
        m => proxy::add_transcode_route(&stream_id, &rm_url, &token, m),
    }
    let stream_url = proxy::proxy_url(&host, port, &stream_id, &token);

    let duration_sec = session.status()["duration_sec"].as_f64().unwrap_or(0.0);
    let mut media_info = serde_json::json!({ "duration_sec": duration_sec });
    if !content_codec.is_empty() {
        media_info["codec"] = serde_json::Value::String(content_codec);
    }
    let body = serde_json::json!({
        "mode": mode,
        "stream_url": stream_url,
        "media_info": media_info,
        "autoplay": true,
    });
    let cast_url = format!("http://{}:{}/cast", rx.ip, rx.port);
    // reqwest 未开 json feature：手写 Content-Type + serde 解析（§依赖面不变）。
    let resp = match http_client()?
        .post(&cast_url)
        .header("content-type", "application/json")
        .body(body.to_string())
        .send()
    {
        Ok(r) if r.status().is_success() => r
            .text()
            .ok()
            .and_then(|t| serde_json::from_str::<Value>(&t).ok())
            .unwrap_or(Value::Null),
        Ok(r) => {
            proxy::remove_route(&stream_id);
            return Err(PlayerError::target_unreachable(format!(
                "接收器 HTTP {cast_url} 返回 {}",
                r.status()
            )));
        }
        Err(e) => {
            proxy::remove_route(&stream_id);
            return Err(PlayerError::target_unreachable(format!(
                "接收器不可达 {cast_url}：{e}"
            )));
        }
    };
    if resp["accepted"] != true {
        proxy::remove_route(&stream_id);
        let reason = resp["reason"].as_str().unwrap_or("unknown");
        return Err(PlayerError::stream_rejected(format!("接收器拒绝投屏：{reason}")));
    }

    state.casts.lock().unwrap().insert(
        sid.clone(),
        CastState {
            receiver_id: rx.id.clone(),
            stream_id,
            mode: mode.to_string(),
        },
    );

    let _ = ctx.emit(
        "player.cast.started",
        serde_json::json!({ "session_id": sid, "target": rx.id, "mode": mode }),
    );

    Ok(serde_json::json!({
        "receiver_id": rx.id,
        "mode": mode,
        "accepted": true,
    }))
}

/// 结束一条投屏：摘路由 → 通知对端回待机（best-effort）→ 发 `cast.ended`（含 mode，
/// #55 扩展：UI 需要感知哪条投送结束）。
/// 返回是否确有投屏被结束（`player.cast.stop` 响应用）。
pub fn end_cast(ctx: &Context, state: &Arc<PlayerState>, sid: &str, reason: &str) -> bool {
    let Some(cast) = state.casts.lock().unwrap().remove(sid) else {
        return false;
    };
    proxy::remove_route(&cast.stream_id);
    if let Some(rx) = discovery::get(&cast.receiver_id) {
        notify_stop(&rx);
    }
    let _ = ctx.emit(
        "player.cast.ended",
        serde_json::json!({ "session_id": sid, "reason": reason, "mode": cast.mode }),
    );
    true
}

/// 对端回待机（§5.13 生命周期；实装增补 /stop，附录 B #46）。
fn notify_stop(rx: &ReceiverInfo) {
    let url = format!("http://{}:{}/stop", rx.ip, rx.port);
    let _ = http_client().and_then(|c| {
        c.post(&url)
            .send()
            .map_err(|e| PlayerError::target_unreachable(e.to_string()))
    });
}

/// `player.cast.stop` 入参校验 + 执行（无投屏 → `{stopped:false}`，不报错）。
pub fn stop(
    ctx: &Context,
    state: &Arc<PlayerState>,
    args: &Value,
) -> Result<Value, PlayerError> {
    let sid = crate::str_field(args, "session_id")?;
    let stopped = end_cast(ctx, state, &sid, "user");
    Ok(serde_json::json!({ "stopped": stopped }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn t1_parse_mode() {
        // 缺省 → 自动路由（None；§5.3.2 user_override=None）
        assert_eq!(parse_mode(&serde_json::json!({})).unwrap(), None);
        assert!(parse_mode(&serde_json::json!({"mode": null})).is_err(), "null 非法（P100）");
        assert!(parse_mode(&serde_json::json!({"mode": ""})).unwrap().is_none());
        // 用户强制 A/B/C → Some（P4 起 B/C 合法，#50）
        for m in ["A", "B", "C"] {
            assert_eq!(
                parse_mode(&serde_json::json!({"mode": m})).unwrap().as_deref(),
                Some(m)
            );
        }
        // D（电视协议占位）/ 未知值 / 非字符串 → P100
        let e = parse_mode(&serde_json::json!({"mode": "D"})).unwrap_err();
        assert_eq!(e.code, "P100");
        assert!(e.message.contains("占位"), "{}", e.message);
        let e = parse_mode(&serde_json::json!({"mode": "E"})).unwrap_err();
        assert_eq!(e.code, "P100");
        assert!(e.message.contains("auto/A/B/C"));
        let e = parse_mode(&serde_json::json!({"mode": 7})).unwrap_err();
        assert_eq!(e.code, "P100");
    }

    #[test]
    fn t3_probe_content_codec_shapes() {
        // 不存在的 URL → None（探测失败降级，不 panic）
        assert_eq!(probe_content_codec("http://127.0.0.1:1/nope"), None);
    }

    #[test]
    fn t2_sender_host_loopback() {
        // 同机对端 → 本机地址应回 127.0.0.1（e2e 同机投屏依赖此判定）。
        assert_eq!(sender_host_for("127.0.0.1").unwrap(), "127.0.0.1");
        // 非法地址 → P201（解析失败；合法但不可路由的地址由 5s POST 超时兜成 P201）
        let e = sender_host_for("not-an-ip").unwrap_err();
        assert_eq!(e.code, "P201");
    }
}
