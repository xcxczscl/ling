//! 流交付服务（`docs/03` §2.6；契约冻结，此处为契约落地实现，不改设计）。
//!
//! - 回环 HTTP（`127.0.0.1`，基端口 7913，占用顺延；跳过 7914/7915——player 保留）
//! - 128-bit 内存态 token：不落盘、只绑一个 placement、idle 10 分钟过期
//! - `GET`/`HEAD` + `Range`（206）；每次成功请求刷新空闲计时
//! - 放置/位置状态**每次请求实时查**，离线/删除立即断流（403，固定文案）
//! - 压缩内容（`enc=zstd`）按需解压，对消费者透明（Range 走解压+跳过）

use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::net::{Ipv4Addr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rusqlite::Connection;

/// 契约示例端口（`docs/03` §2.6.1：7913；URL 以实际返回为准）。
const PREFERRED_PORT: u16 = 7913;
/// 7913 被占时的退避段：避开 LAN 传输面 7912–7931 与 player 保留 7914/7915。
const FALLBACK_BASE: u16 = 7932;
/// 退避段长度。
const FALLBACK_SPAN: u16 = 20;
/// player 保留端口（模式 A 代理 7914 / lite 接收器 7915），任何段都不占。
const RESERVED_PORTS: [u16; 2] = [7914, 7915];
/// 空闲过期（契约 §2.6.1：600 秒）。
const IDLE_TTL: Duration = Duration::from_secs(600);
/// 403 固定文案（不泄露放置信息）。
const FORBIDDEN_BODY: &str = "F502 invalid or expired token";

struct TokenEntry {
    placement_id: i64,
    last_active: Instant,
}

/// 从 DB 实时取回的放置服务信息。
struct Serving {
    state: String,
    enc: String,
    /// 逻辑长度（plain：文件实际长；zstd：content.size）。
    logical_len: u64,
    path: PathBuf,
}

struct Head {
    method: String,
    path: String,
    range: Option<String>,
}

/// 流服务句柄：持有端口、token 表与服务线程；Drop 即停。
pub struct StreamHandle {
    port: u16,
    tokens: Arc<Mutex<HashMap<String, TokenEntry>>>,
    stop: Arc<AtomicBool>,
    join: Option<std::thread::JoinHandle<()>>,
}

impl StreamHandle {
    /// 绑回环端口并启动服务线程：先试 7913（契约示例），被占则退 7932+。
    /// 全部占用 → Err（上层转 F504）。
    pub fn start(db_path: PathBuf) -> Result<Self, String> {
        let mut listener: Option<TcpListener> = None;
        let mut bound = 0u16;
        let candidates = std::iter::once(PREFERRED_PORT)
            .chain(FALLBACK_BASE..FALLBACK_BASE + FALLBACK_SPAN);
        for port in candidates {
            if RESERVED_PORTS.contains(&port) {
                continue;
            }
            if let Ok(l) = TcpListener::bind((Ipv4Addr::LOCALHOST, port)) {
                listener = Some(l);
                bound = port;
                break;
            }
        }
        let listener = listener.ok_or_else(|| {
            format!(
                "端口 {PREFERRED_PORT} 与 {FALLBACK_BASE}–{} 均被占用",
                FALLBACK_BASE + FALLBACK_SPAN - 1
            )
        })?;

        let tokens: Arc<Mutex<HashMap<String, TokenEntry>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let stop = Arc::new(AtomicBool::new(false));

        // 服务线程自持一条 DB 连接（WAL 读与写并发安全；每请求实时查状态）。
        let conn = Connection::open(&db_path)
            .map_err(|e| format!("流服务打开目录库失败 {db_path:?}：{e}"))?;
        let _ = conn.busy_timeout(Duration::from_secs(5));
        let db_tokens = Arc::clone(&tokens);
        let db_stop = Arc::clone(&stop);
        let conn = Arc::new(Mutex::new(conn));
        let join = std::thread::Builder::new()
            .name("rm-stream-server".into())
            .spawn(move || {
                for stream in listener.incoming() {
                    if db_stop.load(Ordering::SeqCst) {
                        break;
                    }
                    let Ok(stream) = stream else { continue };
                    let tokens = Arc::clone(&db_tokens);
                    let conn = Arc::clone(&conn);
                    let stop = Arc::clone(&db_stop);
                    std::thread::spawn(move || {
                        let _ = handle_conn(stream, &tokens, &conn, &stop);
                    });
                }
            })
            .map_err(|e| format!("启动流服务线程失败：{e}"))?;

        Ok(Self { port: bound, tokens, stop, join: Some(join) })
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    /// 铸造 token：绑一个 placement，返回 (stream_id, token)。
    pub fn mint(&self, placement_id: i64) -> (String, String) {
        let mut id_raw = [0u8; 8];
        let mut tok_raw = [0u8; 16];
        let _ = getrandom::getrandom(&mut id_raw);
        let _ = getrandom::getrandom(&mut tok_raw);
        let stream_id = format!("st_{}", hex(&id_raw));
        let token = hex(&tok_raw);
        let key = format!("{stream_id}/{token}");
        self.tokens
            .lock()
            .unwrap()
            .insert(key, TokenEntry { placement_id, last_active: Instant::now() });
        (stream_id, token)
    }

    /// 契约 URL 形态（`docs/03` §2.6.1）。
    pub fn url_for(&self, stream_id: &str, token: &str) -> String {
        format!("http://127.0.0.1:{}/s/{stream_id}/{token}", self.port)
    }

    pub fn stop(mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect((Ipv4Addr::LOCALHOST, self.port));
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

impl Drop for StreamHandle {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect((Ipv4Addr::LOCALHOST, self.port));
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// 单连接：鉴权 → 实时查状态 → HEAD/GET + Range。
fn handle_conn(
    mut stream: TcpStream,
    tokens: &Mutex<HashMap<String, TokenEntry>>,
    conn: &Mutex<Connection>,
    stop: &AtomicBool,
) -> std::io::Result<()> {
    // 仅回环（契约 §2.6.3）。
    if let Ok(peer) = stream.peer_addr() {
        if !peer.ip().is_loopback() {
            return write_forbidden(&mut stream);
        }
    }

    let head = match read_head(&mut stream) {
        Ok(h) => h,
        Err(_) => return write_forbidden(&mut stream),
    };

    // 路径：/s/{stream_id}/{token}
    let Some(rest) = head.path.strip_prefix("/s/") else {
        return write_forbidden(&mut stream);
    };
    let key = rest.to_string();
    if key.matches('/').count() != 1 || key.is_empty() {
        return write_forbidden(&mut stream);
    }

    // token 查验 + 过期清扫；通过则刷新空闲计时（契约：活跃即续期）。
    let placement_id = {
        let mut map = tokens.lock().unwrap();
        map.retain(|_, e| e.last_active.elapsed() <= IDLE_TTL);
        match map.get_mut(&key) {
            None => return write_forbidden(&mut stream),
            Some(e) => {
                e.last_active = Instant::now();
                e.placement_id
            }
        }
    };
    let _ = stop;

    // 实时查放置/位置状态（契约 §2.6.2：每次请求查，离线立即断流）。
    let serving = {
        let c = conn.lock().unwrap();
        lookup_serving(&c, placement_id)
    };
    let Some(s) = serving else {
        return write_forbidden(&mut stream);
    };
    if s.state != "online" {
        return write_forbidden(&mut stream);
    }

    serve(&mut stream, &head, &s)
}

/// 查放置实时状态 + 解析绝对路径。
fn lookup_serving(conn: &Connection, placement_id: i64) -> Option<Serving> {
    let row = conn
        .query_row(
            "SELECT p.state, l.root_path, p.rel_path, l.state, c.enc, c.size
             FROM placement p
             JOIN location l ON l.id = p.location_id
             JOIN content c ON c.blake3 = p.content_id
             WHERE p.id = ?1",
            rusqlite::params![placement_id],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, String>(4)?,
                    r.get::<_, i64>(5)?,
                ))
            },
        )
        .ok()?;
    let (state, root_path, rel_path, loc_state, enc, content_size) = row;
    if loc_state != "online" {
        return None;
    }
    let path = std::path::Path::new(&root_path).join(&rel_path);
    let physical = std::fs::metadata(&path).ok()?.len();
    let logical_len = if enc == "plain" {
        // plain：以盘上文件为准（Range 数学必须与可读字节一致）。
        physical
    } else {
        // zstd：逻辑长度以 content.size 为准。
        content_size.max(0) as u64
    };
    Some(Serving { state, enc, logical_len, path })
}

/// HEAD/GET + Range（契约 §2.6.2）。
fn serve(stream: &mut TcpStream, head: &Head, s: &Serving) -> std::io::Result<()> {
    let total = s.logical_len;

    // HEAD：镜像全流 GET 的响应头（Content-Length = total），不带 body。
    if head.method == "HEAD" {
        let header = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/octet-stream\r\nContent-Length: {total}\r\nAccept-Ranges: bytes\r\nConnection: close\r\n\r\n"
        );
        stream.write_all(header.as_bytes())?;
        return stream.flush();
    }

    // GET：解析 Range（仅 bytes 单区间；后缀/多区间 P0 不支持 → 整流）。
    let range = head.range.as_deref().and_then(parse_range);
    let (status, start, end) = match range {
        Some((a, end_opt)) => {
            if total == 0 || a >= total {
                return write_416(stream, total);
            }
            let end = end_opt.unwrap_or(u64::MAX).min(total - 1);
            if a > end {
                return write_416(stream, total);
            }
            ("206 Partial Content", a, end)
        }
        None => {
            if total == 0 {
                ("200 OK", 0u64, 0u64)
            } else {
                ("200 OK", 0u64, total - 1)
            }
        }
    };
    let body_len = if total == 0 { 0 } else { end - start + 1 };

    let mut header = format!(
        "HTTP/1.1 {status}\r\nContent-Type: application/octet-stream\r\nContent-Length: {body_len}\r\nAccept-Ranges: bytes\r\nConnection: close\r\n"
    );
    if status.starts_with("206") {
        header.push_str(&format!("Content-Range: bytes {start}-{end}/{total}\r\n"));
    }
    header.push_str("\r\n");
    stream.write_all(header.as_bytes())?;

    if body_len == 0 {
        return stream.flush();
    }

    // 按编码取 [start, end] 字节。
    if s.enc == "plain" {
        let mut file = File::open(&s.path)?;
        file.seek(SeekFrom::Start(start))?;
        std::io::copy(&mut (&mut file).take(body_len), stream)?;
    } else {
        // zstd：从头解压，跳过 start，取 body_len（解压+跳过，O(start)）。
        let file = File::open(&s.path)?;
        let mut dec = zstd::stream::read::Decoder::new(file)?;
        if start > 0 {
            let mut sink = std::io::sink();
            let skipped = std::io::copy(&mut (&mut dec).take(start), &mut sink)?;
            if skipped < start {
                return stream.flush();
            }
        }
        std::io::copy(&mut (&mut dec).take(body_len), stream)?;
    }
    stream.flush()
}

fn write_416(stream: &mut TcpStream, total: u64) -> std::io::Result<()> {
    let resp = format!(
        "HTTP/1.1 416 Range Not Satisfiable\r\nContent-Range: bytes */{total}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
    );
    stream.write_all(resp.as_bytes())?;
    stream.flush()
}

/// 解析 `bytes=a-b` / `bytes=a-` → (start, end_opt)；后缀/多区间/畸形 → None（整流）。
fn parse_range(v: &str) -> Option<(u64, Option<u64>)> {
    let v = v.trim();
    let rest = v.strip_prefix("bytes=")?;
    if rest.contains(',') {
        return None;
    }
    let (a, b) = rest.split_once('-')?;
    if a.is_empty() {
        return None;
    }
    let start: u64 = a.parse().ok()?;
    if b.is_empty() {
        return Some((start, None));
    }
    let end: u64 = b.parse().ok()?;
    Some((start, Some(end)))
}

/// 读请求行与头部（连即用即弃，不读 body——本服务只收 GET/HEAD）。
fn read_head(stream: &TcpStream) -> std::io::Result<Head> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut line = String::new();
    reader.read_line(&mut line)?;
    let mut parts = line.split_whitespace();
    let method = parts.next().unwrap_or("").to_string();
    let target = parts.next().unwrap_or("").to_string();
    let path = target.split('?').next().unwrap_or("").to_string();

    let mut range = None;
    loop {
        line.clear();
        reader.read_line(&mut line)?;
        let t = line.trim_end_matches(['\r', '\n']);
        if t.is_empty() {
            break;
        }
        if let Some((k, v)) = t.split_once(':') {
            if k.eq_ignore_ascii_case("range") {
                range = Some(v.trim().to_string());
            }
        }
    }
    if method != "GET" && method != "HEAD" {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "method"));
    }
    Ok(Head { method, path, range })
}

fn write_forbidden(stream: &mut TcpStream) -> std::io::Result<()> {
    let resp = format!(
        "HTTP/1.1 403 Forbidden\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        FORBIDDEN_BODY.len(),
        FORBIDDEN_BODY
    );
    stream.write_all(resp.as_bytes())?;
    stream.flush()
}
