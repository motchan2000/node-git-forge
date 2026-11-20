//! HTTP/1.1 over std::net: one thread per connection, keep-alive, pipelining, Content-Length and
//! chunked request bodies, Expect: 100-continue, and every response written in one call.

use crate::json::Json;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

pub struct Request {
    pub method: String,
    pub path: String,
    pub query: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Request {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str())
    }
    /// A query parameter, decoded.
    pub fn param(&self, name: &str) -> Option<String> {
        for pair in self.query.split('&') {
            let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
            if crate::util::query_decode(k) == name {
                return Some(crate::util::query_decode(v));
            }
        }
        None
    }
}

pub struct Response {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

static REQUEST_ID: AtomicU64 = AtomicU64::new(0);

pub fn request_id() -> String {
    let n = REQUEST_ID.fetch_add(1, Ordering::Relaxed) + 1;
    format!("F0RGE:{:08X}", n)
}

impl Response {
    pub fn json_text(status: u16, text: String) -> Response {
        Response {
            status,
            headers: vec![
                ("content-type".into(), "application/json; charset=utf-8".into()),
                ("x-github-request-id".into(), request_id()),
                ("x-github-media-type".into(), "github.v3; format=json".into()),
            ],
            body: if status == 204 { Vec::new() } else { text.into_bytes() },
        }
    }
    pub fn json(status: u16, value: &Json) -> Response {
        Response::json_text(status, if status == 204 { String::new() } else { value.to_string() })
    }
    pub fn error(status: u16, message: &str) -> Response {
        Response::json(
            status,
            &crate::jobj! { "message" => message, "documentation_url" => "https://docs.github.com/rest", "status" => status.to_string() },
        )
    }
    pub fn bytes(status: u16, content_type: &str, body: Vec<u8>) -> Response {
        Response { status, headers: vec![("content-type".into(), content_type.into())], body }
    }
    pub fn with_header(mut self, name: &str, value: &str) -> Response {
        self.headers.push((name.to_string(), value.to_string()));
        self
    }
}

fn reason(status: u16) -> &'static str {
    match status {
        100 => "Continue",
        200 => "OK",
        201 => "Created",
        202 => "Accepted",
        204 => "No Content",
        304 => "Not Modified",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        409 => "Conflict",
        413 => "Payload Too Large",
        422 => "Unprocessable Entity",
        431 => "Request Header Fields Too Large",
        500 => "Internal Server Error",
        503 => "Service Unavailable",
        _ => "Unknown",
    }
}

pub type Handler = Arc<dyn Fn(&Request) -> Response + Send + Sync>;

const MAX_HEADER: usize = 64 * 1024;
const MAX_BODY: usize = 512 * 1024 * 1024;
const MAX_THREADS: usize = 200;

static ACTIVE: AtomicUsize = AtomicUsize::new(0);

pub fn serve(listener: TcpListener, handler: Handler) {
    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        if ACTIVE.load(Ordering::Relaxed) >= MAX_THREADS {
            let mut s = stream;
            let _ = s.write_all(b"HTTP/1.1 503 Service Unavailable\r\ncontent-length: 0\r\nconnection: close\r\n\r\n");
            continue;
        }
        ACTIVE.fetch_add(1, Ordering::Relaxed);
        let h = handler.clone();
        let spawned = std::thread::Builder::new().stack_size(4 << 20).spawn(move || {
            let _ = connection(stream, &h);
            ACTIVE.fetch_sub(1, Ordering::Relaxed);
        });
        if spawned.is_err() {
            ACTIVE.fetch_sub(1, Ordering::Relaxed);
        }
    }
}

fn find(hay: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    if hay.len() < needle.len() {
        return None;
    }
    (from..=hay.len() - needle.len()).find(|&i| &hay[i..i + needle.len()] == needle)
}

fn simple(stream: &mut TcpStream, status: u16) {
    let _ = stream.write_all(format!("HTTP/1.1 {} {}\r\ncontent-type: application/json\r\ncontent-length: 2\r\nconnection: close\r\n\r\n{{}}", status, reason(status)).as_bytes());
}

fn connection(mut stream: TcpStream, handler: &Handler) -> std::io::Result<()> {
    let _ = stream.set_nodelay(true);
    let _ = stream.set_read_timeout(Some(Duration::from_secs(300)));
    let mut buf: Vec<u8> = Vec::with_capacity(16 * 1024);
    let mut chunk = vec![0u8; 64 * 1024];
    loop {
        // Headers.
        let head_end = loop {
            if let Some(i) = find(&buf, b"\r\n\r\n", 0) {
                break i;
            }
            if buf.len() > MAX_HEADER {
                simple(&mut stream, 431);
                return Ok(());
            }
            let n = stream.read(&mut chunk)?;
            if n == 0 {
                return Ok(());
            }
            buf.extend_from_slice(&chunk[..n]);
        };
        let head = String::from_utf8_lossy(&buf[..head_end]).into_owned();
        let mut lines = head.split("\r\n");
        let request_line = lines.next().unwrap_or("");
        let parts: Vec<&str> = request_line.split(' ').collect();
        if parts.len() != 3 || !parts[2].starts_with("HTTP/1.") || parts[0].is_empty() {
            simple(&mut stream, 400);
            return Ok(());
        }
        let http10 = parts[2] == "HTTP/1.0";
        let mut headers: Vec<(String, String)> = Vec::new();
        for line in lines {
            let Some((k, v)) = line.split_once(':') else {
                simple(&mut stream, 400);
                return Ok(());
            };
            let k = k.trim().to_ascii_lowercase();
            let v = v.trim().to_string();
            if let Some(slot) = headers.iter_mut().find(|(name, _)| *name == k) {
                slot.1 = format!("{}, {}", slot.1, v);
            } else {
                headers.push((k, v));
            }
        }
        buf.drain(..head_end + 4);
        let get = |name: &str| headers.iter().find(|(k, _)| k == name).map(|(_, v)| v.clone());
        let connection_hdr = get("connection").unwrap_or_default().to_ascii_lowercase();
        let keep_alive = if http10 { connection_hdr.contains("keep-alive") } else { !connection_hdr.contains("close") };

        // Body.
        let mut sent_continue = false;
        let expect_continue = get("expect").map(|v| v.eq_ignore_ascii_case("100-continue")).unwrap_or(false);
        let chunked = get("transfer-encoding").map(|v| v.to_ascii_lowercase().contains("chunked")).unwrap_or(false);
        let body: Vec<u8> = if chunked {
            loop {
                match decode_chunked(&buf) {
                    Err(()) => {
                        simple(&mut stream, 400);
                        return Ok(());
                    }
                    Ok(Some((body, used))) => {
                        buf.drain(..used);
                        break body;
                    }
                    Ok(None) => {
                        if buf.len() > MAX_BODY {
                            simple(&mut stream, 413);
                            return Ok(());
                        }
                        if expect_continue && !sent_continue {
                            sent_continue = true;
                            stream.write_all(b"HTTP/1.1 100 Continue\r\n\r\n")?;
                        }
                        let n = stream.read(&mut chunk)?;
                        if n == 0 {
                            return Ok(());
                        }
                        buf.extend_from_slice(&chunk[..n]);
                    }
                }
            }
        } else {
            let len = match get("content-length") {
                None => 0,
                Some(v) => match v.trim().parse::<usize>() {
                    Ok(n) => n,
                    Err(_) => {
                        simple(&mut stream, 400);
                        return Ok(());
                    }
                },
            };
            if len > MAX_BODY {
                simple(&mut stream, 413);
                return Ok(());
            }
            if buf.len() < len && expect_continue {
                stream.write_all(b"HTTP/1.1 100 Continue\r\n\r\n")?;
            }
            if buf.len() < len {
                buf.reserve(len - buf.len());
            }
            while buf.len() < len {
                let n = stream.read(&mut chunk)?;
                if n == 0 {
                    return Ok(());
                }
                buf.extend_from_slice(&chunk[..n]);
            }
            buf.drain(..len).collect()
        };

        let (path_raw, query) = match parts[1].split_once('?') {
            Some((p, q)) => (p.to_string(), q.to_string()),
            None => (parts[1].to_string(), String::new()),
        };
        let req = Request { method: parts[0].to_string(), path: path_raw, query, headers, body };
        let head_only = req.method == "HEAD";
        let res = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| handler(&req))) {
            Ok(r) => r,
            Err(_) => Response::error(500, "Internal Server Error"),
        };

        // One write: status line, headers, body.
        let no_body = res.status == 204 || res.status == 304;
        let mut out = Vec::with_capacity(256 + res.body.len());
        out.extend_from_slice(format!("HTTP/1.1 {} {}\r\n", res.status, reason(res.status)).as_bytes());
        for (k, v) in &res.headers {
            out.extend_from_slice(k.as_bytes());
            out.extend_from_slice(b": ");
            out.extend_from_slice(v.as_bytes());
            out.extend_from_slice(b"\r\n");
        }
        if !no_body {
            out.extend_from_slice(format!("content-length: {}\r\n", res.body.len()).as_bytes());
        }
        if !keep_alive {
            out.extend_from_slice(b"connection: close\r\n");
        }
        out.extend_from_slice(b"\r\n");
        if !no_body && !head_only {
            out.extend_from_slice(&res.body);
        }
        stream.write_all(&out)?;
        if !keep_alive {
            let _ = stream.shutdown(std::net::Shutdown::Write);
            return Ok(());
        }
    }
}

/// A complete chunked body at the start of `buf`: Ok(Some((body, used))), Ok(None) while incomplete.
fn decode_chunked(buf: &[u8]) -> Result<Option<(Vec<u8>, usize)>, ()> {
    let mut body = Vec::new();
    let mut i = 0;
    loop {
        let Some(eol) = find(buf, b"\r\n", i) else { return Ok(None) };
        let line = std::str::from_utf8(&buf[i..eol]).map_err(|_| ())?;
        let size_text = line.split(';').next().unwrap_or("").trim();
        let size = usize::from_str_radix(size_text, 16).map_err(|_| ())?;
        i = eol + 2;
        if size == 0 {
            // optional trailers, then an empty line
            let Some(end) = find(buf, b"\r\n\r\n", i - 2) else { return Ok(None) };
            return Ok(Some((body, end + 4)));
        }
        if buf.len() < i + size + 2 {
            return Ok(None);
        }
        body.extend_from_slice(&buf[i..i + size]);
        i += size + 2;
    }
}
