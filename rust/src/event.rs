//! An epoll event loop serving HTTP/1.1: one thread, non-blocking sockets, every ready connection
//! handled per wake-up, and a short busy-poll (epoll_wait with a zero timeout) before blocking.
//! Under a sandboxing kernel, waking a thread is the dominant cost of a request; this design wakes
//! one thread for many requests. Requests are parsed exactly as http.rs parses them.

use crate::http::{reason, Handler, Request, Response};
use std::collections::HashMap;
use std::io::{ErrorKind, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::os::fd::AsRawFd;

#[repr(C, packed)]
#[derive(Clone, Copy)]
struct EpollEvent {
    events: u32,
    data: u64,
}

extern "C" {
    fn epoll_create1(flags: i32) -> i32;
    fn epoll_ctl(epfd: i32, op: i32, fd: i32, event: *mut EpollEvent) -> i32;
    fn epoll_wait(epfd: i32, events: *mut EpollEvent, maxevents: i32, timeout: i32) -> i32;
}

const EPOLLIN: u32 = 0x1;
const EPOLLOUT: u32 = 0x4;
const EPOLLERR: u32 = 0x8;
const EPOLLHUP: u32 = 0x10;
const EPOLLRDHUP: u32 = 0x2000;
const CTL_ADD: i32 = 1;
const CTL_DEL: i32 = 2;
const CTL_MOD: i32 = 3;
const LISTENER: u64 = u64::MAX;
const MAX_HEADER: usize = 64 * 1024;
const MAX_BODY: usize = 512 * 1024 * 1024;

struct Conn {
    stream: TcpStream,
    input: Vec<u8>,
    output: Vec<u8>,
    sent: usize,
    close_after: bool,
    eof: bool,
    continue_sent: bool,
    want_out: bool,
}

enum Parse {
    Incomplete { expect_continue: bool },
    Done { req: Request, keep_alive: bool, used: usize },
    Bad(u16),
}

fn find(hay: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    if hay.len() < needle.len() || from > hay.len() - needle.len() {
        return None;
    }
    (from..=hay.len() - needle.len()).find(|&i| &hay[i..i + needle.len()] == needle)
}

fn chunked(buf: &[u8]) -> Result<Option<(Vec<u8>, usize)>, ()> {
    let mut body = Vec::new();
    let mut i = 0;
    loop {
        let Some(eol) = find(buf, b"\r\n", i) else { return Ok(None) };
        let line = std::str::from_utf8(&buf[i..eol]).map_err(|_| ())?;
        let size = usize::from_str_radix(line.split(';').next().unwrap_or("").trim(), 16).map_err(|_| ())?;
        i = eol + 2;
        if size == 0 {
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

fn parse(buf: &[u8]) -> Parse {
    let Some(head_end) = find(buf, b"\r\n\r\n", 0) else {
        return if buf.len() > MAX_HEADER { Parse::Bad(431) } else { Parse::Incomplete { expect_continue: false } };
    };
    let head = String::from_utf8_lossy(&buf[..head_end]);
    let mut lines = head.split("\r\n");
    let parts: Vec<&str> = lines.next().unwrap_or("").split(' ').collect();
    if parts.len() != 3 || !parts[2].starts_with("HTTP/1.") || parts[0].is_empty() {
        return Parse::Bad(400);
    }
    let mut headers: Vec<(String, String)> = Vec::new();
    for line in lines {
        let Some((k, v)) = line.split_once(':') else { return Parse::Bad(400) };
        let k = k.trim().to_ascii_lowercase();
        let v = v.trim().to_string();
        match headers.iter_mut().find(|(n, _)| *n == k) {
            Some(slot) => slot.1 = format!("{}, {}", slot.1, v),
            None => headers.push((k, v)),
        }
    }
    let get = |n: &str| headers.iter().find(|(k, _)| k == n).map(|(_, v)| v.as_str());
    let conn_hdr = get("connection").unwrap_or("").to_ascii_lowercase();
    let keep_alive = if parts[2] == "HTTP/1.0" { conn_hdr.contains("keep-alive") } else { !conn_hdr.contains("close") };
    let expect_continue = get("expect").map(|v| v.eq_ignore_ascii_case("100-continue")).unwrap_or(false);
    let rest = &buf[head_end + 4..];
    let (body, body_used) = if get("transfer-encoding").map(|v| v.to_ascii_lowercase().contains("chunked")).unwrap_or(false) {
        match chunked(rest) {
            Err(()) => return Parse::Bad(400),
            Ok(None) => return if rest.len() > MAX_BODY { Parse::Bad(413) } else { Parse::Incomplete { expect_continue } },
            Ok(Some(x)) => x,
        }
    } else {
        let len = match get("content-length") {
            None => 0,
            Some(v) => match v.trim().parse::<usize>() {
                Ok(n) => n,
                Err(_) => return Parse::Bad(400),
            },
        };
        if len > MAX_BODY {
            return Parse::Bad(413);
        }
        if rest.len() < len {
            return Parse::Incomplete { expect_continue };
        }
        (rest[..len].to_vec(), len)
    };
    let (path, query) = match parts[1].split_once('?') {
        Some((p, q)) => (p.to_string(), q.to_string()),
        None => (parts[1].to_string(), String::new()),
    };
    let req = Request { method: parts[0].to_string(), path, query, headers, body };
    Parse::Done { req, keep_alive, used: head_end + 4 + body_used }
}

fn render(res: &Response, keep_alive: bool, head_only: bool, out: &mut Vec<u8>) {
    let no_body = res.status == 204 || res.status == 304;
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
}

fn ctl(ep: i32, op: i32, fd: i32, events: u32, token: u64) {
    let mut ev = EpollEvent { events, data: token };
    unsafe {
        epoll_ctl(ep, op, fd, &mut ev);
    }
}

/// Serve requests on `listener` with one epoll loop. Returns only if epoll cannot be created.
pub fn serve(listener: TcpListener, handler: Handler, spin_polls: usize) -> Result<(), TcpListener> {
    let ep = unsafe { epoll_create1(0o2000000) }; // EPOLL_CLOEXEC
    if ep < 0 || listener.set_nonblocking(true).is_err() {
        return Err(listener);
    }
    ctl(ep, CTL_ADD, listener.as_raw_fd(), EPOLLIN, LISTENER);
    let mut conns: HashMap<u64, Conn> = HashMap::new();
    let mut next_token: u64 = 1;
    let mut events = vec![EpollEvent { events: 0, data: 0 }; 256];
    let mut idle_polls = 0usize;
    let mut chunk = vec![0u8; 64 * 1024];
    loop {
        let timeout = if idle_polls < spin_polls { 0 } else { -1 };
        let n = unsafe { epoll_wait(ep, events.as_mut_ptr(), events.len() as i32, timeout) };
        if n <= 0 {
            if n == 0 {
                idle_polls += 1;
                std::hint::spin_loop();
            }
            continue;
        }
        idle_polls = 0;
        for i in 0..n as usize {
            let EpollEvent { events: flags, data: token } = events[i];
            if token == LISTENER {
                loop {
                    match listener.accept() {
                        Ok((s, _)) => {
                            let _ = s.set_nonblocking(true);
                            let _ = s.set_nodelay(true);
                            let t = next_token;
                            next_token += 1;
                            ctl(ep, CTL_ADD, s.as_raw_fd(), EPOLLIN | EPOLLRDHUP, t);
                            conns.insert(t, Conn { stream: s, input: Vec::new(), output: Vec::new(), sent: 0, close_after: false, eof: false, continue_sent: false, want_out: false });
                        }
                        Err(e) if e.kind() == ErrorKind::WouldBlock => break,
                        Err(e) if e.kind() == ErrorKind::Interrupted => continue,
                        Err(_) => break,
                    }
                }
                continue;
            }
            let Some(c) = conns.get_mut(&token) else { continue };
            let mut dead = flags & EPOLLERR != 0;
            if flags & (EPOLLIN | EPOLLHUP | EPOLLRDHUP) != 0 && !c.close_after {
                loop {
                    match c.stream.read(&mut chunk) {
                        Ok(0) => {
                            c.eof = true;
                            break;
                        }
                        Ok(k) => c.input.extend_from_slice(&chunk[..k]),
                        Err(e) if e.kind() == ErrorKind::WouldBlock => break,
                        Err(e) if e.kind() == ErrorKind::Interrupted => continue,
                        Err(_) => {
                            dead = true;
                            break;
                        }
                    }
                }
                // Answer every complete request in the buffer, in order.
                while !c.close_after && !c.input.is_empty() {
                    match parse(&c.input) {
                        Parse::Incomplete { expect_continue } => {
                            if expect_continue && !c.continue_sent {
                                c.continue_sent = true;
                                c.output.extend_from_slice(b"HTTP/1.1 100 Continue\r\n\r\n");
                            }
                            break;
                        }
                        Parse::Bad(status) => {
                            c.output.extend_from_slice(format!("HTTP/1.1 {} {}\r\ncontent-type: application/json\r\ncontent-length: 2\r\nconnection: close\r\n\r\n{{}}", status, reason(status)).as_bytes());
                            c.close_after = true;
                            c.input.clear();
                        }
                        Parse::Done { req, keep_alive, used } => {
                            c.input.drain(..used);
                            c.continue_sent = false;
                            let head_only = req.method == "HEAD";
                            let res = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| handler(&req))) {
                                Ok(r) => r,
                                Err(_) => Response::error(500, "Internal Server Error"),
                            };
                            render(&res, keep_alive, head_only, &mut c.output);
                            if !keep_alive {
                                c.close_after = true;
                            }
                        }
                    }
                }
            }
            // Flush.
            if !dead && c.sent < c.output.len() {
                loop {
                    match c.stream.write(&c.output[c.sent..]) {
                        Ok(0) => {
                            dead = true;
                            break;
                        }
                        Ok(k) => {
                            c.sent += k;
                            if c.sent == c.output.len() {
                                break;
                            }
                        }
                        Err(e) if e.kind() == ErrorKind::WouldBlock => break,
                        Err(e) if e.kind() == ErrorKind::Interrupted => continue,
                        Err(_) => {
                            dead = true;
                            break;
                        }
                    }
                }
            }
            if c.sent == c.output.len() && !c.output.is_empty() {
                c.output.clear();
                c.sent = 0;
            }
            let pending = c.sent < c.output.len();
            if !dead && pending != c.want_out {
                c.want_out = pending;
                ctl(ep, CTL_MOD, c.stream.as_raw_fd(), EPOLLIN | EPOLLRDHUP | if pending { EPOLLOUT } else { 0 }, token);
            }
            if dead || (!pending && (c.close_after || c.eof)) {
                if !dead && c.close_after {
                    let _ = c.stream.flush();
                    let _ = c.stream.shutdown(std::net::Shutdown::Write);
                }
                ctl(ep, CTL_DEL, c.stream.as_raw_fd(), 0, token);
                conns.remove(&token);
            }
        }
    }
}
