//! P3/P4 模式 A/B/C 本地代理（`docs/01` §5.3.1）：对端从 `http://<发送端>:<代理>/s/<stream_id>/<token>`
//! 拉流——A 把请求保留 Range 转发到 rm 的 127.0.0.1 流 URL（对端拿到的字节与直读
//! rm 流完全一致，rm URL 绑 127.0.0.1 不能直接转给对端，A-17）；B/C 起 ffmpeg
//! 子进程解码→重编码，以 **HTTP 分块流**推给对端（§5.3.1「对端直接从 7914 拉分块流」）。
//!
//! - 代理端口：默认 7914，忙则顺延（附录 B #44：7916..=7930，跳过 7915 lite），
//!   全忙 → **P203**；
//! - 路由表：进程级内存态 `{stream_id → rm_url + token + kind}`，cast.stop/会话 stop
//!   摘除；代理 token = 128-bit 随机 hex（§5.3.1），坏 token/未知流 → 403；
//! - A 转发语义：单请求单连接（`Connection: close`），Range 头透传，上游 200/206/403
//!   状态与 Content-Length/Content-Range 原样回传；
//! - B/C 转码语义：`GET` 忽略 Range（现场流不可寻址）→ 200 + `Transfer-Encoding:
//!   chunked`，ffmpeg stdout 逐块下发；客户端断开即杀子进程（#53）。

use std::collections::HashMap;
use std::io::{BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use crate::error::PlayerError;
use crate::httpmini;
use crate::shcmd;

/// 代理端口候选：7914（设计默认）→ 7916..=7930（跳过 7915：lite 控制面；附录 B #44）。
pub const PORT_CANDIDATES: &[u16] = &{
    let mut p = [0u16; 16];
    p[0] = 7914;
    let mut i = 1usize;
    while i < p.len() {
        p[i] = 7915 + i as u16; // i=1..15 → 7916..=7930
        i += 1;
    }
    p
};

/// 路由类型：A 字节转发 vs B/C 现场转码（`src_url` 即 `rm_url`，两种共用）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RouteKind {
    /// 模式 A：原样转发（Range 透传）。
    Bytes,
    /// 模式 B/C：ffmpeg 解码→重编码出分块流。
    Transcode { mode: &'static str },
}

/// 一条投屏流的代理路由。
#[derive(Clone, Debug)]
pub struct Route {
    pub rm_url: String,
    pub token: String,
    pub kind: RouteKind,
}

pub struct ProxyCore {
    pub port: u16,
    routes: Mutex<HashMap<String, Route>>,
}

impl ProxyCore {
    fn get(&self, stream_id: &str, token: &str) -> Option<Route> {
        let routes = self.routes.lock().unwrap();
        routes
            .get(stream_id)
            .filter(|r| r.token == token)
            .cloned()
    }
}

static PROXY: OnceLock<Arc<ProxyCore>> = OnceLock::new();
static INIT: Mutex<()> = Mutex::new(());
static CLIENT: OnceLock<reqwest::blocking::Client> = OnceLock::new();

fn client() -> &'static reqwest::blocking::Client {
    CLIENT.get_or_init(|| {
        reqwest::blocking::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .build()
            .expect("代理 HTTP 客户端")
    })
}

/// 按候选表绑第一个可监听端口（单元测试占位验证顺延）。
pub fn bind_first_free(candidates: &[u16]) -> std::io::Result<(u16, TcpListener)> {
    let mut last = None;
    for &p in candidates {
        match TcpListener::bind(("0.0.0.0", p)) {
            Ok(l) => return Ok((p, l)),
            Err(e) => last = Some(e),
        }
    }
    Err(last.unwrap_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::AddrNotAvailable, "无候选端口")
    }))
}

/// 128-bit 随机代理 token（32 hex；§5.3.1 内存态同 rm 机制，getrandom 跨平台）。
pub fn new_token() -> String {
    let mut buf = [0u8; 16];
    if getrandom::getrandom(&mut buf).is_err() {
        // 极端回退：时间 + 线程 id 混洗（OS 无随机源不现实，仅防 panic）。
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        std::time::SystemTime::now().hash(&mut h);
        std::thread::current().id().hash(&mut h);
        let v = h.finish();
        buf[..8].copy_from_slice(&v.to_le_bytes());
        buf[8..].copy_from_slice(&(!v).to_le_bytes());
    }
    buf.iter().map(|b| format!("{b:02x}")).collect()
}

/// 对端拉流 URL（发送端代理地址 + 路由 + token）。
pub fn proxy_url(host: &str, port: u16, stream_id: &str, token: &str) -> String {
    format!("http://{host}:{port}/s/{stream_id}/{token}")
}

/// 确保代理在跑：返回监听端口（进程级单例；附录 B #44）。
pub fn ensure_proxy() -> Result<u16, PlayerError> {
    if let Some(core) = PROXY.get() {
        return Ok(core.port);
    }
    let _g = INIT.lock().unwrap();
    if let Some(core) = PROXY.get() {
        return Ok(core.port);
    }
    let (port, listener) = bind_first_free(PORT_CANDIDATES).map_err(|e| {
        PlayerError::proxy_port_failed(format!("代理端口分配失败（7914..7930）：{e}"))
    })?;
    let core = Arc::new(ProxyCore { port, routes: Mutex::new(HashMap::new()) });
    let accept_core = Arc::clone(&core);
    std::thread::Builder::new()
        .name("player-proxy".into())
        .spawn(move || accept_loop(listener, accept_core))
        .map_err(|e| PlayerError::proxy_port_failed(format!("代理线程失败：{e}")))?;
    let _ = PROXY.set(core);
    Ok(port)
}

/// 注册 A 字节转发路由（cast 成功开流后调；ensure_proxy 先行）。
pub fn add_route(stream_id: &str, rm_url: &str, token: &str) {
    add_route_kind(stream_id, rm_url, token, RouteKind::Bytes);
}

/// 注册 B/C 转码路由（mode ∈ B/C）。
pub fn add_transcode_route(stream_id: &str, src_url: &str, token: &str, mode: &'static str) {
    add_route_kind(stream_id, src_url, token, RouteKind::Transcode { mode });
}

fn add_route_kind(stream_id: &str, rm_url: &str, token: &str, kind: RouteKind) {
    if let Some(core) = PROXY.get() {
        core.routes.lock().unwrap().insert(
            stream_id.to_string(),
            Route { rm_url: rm_url.to_string(), token: token.to_string(), kind },
        );
    }
}

/// 摘除路由（cast.stop / 会话 stop）。
pub fn remove_route(stream_id: &str) {
    if let Some(core) = PROXY.get() {
        core.routes.lock().unwrap().remove(stream_id);
    }
}

fn accept_loop(listener: TcpListener, core: Arc<ProxyCore>) {
    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        let core = Arc::clone(&core);
        std::thread::spawn(move || {
            let _ = handle_conn(stream, &core);
        });
    }
}

/// 单连接单请求：解析 → 路由 → 转发 → 关闭（`Connection: close`，附录 B #44）。
fn handle_conn(mut stream: TcpStream, core: &ProxyCore) -> std::io::Result<()> {
    let mut rd = BufReader::new(stream.try_clone()?);
    let Some(req) = httpmini::read_request(&mut rd)? else {
        return Ok(());
    };
    if req.method != "GET" && req.method != "HEAD" {
        httpmini::write_response(
            &mut stream,
            405,
            "text/plain",
            b"method not allowed",
        )?;
        return Ok(());
    }
    // /s/<stream_id>/<token>
    let segs: Vec<&str> = req.path.trim_start_matches('/').split('/').collect();
    let (Some(_), Some(sid), Some(token)) = (segs.first(), segs.get(1), segs.get(2)) else {
        httpmini::write_response(&mut stream, 403, "text/plain", b"forbidden")?;
        return Ok(());
    };
    let Some(route) = core.get(sid, token) else {
        // 未知流与坏 token 统一 403（不泄露存在性；§5.3.1 token 鉴权）。
        httpmini::write_response(&mut stream, 403, "text/plain", b"forbidden")?;
        return Ok(());
    };
    match route.kind {
        RouteKind::Bytes => forward(&mut stream, &req, &route),
        RouteKind::Transcode { mode } => {
            if req.method != "GET" {
                httpmini::write_response(&mut stream, 405, "text/plain", b"method not allowed")?;
                return Ok(());
            }
            stream_transcode(&mut stream, mode, &route.rm_url)
        }
    }
}

/// 把请求转发到 rm 流 URL（Range 透传；上游状态/头原样回传；body 流式拷贝）。
fn forward(stream: &mut TcpStream, req: &httpmini::Request, route: &Route) -> std::io::Result<()> {
    let mut up = if req.method == "HEAD" {
        client().head(&route.rm_url)
    } else {
        client().get(&route.rm_url)
    };
    if let Some(range) = req.header("range") {
        up = up.header(reqwest::header::RANGE, range);
    }
    // 字节语义要求 identity：防中间层压缩改字节。
    up = up.header(reqwest::header::ACCEPT_ENCODING, "identity");
    let mut resp = match up.send() {
        Ok(r) => r,
        Err(e) => {
            httpmini::write_response(
                stream,
                502,
                "text/plain",
                format!("upstream error: {e}").as_bytes(),
            )?;
            return Ok(());
        }
    };
    let status = resp.status().as_u16();
    let mut headers: Vec<(String, String)> = Vec::new();
    for name in ["content-type", "content-length", "content-range", "accept-ranges"] {
        if let Some(v) = resp.headers().get(name).and_then(|v| v.to_str().ok()) {
            headers.push((name.to_string(), v.to_string()));
        }
    }
    headers.push(("connection".into(), "close".into()));
    let head_refs: Vec<(&str, &str)> =
        headers.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    httpmini::write_head(stream, status, &head_refs)?;
    if req.method == "HEAD" {
        stream.flush()?;
        return Ok(());
    }
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        match resp.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => stream.write_all(&buf[..n])?,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => break, // 对端断开：结束转发
        }
    }
    stream.flush()
}

/// B/C 转码 ffmpeg 参数（纯函数，供单测；§5.3 模式 B/C 口径）。
///
/// - B：H.264 + AAC → MPEG-TS（无容器索引的现场流，mpv/ffmpeg 客户端免 seek 播放）；
/// - C：MJPEG 帧流（`mpjpeg` multipart，无音频——保最低延迟/最高兼容，§5.3）。
pub fn transcode_args(mode: &'static str, src_url: &str) -> Vec<String> {
    let common = |extra: &[&str]| -> Vec<String> {
        let mut v = vec![
            "ffmpeg".into(),
            "-nostdin".into(),
            "-v".into(),
            "error".into(),
            "-i".into(),
            src_url.into(),
        ];
        v.extend(extra.iter().map(|s| s.to_string()));
        v
    };
    match mode {
        "B" => common(&[
            "-c:v", "libx264", "-preset", "ultrafast", "-tune", "zerolatency",
            "-b:v", "4M", "-maxrate", "4M", "-bufsize", "8M",
            "-g", "48", "-pix_fmt", "yuv420p",
            "-c:a", "aac", "-b:a", "128k",
            "-f", "mpegts", "pipe:1",
        ]),
        "C" => common(&["-an", "-c:v", "mjpeg", "-q:v", "6", "-f", "mpjpeg", "pipe:1"]),
        _ => Vec::new(), // 调用方保证 mode ∈ {B,C}
    }
}

/// 转码前置探测（P104：本机无可用编解码器）：ffmpeg 存在且含所需编码器。
/// 结果按模式缓存（一次性子进程探测，cast 热路径零开销）。
pub fn ensure_transcode_capable(mode: &'static str) -> Result<(), PlayerError> {
    static PROBE: OnceLock<Mutex<HashMap<&'static str, bool>>> = OnceLock::new();
    let cache = PROBE.get_or_init(|| Mutex::new(HashMap::new()));
    {
        let map = cache.lock().unwrap();
        if let Some(&ok) = map.get(mode) {
            return if ok {
                Ok(())
            } else {
                Err(PlayerError::no_decoder(format!(
                    "本机无可用重编码器（模式 {mode} 需 ffmpeg 对应编码器）"
                )))
            };
        }
    }
    let out = shcmd::run(
        &["ffmpeg", "-hide_banner", "-encoders"],
        Duration::from_secs(10),
    );
    let text = out.map(|o| String::from_utf8_lossy(&o.stdout).to_string()).unwrap_or_default();
    let need = match mode {
        "B" => text.contains("libx264"),
        "C" => text.contains("mjpeg"),
        _ => false,
    };
    cache.lock().unwrap().insert(mode, need);
    if need {
        Ok(())
    } else {
        Err(PlayerError::no_decoder(format!(
            "本机无可用重编码器（模式 {mode} 需 ffmpeg 对应编码器）"
        )))
    }
}

/// 现场转码分块流：200 + chunked → ffmpeg stdout 逐块下发；客户端断开 → 杀子进程（#53）。
fn stream_transcode(stream: &mut TcpStream, mode: &'static str, src_url: &str) -> std::io::Result<()> {
    let args = transcode_args(mode, src_url);
    let mut child = match Command::new(&args[0])
        .args(&args[1..])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            httpmini::write_response(
                stream,
                502,
                "text/plain",
                format!("transcode spawn failed: {e}").as_bytes(),
            )?;
            return Ok(());
        }
    };
    let ctype = match mode {
        "B" => "video/mp2t",
        // mpjpeg 客户端按 multipart/x-mixed-replace 探测（boundary 用 ffmpeg 默认）。
        _ => "multipart/x-mixed-replace",
    };
    let head_ok = httpmini::write_head(stream, 200, &[
        ("content-type", ctype),
        ("transfer-encoding", "chunked"),
        ("connection", "close"),
    ]);
    if head_ok.is_err() {
        kill_child(&mut child);
        return Ok(());
    }
    pump_chunked(stream, &mut child);
    kill_child(&mut child);
    Ok(())
}

/// ffmpeg stdout → 分块响应体（每读一块发一个 chunk；读尽/写失败即止）。
fn pump_chunked(stream: &mut TcpStream, child: &mut Child) {
    let Some(out) = child.stdout.as_mut() else { return };
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        match out.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                if write_chunk(stream, &buf[..n]).is_err() {
                    break; // 对端断开：外层负责杀进程
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        }
    }
    let _ = stream.flush();
}

fn write_chunk<W: Write>(w: &mut W, data: &[u8]) -> std::io::Result<()> {
    write!(w, "{:X}\r\n", data.len())?;
    w.write_all(data)?;
    w.write_all(b"\r\n")
}

fn kill_child(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener as Tl;

    #[test]
    fn t1_token_128bit_hex() {
        let a = new_token();
        assert_eq!(a.len(), 32, "128-bit → 32 hex");
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
        let b = new_token();
        assert_ne!(a, b, "两次生成应不同");
        assert_eq!(new_token().len(), 32, "长度稳定");
    }

    #[test]
    fn t2_routes_lookup_and_remove() {
        // 直接测 ProxyCore 的匹配逻辑（ensure_proxy 由 e2e 走真实监听）。
        let core = ProxyCore { port: 1, routes: Mutex::new(HashMap::new()) };
        core.routes.lock().unwrap().insert(
            "st_1".into(),
            Route {
                rm_url: "http://127.0.0.1:7913/s/st_1/tok".into(),
                token: "aa".into(),
                kind: RouteKind::Bytes,
            },
        );
        assert!(core.get("st_1", "aa").is_some());
        assert!(core.get("st_1", "bb").is_none(), "坏 token 不命中");
        assert!(core.get("st_x", "aa").is_none(), "未知流不命中");
        core.routes.lock().unwrap().remove("st_1");
        assert!(core.get("st_1", "aa").is_none(), "摘除后不命中");
    }

    #[test]
    fn t6_transcode_args_shapes() {
        let b = transcode_args("B", "http://127.0.0.1:7913/s/st/tok");
        assert_eq!(b[0], "ffmpeg");
        let joined = b.join(" ");
        assert!(joined.contains("-i http://127.0.0.1:7913/s/st/tok"), "{joined}");
        assert!(joined.contains("libx264") && joined.contains("aac"), "B：H.264+AAC（§5.3）");
        assert!(joined.contains("-f mpegts pipe:1"), "B：MPEG-TS 现场流：{joined}");
        let c = transcode_args("C", "u");
        let joined = c.join(" ");
        assert!(joined.contains("mjpeg") && joined.contains("-f mpjpeg"), "C：MJPEG 帧流：{joined}");
        assert!(joined.contains("-an"), "C 无音频：{joined}");
        assert!(transcode_args("A", "u").is_empty(), "A 不走转码");
    }

    #[test]
    fn t7_route_kind_dispatch_data() {
        // A/B 路由 kind 数据面（add_* 经单例，此处测纯结构）。
        let a = Route { rm_url: "u".into(), token: "t".into(), kind: RouteKind::Bytes };
        assert_eq!(a.kind, RouteKind::Bytes);
        let b = Route { rm_url: "u".into(), token: "t".into(), kind: RouteKind::Transcode { mode: "B" } };
        assert_eq!(b.kind, RouteKind::Transcode { mode: "B" });
    }

    #[test]
    fn t3_bind_fallback_skips_occupied() {
        // 占住候选 1，必须顺延到候选 2。
        let hold = Tl::bind(("127.0.0.1", 0)).unwrap();
        let p1 = hold.local_addr().unwrap().port();
        let free = Tl::bind(("127.0.0.1", 0)).unwrap();
        let p2 = free.local_addr().unwrap().port();
        drop(free);
        let (port, _l) = bind_first_free(&[p1, p2]).unwrap();
        assert_eq!(port, p2, "被占端口应顺延");
        // 全占 → 错误
        let hold2 = Tl::bind(("127.0.0.1", 0)).unwrap();
        let p3 = hold2.local_addr().unwrap().port();
        drop(hold); // p1 释放后 p3 仍占
        assert!(bind_first_free(&[p3]).is_err(), "单候选被占应报错");
    }

    #[test]
    fn t4_proxy_url_shape() {
        assert_eq!(
            proxy_url("192.168.1.5", 7914, "st_9", "ab"),
            "http://192.168.1.5:7914/s/st_9/ab"
        );
    }

    #[test]
    fn t5_port_candidates_skip_7915() {
        assert_eq!(PORT_CANDIDATES[0], 7914);
        assert!(!PORT_CANDIDATES.contains(&7915), "7915 归 lite 控制面");
        assert!(PORT_CANDIDATES.contains(&7916) && PORT_CANDIDATES.contains(&7930));
        assert_eq!(PORT_CANDIDATES.len(), 16);
        assert!(!PORT_CANDIDATES.contains(&7931), "顺延池上界 7930");
    }
}
