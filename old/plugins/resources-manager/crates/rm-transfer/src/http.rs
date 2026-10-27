//! 极简 HTTP/1.1 客户端与服务端原语（无第三方依赖，与 core-ui 同风格）。
//!
//! 只覆盖本插件需要的子集：`Content-Length` 定长 body（不用 chunked）、
//! 单次请求-响应、连接即用即弃。

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

use crate::error::TransferError;

/// 连接超时（秒）。
const CONNECT_TIMEOUT: u64 = 10;
/// 读写超时（秒）。
const IO_TIMEOUT: u64 = 60;

/// 解析后的请求。
pub struct Request {
    pub method: String,
    pub path: String,
    /// path 中 `?` 之后的部分（无则空串）。
    pub query: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Request {
    /// 取请求头（大小写不敏感）。
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    /// 取查询参数。
    pub fn query_param(&self, name: &str) -> Option<&str> {
        self.query.split('&').find_map(|pair| {
            let (k, v) = pair.split_once('=')?;
            (k == name).then_some(v)
        })
    }
}

/// 写出响应。
pub fn write_response(
    stream: &mut TcpStream,
    status: u16,
    content_type: &str,
    body: &[u8],
) -> std::io::Result<()> {
    let reason = match status {
        200 => "OK",
        206 => "Partial Content",
        400 => "Bad Request",
        403 => "Forbidden",
        404 => "Not Found",
        409 => "Conflict",
        _ => "Internal Server Error",
    };
    let head = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(head.as_bytes())?;
    stream.write_all(body)?;
    stream.flush()
}

/// 读一个请求；连接错误/超时 → Err。
pub fn read_request(stream: &TcpStream) -> Result<Request, TransferError> {
    let mut reader = BufReader::new(
        stream
            .try_clone()
            .map_err(|e| TransferError::io(format!("克隆连接失败：{e}")))?,
    );
    let mut line = String::new();
    reader
        .read_line(&mut line)
        .map_err(|e| TransferError::io(format!("读请求行失败：{e}")))?;
    let mut parts = line.split_whitespace();
    let method = parts.next().unwrap_or("").to_string();
    let target = parts.next().unwrap_or("").to_string();
    if method.is_empty() {
        return Err(TransferError::proto("空请求行"));
    }
    let (path, query) = match target.split_once('?') {
        Some((p, q)) => (p.to_string(), q.to_string()),
        None => (target, String::new()),
    };

    let mut headers = Vec::new();
    loop {
        line.clear();
        reader
            .read_line(&mut line)
            .map_err(|e| TransferError::io(format!("读请求头失败：{e}")))?;
        let line = line.trim_end_matches(['\r', '\n']);
        if line.is_empty() {
            break;
        }
        if let Some((k, v)) = line.split_once(':') {
            headers.push((k.trim().to_string(), v.trim().to_string()));
        }
    }
    let content_len: usize = headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("content-length"))
        .and_then(|(_, v)| v.parse().ok())
        .unwrap_or(0);
    let mut body = vec![0u8; content_len];
    if content_len > 0 {
        reader
            .read_exact(&mut body)
            .map_err(|e| TransferError::io(format!("读请求体失败：{e}")))?;
    }
    Ok(Request {
        method,
        path,
        query,
        headers,
        body,
    })
}

/// 发起一次 HTTP 请求并读完整响应体。返回 (status, body)。
pub fn http_bytes(
    addr: &str,
    method: &str,
    path: &str,
    headers: &[(&str, String)],
    body: Option<&[u8]>,
) -> Result<(u16, Vec<u8>), TransferError> {
    let mut stream = connect(addr)?;
    let mut head = format!("{method} {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n");
    for (k, v) in headers {
        head.push_str(&format!("{k}: {v}\r\n"));
    }
    let body_len = body.map(|b| b.len()).unwrap_or(0);
    head.push_str(&format!("Content-Length: {body_len}\r\n\r\n"));
    stream
        .write_all(head.as_bytes())
        .map_err(|e| TransferError::unreachable(format!("发送请求失败：{e}")))?;
    if let Some(b) = body {
        stream
            .write_all(b)
            .map_err(|e| TransferError::unreachable(format!("发送请求体失败：{e}")))?;
    }
    let (status, resp_body) = read_response(&mut stream)?;
    Ok((status, resp_body))
}

/// 发起请求，流式消费响应体（blob 用）：`expected_len` 为已知 body 长度，
/// 分块回调 `sink`，返回实际读取字节数。
pub fn http_stream(
    addr: &str,
    method: &str,
    path: &str,
    headers: &[(&str, String)],
    expected_len: u64,
    mut sink: impl FnMut(&[u8]) -> Result<(), TransferError>,
) -> Result<u64, TransferError> {
    let mut stream = connect(addr)?;
    let mut head = format!("{method} {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n");
    for (k, v) in headers {
        head.push_str(&format!("{k}: {v}\r\n"));
    }
    head.push_str("Content-Length: 0\r\n\r\n");
    stream
        .write_all(head.as_bytes())
        .map_err(|e| TransferError::unreachable(format!("发送请求失败：{e}")))?;

    let mut reader = BufReader::new(
        stream
            .try_clone()
            .map_err(|e| TransferError::io(format!("克隆连接失败：{e}")))?,
    );
    let status = read_status_and_headers(&mut reader)?;
    if status != 200 {
        let mut rest = Vec::new();
        let _ = reader.read_to_end(&mut rest);
        return Err(TransferError::unreachable(format!(
            "对端返回 HTTP {status}：{}",
            String::from_utf8_lossy(&rest[..rest.len().min(200)])
        )));
    }
    let mut remaining = expected_len;
    let mut buf = vec![0u8; 64 * 1024];
    let mut total = 0u64;
    while remaining > 0 {
        let want = buf.len().min(remaining as usize);
        let n = reader
            .read(&mut buf[..want])
            .map_err(|e| TransferError::unreachable(format!("读响应体失败：{e}")))?;
        if n == 0 {
            return Err(TransferError::unreachable(format!(
                "响应体提前结束：已读 {total}，预期 {expected_len}"
            )));
        }
        sink(&buf[..n])?;
        total += n as u64;
        remaining -= n as u64;
    }
    Ok(total)
}

fn connect(addr: &str) -> Result<TcpStream, TransferError> {
    let sock: std::net::SocketAddr = addr
        .to_socket_addrs()
        .map_err(|_| TransferError::unreachable(format!("地址不可解析：{addr}")))?
        .next()
        .ok_or_else(|| TransferError::unreachable(format!("地址无解析结果：{addr}")))?;
    let stream = TcpStream::connect_timeout(&sock, Duration::from_secs(CONNECT_TIMEOUT))
        .map_err(|e| TransferError::unreachable(format!("连接 {addr} 失败：{e}")))?;
    stream
        .set_read_timeout(Some(Duration::from_secs(IO_TIMEOUT)))
        .map_err(|e| TransferError::io(format!("设置读超时失败：{e}")))?;
    stream
        .set_write_timeout(Some(Duration::from_secs(IO_TIMEOUT)))
        .map_err(|e| TransferError::io(format!("设置写超时失败：{e}")))?;
    Ok(stream)
}

fn read_response(stream: &mut TcpStream) -> Result<(u16, Vec<u8>), TransferError> {
    let mut reader = BufReader::new(
        stream
            .try_clone()
            .map_err(|e| TransferError::io(format!("克隆连接失败：{e}")))?,
    );
    let status = read_status_and_headers(&mut reader)?;
    let mut body = Vec::new();
    reader
        .read_to_end(&mut body)
        .map_err(|e| TransferError::io(format!("读响应体失败：{e}")))?;
    Ok((status, body))
}

fn read_status_and_headers(reader: &mut impl BufRead) -> Result<u16, TransferError> {
    let mut line = String::new();
    reader
        .read_line(&mut line)
        .map_err(|e| TransferError::io(format!("读状态行失败：{e}")))?;
    let status: u16 = line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| TransferError::proto(format!("非法状态行：{}", line.trim())))?;
    loop {
        line.clear();
        reader
            .read_line(&mut line)
            .map_err(|e| TransferError::io(format!("读响应头失败：{e}")))?;
        if line.trim_end_matches(['\r', '\n']).is_empty() {
            break;
        }
    }
    Ok(status)
}
