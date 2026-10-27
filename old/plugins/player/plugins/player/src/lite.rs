//! P3 lite 接收器（`docs/01` §5.4/§5.13；附录 B #46/#47）：同一代码库的精简构建
//! （bin `player-lite`）——只有 Sink + 解码器 + 被动发现，没有 Source/Capability/Session。
//!
//! - HTTP 控制面（默认 7915，`--port 0` 走 OS 分配并 `PORT=` 回显）：
//!   `POST /cast`（§5.13 照表，mode A/B/C——C 起 P4，#54）、`GET /status`、`POST /stop`；
//! - UDP 心跳每 1 秒宣告自身（`127.0.0.1` + 广播，端口 7916），并宣称
//!   `codecs=["h264","aac"]`（P4 自动路由输入，#50）；
//! - 收到投屏 → 起 `MpvBackend` 从 `stream_url` 播（A 字节流 / B mpegts / C mpjpeg
//!   均由 mpv 运行时探测解复用；headless 环境 `vo=null ao=null` 继承 `PLAYER_VO`/DISPLAY
//!   判定）；播完/失败 → 回待机（§5.13）。

use std::net::{TcpListener, TcpStream, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::Value;

use crate::backend::MpvBackend;
use crate::discovery;
use crate::httpmini;
use crate::proxy::new_token;

/// lite 宣称的编解码基线（心跳外发；对端据此自动路由，#50）。
/// mpv/ffmpeg 实际能解的远多于此——宣称的是「保证可播」的保守基线。
pub const LITE_CODECS: &[&str] = &["h264", "aac"];

/// lite 启动参数（附录 B #47）。
pub struct LiteOpts {
    /// 监听端口；0 = OS 分配（测试用，stdout `PORT=` 回显）。
    pub http_port: u16,
    pub name: String,
    pub id: String,
    /// 心跳目的地（`host:port`）；空 → 默认 `127.0.0.1:7916` + 广播 `255.255.255.255:7916`。
    /// e2e/同机多实例用它指向 `receiver.list` 外发的 `discovery_port`（附录 B #44）。
    pub hb_targets: Vec<String>,
}

impl Default for LiteOpts {
    fn default() -> Self {
        let t = new_token();
        Self {
            http_port: 7915,
            name: "lite".into(),
            id: format!("lite-{}", &t[..12]),
            hb_targets: Vec::new(),
        }
    }
}

impl LiteOpts {
    /// 实际心跳目的地（未显式指定 → 设计默认双目的地）。
    pub fn heartbeat_targets(&self) -> Vec<String> {
        if !self.hb_targets.is_empty() {
            return self.hb_targets.clone();
        }
        vec![
            format!("127.0.0.1:{}", discovery::DISCOVERY_PORT),
            format!("255.255.255.255:{}", discovery::DISCOVERY_PORT),
        ]
    }
}

struct Playing {
    backend: MpvBackend,
    eof: Arc<AtomicBool>,
}

struct LiteState {
    id: String,
    name: String,
    port: u16,
    player: Mutex<Option<Playing>>,
    mode: Mutex<String>,
    stream_url: Mutex<String>,
}

impl LiteState {
    /// 心跳/status 共用状态派生：idle | playing | paused（EOF 回待机，§5.13）。
    fn state(&self) -> &'static str {
        let slot = self.player.lock().unwrap();
        let Some(p) = slot.as_ref() else { return "idle" };
        if p.eof.load(Ordering::SeqCst) {
            "idle"
        } else if p.backend.is_paused() {
            "paused"
        } else {
            "playing"
        }
    }

    fn position(&self) -> f64 {
        self.player.lock().unwrap().as_ref().map(|p| p.backend.position()).unwrap_or(0.0)
    }

    fn duration(&self) -> f64 {
        self.player.lock().unwrap().as_ref().map(|p| p.backend.duration()).unwrap_or(0.0)
    }
}

/// 校验投屏请求（纯函数，供单测）：mode ∈ A/B/C（P4 起收 B/C，#54；D=电视协议占位
/// 与未知值 → unsupported_mode）；缺 stream_url → bad_request。返回 (mode, url, autoplay)。
pub fn validate_cast(body: &Value) -> Result<(String, String, bool), &'static str> {
    let mode = body["mode"].as_str().unwrap_or("A");
    if !matches!(mode, "A" | "B" | "C") {
        return Err("unsupported_mode");
    }
    let url = body["stream_url"].as_str().filter(|u| !u.is_empty()).ok_or("bad_request")?;
    let autoplay = body["autoplay"].as_bool().unwrap_or(true);
    Ok((mode.to_string(), url.to_string(), autoplay))
}

/// 起播（旧实例先收束再换新——事件泵 EOF 后自停，必须换新实例）。
fn start_playback(state: &LiteState, url: &str, autoplay: bool) -> Result<(), &'static str> {
    let eof = Arc::new(AtomicBool::new(false));
    let eof_cb = Arc::clone(&eof);
    let backend = MpvBackend::start(move |ev| {
        use crate::backend::BackendEvent;
        if matches!(ev, BackendEvent::Finished | BackendEvent::Failed { .. }) {
            eof_cb.store(true, Ordering::SeqCst);
        }
    })
    .map_err(|_| "decoder_error")?;
    backend.load(url).map_err(|_| "decoder_error")?;
    if !autoplay {
        let _ = backend.set_pause(true);
    }
    let mut slot = state.player.lock().unwrap();
    *slot = Some(Playing { backend, eof });
    Ok(())
}

fn status_json(state: &LiteState) -> Value {
    let mode = state.mode.lock().unwrap().clone();
    let stream_url = state.stream_url.lock().unwrap().clone();
    serde_json::json!({
        "id": state.id,
        "name": state.name,
        "state": state.state(),
        "mode": mode,
        "position_sec": state.position(),
        "duration_sec": state.duration(),
        "stream_url": stream_url,
    })
}

/// 运行 lite（阻塞；绑端口成功先回显 `PORT=`，再心跳 + 接受连接）。
pub fn run(opts: LiteOpts) -> std::io::Result<()> {
    let listener = TcpListener::bind(("0.0.0.0", opts.http_port))?;
    let port = listener.local_addr()?.port();
    let targets = opts.heartbeat_targets();
    let state = Arc::new(LiteState {
        id: opts.id,
        name: opts.name,
        port,
        player: Mutex::new(None),
        mode: Mutex::new("A".into()),
        stream_url: Mutex::new(String::new()),
    });

    // 心跳线程（发送侧无绑定冲突；目的地默认 7916，--hb 可覆盖）。
    {
        let st = Arc::clone(&state);
        std::thread::Builder::new()
            .name("lite-heartbeat".into())
            .spawn(move || heartbeat_loop(st, targets))?;
    }
    println!("PORT={port}");
    let _ = std::io::Write::flush(&mut std::io::stdout());

    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        let st = Arc::clone(&state);
        std::thread::spawn(move || {
            let _ = handle_conn(stream, &st);
        });
    }
    Ok(())
}

fn heartbeat_loop(state: Arc<LiteState>, targets: Vec<String>) {
    let Ok(sock) = UdpSocket::bind("0.0.0.0:0") else { return };
    let _ = sock.set_broadcast(true);
    loop {
        let payload = discovery::heartbeat_payload(
            &state.id,
            &state.name,
            state.port,
            state.state(),
            LITE_CODECS,
        );
        for dest in &targets {
            let _ = sock.send_to(payload.as_bytes(), dest);
        }
        std::thread::sleep(Duration::from_secs(1));
    }
}

/// 单连接单请求（`Connection: close`）。
fn handle_conn(mut stream: TcpStream, state: &LiteState) -> std::io::Result<()> {
    let mut rd = std::io::BufReader::new(stream.try_clone()?);
    let Some(req) = httpmini::read_request(&mut rd)? else {
        return Ok(());
    };
    match (req.method.as_str(), req.path.as_str()) {
        ("POST", "/cast") => {
            let body = httpmini::read_body(&mut rd, req.content_length())?;
            let value: Value = serde_json::from_slice(&body)
                .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidData, "非 JSON"))?;
            match validate_cast(&value) {
                Ok((mode, url, autoplay)) => match start_playback(state, &url, autoplay) {
                    Ok(()) => {
                        *state.mode.lock().unwrap() = mode;
                        *state.stream_url.lock().unwrap() = url;
                        json_ok(&mut stream, &serde_json::json!({"accepted": true, "reason": null}))
                    }
                    Err(reason) => json_ok(
                        &mut stream,
                        &serde_json::json!({"accepted": false, "reason": reason}),
                    ),
                },
                Err(reason) => json_ok(
                    &mut stream,
                    &serde_json::json!({"accepted": false, "reason": reason}),
                ),
            }
        }
        ("GET", "/status") => json_ok(&mut stream, &status_json(state)),
        ("POST", "/stop") => {
            // 收束播放（Drop 会 join 事件线程，≤0.5s 心跳）→ 回待机。
            *state.player.lock().unwrap() = None;
            *state.stream_url.lock().unwrap() = String::new();
            json_ok(&mut stream, &serde_json::json!({"ok": true}))
        }
        _ => {
            httpmini::write_response(&mut stream, 404, "text/plain", b"not found")?;
            Ok(())
        }
    }
}

fn json_ok(stream: &mut TcpStream, value: &Value) -> std::io::Result<()> {
    httpmini::write_response(stream, 200, "application/json", value.to_string().as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn t1_validate_cast_modes() {
        // mode 缺省/显式 A → 收
        let (m, u, a) =
            validate_cast(&serde_json::json!({"stream_url": "http://x/s/a/t"})).unwrap();
        assert_eq!((m.as_str(), u.as_str()), ("A", "http://x/s/a/t"));
        assert!(a, "autoplay 默认 true");
        let (_, _, a) = validate_cast(
            &serde_json::json!({"mode": "A", "stream_url": "u", "autoplay": false}),
        )
        .unwrap();
        assert!(!a, "autoplay 显式 false 保留");
        // B/C（P4 起收，#54）→ 收且原样回传 mode
        for mode in ["B", "C"] {
            let (m, _, _) =
                validate_cast(&serde_json::json!({"mode": mode, "stream_url": "u"})).unwrap();
            assert_eq!(m, mode);
        }
        // D（电视协议占位）与未知 mode → 拒
        for mode in ["D", "E", "auto"] {
            assert_eq!(
                validate_cast(&serde_json::json!({"mode": mode, "stream_url": "u"}))
                    .unwrap_err(),
                "unsupported_mode",
                "mode {mode}"
            );
        }
        // 缺 stream_url / 空 URL → 拒
        assert_eq!(
            validate_cast(&serde_json::json!({"mode": "A"})).unwrap_err(),
            "bad_request"
        );
        assert_eq!(
            validate_cast(&serde_json::json!({"stream_url": ""})).unwrap_err(),
            "bad_request"
        );
    }

    #[test]
    fn t3_lite_codecs_claim() {
        assert_eq!(LITE_CODECS, &["h264", "aac"], "保守宣称基线（#50）");
        // 与心跳负载互通
        let s = discovery::heartbeat_payload("id", "n", 7915, "idle", LITE_CODECS);
        let hb = discovery::parse_heartbeat(s.as_bytes()).unwrap();
        assert_eq!(hb.codecs, vec!["h264", "aac"]);
    }

    #[test]
    fn t2_default_opts_shape() {
        let o = LiteOpts::default();
        assert_eq!(o.http_port, 7915, "设计默认端口（§5.13）");
        assert!(o.id.starts_with("lite-") && o.id.len() > 8);
        assert_ne!(LiteOpts::default().id, o.id, "id 每次启动随机");
        // 心跳目的地：默认双目的地（loopback + 广播），--hb 覆盖
        let def = o.heartbeat_targets();
        assert_eq!(def.len(), 2);
        assert!(def[0].ends_with(":7916") && def[1].starts_with("255.255.255.255:"));
        let mut custom = o;
        custom.hb_targets.push("127.0.0.1:55555".into());
        assert_eq!(custom.heartbeat_targets(), vec!["127.0.0.1:55555".to_string()]);
    }
}
