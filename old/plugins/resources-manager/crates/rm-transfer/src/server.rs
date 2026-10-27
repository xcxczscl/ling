//! 局域网传输面服务端（`docs/03` §1 第二例外、§5.2）。
//!
//! `std::net::TcpListener` + 每连接一线程；端口默认 7912，占用顺延。

use std::io::Write;
use std::net::{Ipv4Addr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use crate::error::TransferError;
use crate::http::{self, Request};
use crate::identity::{fingerprint_of, verify, Identity};
use crate::protocol::{
    blob_sign_payload, from_hex, now_ts, op_sign_payload, pair_client_payload, pair_server_payload,
    to_hex, ts_ok, BLOCK_HEADER_LEN, BLOCK_SIZE, DEFAULT_PORT, PAIRING_TTL_SECS, PROTO,
};

/// 配对码状态（服务端内存态；`docs/03` §5.2：10 分钟过期、一次性）。
#[derive(Default)]
pub struct PairingState {
    code: Option<String>,
    expires_at: i64,
}

impl PairingState {
    /// 生成新码（旧码作废）。返回 (码, 有效期秒)。
    pub fn issue(&mut self) -> (String, u64) {
        let mut raw = [0u8; 4];
        let _ = getrandom::getrandom(&mut raw);
        let n = u32::from_le_bytes(raw) % 100_000_000;
        let code = format!("{n:08}");
        self.expires_at = now_ts() + protocol_pairing_ttl();
        self.code = Some(code.clone());
        (code, PAIRING_TTL_SECS)
    }

    /// 校验并消费配对码（一次性）。
    fn consume(&mut self, code: &str) -> bool {
        let ok = self
            .code
            .as_deref()
            .map(|c| c == code && now_ts() <= self.expires_at)
            .unwrap_or(false);
        if ok {
            self.code = None;
        }
        ok
    }
}

fn protocol_pairing_ttl() -> i64 {
    PAIRING_TTL_SECS as i64
}

/// 服务端需要的目录库能力（由调用方实现，解耦 SQLite 连接所有权）。
pub trait ServerCatalog: Send + Sync {
    /// 对端已 pin 的指纹；未配对返回 None。
    fn peer_fingerprint(&self, device_id: &str) -> Option<String>;
    /// 对端已登记的公钥（hex）；用于验请求签名。
    fn peer_pubkey(&self, device_id: &str) -> Option<String>;
    /// 配对成功后登记对端（peer + device 两表）。
    fn upsert_peer(
        &self,
        device_id: &str,
        name: &str,
        platform: &str,
        fingerprint: &str,
        pubkey_hex: &str,
        addr: Option<&str>,
    ) -> Result<(), String>;
    /// 可提供的数据源：(placement_id, 绝对路径, size)；cold/离线/压缩一律不返回。
    fn serving_paths(&self, content_blake3: &str) -> Result<Vec<(i64, String, i64)>, String>;
    /// 导出 op 流水（after 之后，最多 limit 条），JSON 数组。
    fn export_ops(&self, after: i64, limit: u32) -> Result<Vec<serde_json::Value>, String>;
    /// 导入对端 op 流水，返回 (applied, skipped, conflicts)。
    fn import_ops(
        &self,
        peer_device: &str,
        ops: Vec<serde_json::Value>,
    ) -> Result<(usize, usize, usize), String>;
}

/// 服务端配置。
pub struct ServerConfig {
    pub device_id: String,
    pub name: String,
    pub platform: String,
    pub identity: Arc<Identity>,
    /// 0 = 从 DEFAULT_PORT 起顺延。
    pub port: u16,
}

/// 已启动的服务端句柄。
pub struct ServerHandle {
    port: u16,
    stop: Arc<AtomicBool>,
    join: Option<std::thread::JoinHandle<()>>,
}

impl ServerHandle {
    pub fn port(&self) -> u16 {
        self.port
    }

    /// 停止监听（消费句柄）。
    pub fn stop(mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(("127.0.0.1", self.port));
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

impl Drop for ServerHandle {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(("127.0.0.1", self.port));
    }
}

struct Shared {
    cfg: ServerConfig,
    catalog: Arc<dyn ServerCatalog>,
    pairing: Arc<Mutex<PairingState>>,
}

/// 启动 LAN 传输面：绑定 `0.0.0.0:<port>`，被占用则顺延最多 20 个端口。
pub fn start(
    cfg: ServerConfig,
    catalog: Arc<dyn ServerCatalog>,
    pairing: Arc<Mutex<PairingState>>,
) -> Result<ServerHandle, TransferError> {
    let stop = Arc::new(AtomicBool::new(false));
    let mut listener: Option<TcpListener> = None;
    let mut bound = 0u16;
    let base = if cfg.port == 0 { DEFAULT_PORT } else { cfg.port };
    for port in base..base + 20 {
        if let Ok(l) = TcpListener::bind((Ipv4Addr::UNSPECIFIED, port)) {
            listener = Some(l);
            bound = port;
            break;
        }
    }
    let listener = listener.ok_or_else(|| {
        TransferError::new(
            rm_proto::codes::F504,
            format!("端口 {base}–{} 全部被占用，传输面启动失败", base + 19),
        )
    })?;

    let shared = Arc::new(Shared {
        cfg,
        catalog,
        pairing,
    });
    let listener_stop = Arc::clone(&stop);
    let join = std::thread::Builder::new()
        .name("rm-transfer-server".into())
        .spawn(move || {
            for stream in listener.incoming() {
                if listener_stop.load(Ordering::SeqCst) {
                    break;
                }
                let Ok(stream) = stream else { continue };
                let shared = Arc::clone(&shared);
                std::thread::spawn(move || {
                    let _ = handle_conn(shared, stream);
                });
            }
        })
        .map_err(|e| TransferError::io(format!("启动服务线程失败：{e}")))?;

    Ok(ServerHandle {
        port: bound,
        stop,
        join: Some(join),
    })
}

fn handle_conn(shared: Arc<Shared>, mut stream: TcpStream) -> Result<(), TransferError> {
    let req = http::read_request(&stream)?;
    let peer_ip = stream
        .peer_addr()
        .map(|a| a.ip().to_string())
        .unwrap_or_default();

    if req.method == "GET" && req.path.starts_with("/blob/") {
        return blob_stream(&shared, &req, &mut stream);
    }

    match route(&shared, &req, &peer_ip) {
        Ok((status, ctype, body)) => {
            http::write_response(&mut stream, status, ctype, &body)
                .map_err(|e| TransferError::io(format!("写响应失败：{e}")))
        }
        Err(e) => write_error(&mut stream, &e),
    }
}

fn write_error(stream: &mut TcpStream, e: &TransferError) -> Result<(), TransferError> {
    let status = match e.code.as_str() {
        "F401" | "F402" | "F403" => 403,
        "F404" => 404,
        "F301" => 400,
        _ => 500,
    };
    let body = serde_json::json!({"error": {"code": e.code, "message": e.message}}).to_string();
    let _ = http::write_response(stream, status, "application/json", body.as_bytes());
    Ok(())
}

type RouteResult = Result<(u16, &'static str, Vec<u8>), TransferError>;

fn route(shared: &Shared, req: &Request, peer_ip: &str) -> RouteResult {
    match (req.method.as_str(), req.path.as_str()) {
        ("GET", "/hello") => hello(shared),
        ("POST", "/pair") => pair(shared, req, peer_ip),
        ("GET", "/op/export") => op_export(shared, req),
        ("POST", "/op/import") => op_import(shared, req),
        _ => Err(TransferError::new(
            "F301",
            format!("未知路径：{}", req.path),
        )),
    }
}

fn json_ok(v: serde_json::Value) -> (u16, &'static str, Vec<u8>) {
    (200, "application/json", v.to_string().into_bytes())
}

fn hello(shared: &Shared) -> RouteResult {
    Ok(json_ok(serde_json::json!({
        "device_id": shared.cfg.device_id,
        "fingerprint": shared.cfg.identity.fingerprint(),
        "name": shared.cfg.name,
        "platform": shared.cfg.platform,
        "proto": PROTO,
    })))
}

fn pair(shared: &Shared, req: &Request, peer_ip: &str) -> RouteResult {
    let body: serde_json::Value = serde_json::from_slice(&req.body)
        .map_err(|_| TransferError::proto("配对报文不是合法 JSON"))?;
    let code = body["code"].as_str().unwrap_or("");
    let peer_id = body["device_id"].as_str().unwrap_or("");
    let name = body["name"].as_str().unwrap_or("");
    let platform = body["platform"].as_str().unwrap_or("");
    let pubkey_hex = body["pubkey"].as_str().unwrap_or("");
    let ts = body["ts"].as_i64().unwrap_or(0);
    let sig_hex = body["sig"].as_str().unwrap_or("");

    if peer_id.is_empty() || pubkey_hex.is_empty() {
        return Err(TransferError::proto("配对报文缺 device_id/pubkey"));
    }
    if !ts_ok(ts) {
        return Err(TransferError::bad_code("配对请求时间戳过期"));
    }
    let pubkey = from_hex(pubkey_hex).ok_or_else(|| TransferError::proto("pubkey 非法"))?;
    let sig = from_hex(sig_hex).ok_or_else(|| TransferError::proto("sig 非法"))?;
    let payload = pair_client_payload(code, peer_id, pubkey_hex, ts);
    if !verify(&pubkey, payload.as_bytes(), &sig) {
        return Err(TransferError::bad_code("配对签名验证失败"));
    }

    if !shared
        .pairing
        .lock()
        .map(|mut p| p.consume(code))
        .unwrap_or(false)
    {
        return Err(TransferError::bad_code("配对码无效或已过期"));
    }

    let fingerprint = fingerprint_of(&pubkey);
    if let Some(pinned) = shared.catalog.peer_fingerprint(peer_id) {
        if pinned != fingerprint {
            return Err(TransferError::fingerprint_mismatch(format!(
                "对端 {peer_id} 指纹与已登记不一致：已 pin {pinned}，本次 {fingerprint}"
            )));
        }
    }

    let addr = format!("{peer_ip}:*");
    shared
        .catalog
        .upsert_peer(peer_id, name, platform, &fingerprint, pubkey_hex, Some(&addr))
        .map_err(|e| TransferError::io(format!("登记对端失败：{e}")))?;

    let resp_ts = now_ts();
    let my_pk = to_hex(&shared.cfg.identity.public_key());
    let resp_payload = pair_server_payload(&shared.cfg.device_id, &my_pk, resp_ts);
    let resp_sig = to_hex(&shared.cfg.identity.sign(resp_payload.as_bytes()));
    Ok(json_ok(serde_json::json!({
        "device_id": shared.cfg.device_id,
        "name": shared.cfg.name,
        "platform": shared.cfg.platform,
        "pubkey": my_pk,
        "fingerprint": shared.cfg.identity.fingerprint(),
        "ts": resp_ts,
        "sig": resp_sig,
    })))
}

/// 验证传输请求签名头（`/blob` 与 `/op/*`）。
fn auth_headers(shared: &Shared, req: &Request, sign_payload: &str) -> Result<(), TransferError> {
    let device = req
        .header("X-Rm-Device")
        .ok_or_else(|| TransferError::forbidden("缺 X-Rm-Device"))?;
    let ts: i64 = req
        .header("X-Rm-Ts")
        .and_then(|v| v.parse().ok())
        .ok_or_else(|| TransferError::forbidden("缺/非法 X-Rm-Ts"))?;
    let sig_hex = req
        .header("X-Rm-Sig")
        .ok_or_else(|| TransferError::forbidden("缺 X-Rm-Sig"))?;
    if !ts_ok(ts) {
        return Err(TransferError::forbidden("时间戳超出 ±300 秒窗口"));
    }
    let pubkey_hex = shared
        .catalog
        .peer_pubkey(device)
        .ok_or_else(|| TransferError::forbidden("未配对设备"))?;
    let pinned = shared
        .catalog
        .peer_fingerprint(device)
        .ok_or_else(|| TransferError::forbidden("未配对设备"))?;
    let pubkey =
        from_hex(&pubkey_hex).ok_or_else(|| TransferError::forbidden("对端公钥损坏"))?;
    if fingerprint_of(&pubkey) != pinned {
        return Err(TransferError::fingerprint_mismatch(format!(
            "对端 {device} 公钥与已 pin 指纹不一致"
        )));
    }
    let sig = from_hex(sig_hex).ok_or_else(|| TransferError::forbidden("签名格式非法"))?;
    if !verify(&pubkey, sign_payload.as_bytes(), &sig) {
        return Err(TransferError::forbidden("签名验证失败"));
    }
    Ok(())
}

/// 流式提供 `/blob/{hash}?offset=&len=`（`docs/03` §5.2 块帧）。
fn blob_stream(
    shared: &Shared,
    req: &Request,
    stream: &mut TcpStream,
) -> Result<(), TransferError> {
    match blob_inner(shared, req, stream) {
        Ok(()) => Ok(()),
        Err(e) => write_error(stream, &e),
    }
}

fn blob_inner(shared: &Shared, req: &Request, stream: &mut TcpStream) -> Result<(), TransferError> {
    use std::io::{Read, Seek, SeekFrom};
    let hash = req.path.trim_start_matches("/blob/");
    if hash.len() != 64 || !hash.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(TransferError::proto("内容指纹必须是 64 位十六进制"));
    }
    let offset: u64 = req
        .query_param("offset")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let len: u64 = req.query_param("len").and_then(|v| v.parse().ok()).unwrap_or(0);

    let ts: i64 = req.header("X-Rm-Ts").and_then(|v| v.parse().ok()).unwrap_or(0);
    auth_headers(shared, req, &blob_sign_payload(hash, offset, len, ts))?;

    let paths = shared
        .catalog
        .serving_paths(hash)
        .map_err(|e| TransferError::io(e))?;
    let (_, path, _) = paths
        .first()
        .ok_or_else(|| TransferError::forbidden("冷备份或无在线副本，不作为数据源"))?;

    let file_len = std::fs::metadata(path)
        .map(|m| m.len())
        .map_err(|e| TransferError::io(format!("读源文件大小失败 {path}：{e}")))?;
    if offset > file_len {
        return Err(TransferError::proto("offset 超出文件长度"));
    }
    let end = if len == 0 {
        file_len
    } else {
        (offset + len).min(file_len)
    };
    let want = end - offset;

    let mut file = std::fs::File::open(path)
        .map_err(|e| TransferError::io(format!("打开源文件 {path}：{e}")))?;
    file.seek(SeekFrom::Start(offset))
        .map_err(|e| TransferError::io(format!("定位源文件失败：{e}")))?;

    let head = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/octet-stream\r\nContent-Length: {}\r\nX-Rm-Total: {file_len}\r\nConnection: close\r\n\r\n",
        framed_len(want)
    );
    stream
        .write_all(head.as_bytes())
        .map_err(|e| TransferError::io(format!("写响应头失败：{e}")))?;

    let mut remaining = want;
    let mut buf = vec![0u8; BLOCK_SIZE];
    while remaining > 0 {
        let chunk = remaining.min(BLOCK_SIZE as u64) as usize;
        file.read_exact(&mut buf[..chunk])
            .map_err(|e| TransferError::io(format!("读源文件失败：{e}")))?;
        let digest = blake3::hash(&buf[..chunk]);
        stream
            .write_all(&(chunk as u32).to_le_bytes())
            .and_then(|_| stream.write_all(digest.as_bytes()))
            .and_then(|_| stream.write_all(&buf[..chunk]))
            .map_err(|e| TransferError::io(format!("写块帧失败：{e}")))?;
        remaining -= chunk as u64;
    }
    stream
        .flush()
        .map_err(|e| TransferError::io(format!("flush 失败：{e}")))?;
    Ok(())
}

/// 块帧总长（头部 36 字节 × 块数 + 数据）。
pub fn framed_len(want: u64) -> u64 {
    let blocks = want.div_ceil(BLOCK_SIZE as u64);
    blocks * (BLOCK_HEADER_LEN as u64) + want
}

fn op_export(shared: &Shared, req: &Request) -> RouteResult {
    let ts: i64 = req.header("X-Rm-Ts").and_then(|v| v.parse().ok()).unwrap_or(0);
    auth_headers(shared, req, &op_sign_payload("export", b"", ts))?;
    let after: i64 = req
        .query_param("after")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let limit: u32 = req
        .query_param("limit")
        .and_then(|v| v.parse().ok())
        .unwrap_or(1000);
    let ops = shared
        .catalog
        .export_ops(after, limit.min(1000))
        .map_err(|e| TransferError::io(e))?;
    Ok(json_ok(serde_json::json!({"ops": ops, "count": ops.len()})))
}

fn op_import(shared: &Shared, req: &Request) -> RouteResult {
    let ts: i64 = req.header("X-Rm-Ts").and_then(|v| v.parse().ok()).unwrap_or(0);
    auth_headers(shared, req, &op_sign_payload("import", &req.body, ts))?;
    let device = req.header("X-Rm-Device").unwrap_or("").to_string();
    let body: serde_json::Value = serde_json::from_slice(&req.body)
        .map_err(|_| TransferError::proto("op 报文不是合法 JSON"))?;
    let ops = body["ops"]
        .as_array()
        .cloned()
        .ok_or_else(|| TransferError::proto("op 报文缺 ops 数组"))?;
    let (applied, skipped, conflicts) = shared
        .catalog
        .import_ops(&device, ops)
        .map_err(|e| TransferError::io(e))?;
    Ok(json_ok(serde_json::json!({
        "applied": applied, "skipped": skipped, "conflicts": conflicts
    })))
}

/// 客户端侧：请求签名头组装（供 client/op_client 复用）。
pub fn sign_headers(
    identity: &Identity,
    device_id: &str,
    payload: &str,
) -> Vec<(&'static str, String)> {
    let ts = now_ts();
    let sig = to_hex(&identity.sign(payload.as_bytes()));
    vec![
        ("X-Rm-Device", device_id.to_string()),
        ("X-Rm-Ts", ts.to_string()),
        ("X-Rm-Sig", sig),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pairing_code_lifecycle() {
        let mut p = PairingState::default();
        let (code, ttl) = p.issue();
        assert_eq!(code.len(), 8);
        assert_eq!(ttl as i64, protocol_pairing_ttl());
        assert!(p.consume(&code), "首次应消费成功");
        assert!(!p.consume(&code), "一次性：二次失败");
    }

    #[test]
    fn framed_len_matches_blocks() {
        assert_eq!(framed_len(0), 0);
        assert_eq!(framed_len(10), BLOCK_HEADER_LEN as u64 + 10);
        assert_eq!(
            framed_len(BLOCK_SIZE as u64 + 1),
            2 * BLOCK_HEADER_LEN as u64 + BLOCK_SIZE as u64 + 1
        );
    }
}
