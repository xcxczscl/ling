//! 极简 HTTP/1.1 服务端读写（P3 模式 A 代理 + lite 接收器控制面 +
//! P7 控制平面（`control.rs`）共用；手写解析不引 web 框架依赖——只需
//! GET/POST + JSON 与流式转发几种形态）。

use std::collections::HashMap;
use std::io::{BufRead, Write};

/// 已解析的请求行 + 头部（body 由调用方按 Content-Length 另读）。
pub struct Request {
    pub method: String,
    pub path: String,
    pub headers: HashMap<String, String>,
}

impl Request {
    /// 大小写不敏感取头。
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(&name.to_ascii_lowercase()).map(|s| s.as_str())
    }

    pub fn content_length(&self) -> usize {
        self.header("content-length").and_then(|v| v.parse().ok()).unwrap_or(0)
    }
}

/// 单请求上限：代理/lite 控制面都是小请求（投屏 JSON body ≤ 64KB）。
const MAX_HEAD: usize = 16 * 1024;
pub const MAX_BODY: usize = 64 * 1024;

/// 读一个请求；对端关闭 → `None`。
pub fn read_request<R: BufRead>(rd: &mut R) -> std::io::Result<Option<Request>> {
    let mut head = Vec::with_capacity(256);
    loop {
        let mut line = Vec::new();
        let n = rd.read_until(b'\n', &mut line)?;
        if n == 0 {
            if head.is_empty() {
                return Ok(None);
            }
            return Err(std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "head 截断"));
        }
        head.extend_from_slice(&line);
        if head.len() > MAX_HEAD {
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "head 过大"));
        }
        if line == b"\r\n" || line == b"\n" {
            break;
        }
    }
    parse_head(&head).map(Some)
}

/// 解析请求头（单元测试直接喂字节）。
pub fn parse_head(bytes: &[u8]) -> std::io::Result<Request> {
    let text = std::str::from_utf8(bytes)
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidData, "非 UTF-8 头"))?;
    let mut lines = text.split("\r\n").filter(|l| !l.is_empty());
    let req_line = lines
        .next()
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidData, "空请求"))?;
    let mut parts = req_line.split(' ');
    let method = parts
        .next()
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidData, "缺方法"))?
        .to_string();
    let path = parts
        .next()
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidData, "缺路径"))?
        .to_string();
    let mut headers = HashMap::new();
    for line in lines {
        if let Some((k, v)) = line.split_once(':') {
            headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_string());
        }
    }
    Ok(Request { method, path, headers })
}

/// 读满 `len` 字节 body。
pub fn read_body<R: BufRead>(rd: &mut R, len: usize) -> std::io::Result<Vec<u8>> {
    if len > MAX_BODY {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "body 过大"));
    }
    let mut buf = vec![0u8; len];
    rd.read_exact(&mut buf)?;
    Ok(buf)
}

/// 状态行原因短语。
pub fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        400 => "Bad Request",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        406 => "Not Acceptable",
        411 => "Length Required",
        413 => "Payload Too Large",
        500 => "Internal Server Error",
        502 => "Bad Gateway",
        _ => "Error",
    }
}

/// 写状态行 + 头（`Connection: close` 语义交调用方在 headers 里给）。
pub fn write_head<W: Write>(w: &mut W, status: u16, headers: &[(&str, &str)]) -> std::io::Result<()> {
    let mut out = format!("HTTP/1.1 {status} {}\r\n", reason(status));
    for (k, v) in headers {
        out.push_str(k);
        out.push_str(": ");
        out.push_str(v);
        out.push_str("\r\n");
    }
    out.push_str("\r\n");
    w.write_all(out.as_bytes())
}

/// 写完整小响应（JSON/纯文本）。
pub fn write_response<W: Write>(
    w: &mut W,
    status: u16,
    content_type: &str,
    body: &[u8],
) -> std::io::Result<()> {
    write_head(
        w,
        status,
        &[
            ("Content-Type", content_type),
            ("Content-Length", &body.len().to_string()),
            ("Connection", "close"),
        ],
    )?;
    w.write_all(body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn t1_parse_head() {
        let raw = b"GET /s/st_1/deadbeef HTTP/1.1\r\nHost: a\r\nRANGE: bytes=0-9\r\n\r\n";
        let req = parse_head(raw).unwrap();
        assert_eq!(req.method, "GET");
        assert_eq!(req.path, "/s/st_1/deadbeef");
        assert_eq!(req.header("Host"), Some("a"));
        assert_eq!(req.header("range"), Some("bytes=0-9"));
        assert_eq!(req.content_length(), 0);
    }

    #[test]
    fn t2_read_request_eof_and_body() {
        // 正常请求 + body
        let mut rd = Cursor::new(
            &b"POST /cast HTTP/1.1\r\nContent-Length: 5\r\n\r\nhello"[..],
        );
        let req = read_request(&mut rd).unwrap().unwrap();
        assert_eq!(req.path, "/cast");
        assert_eq!(read_body(&mut rd, req.content_length()).unwrap(), b"hello");
        // 空读 → None（对端关闭）
        let mut rd = Cursor::new(&b""[..]);
        assert!(read_request(&mut rd).unwrap().is_none());
    }

    #[test]
    fn t3_write_response() {
        let mut buf: Vec<u8> = Vec::new();
        write_response(&mut buf, 200, "application/json", b"{}").unwrap();
        let text = String::from_utf8(buf).unwrap();
        assert!(text.starts_with("HTTP/1.1 200 OK\r\n"));
        assert!(text.contains("Content-Length: 2\r\n"));
        assert!(text.ends_with("\r\n\r\n{}"));
    }
}
