#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Minimal local HTTP server for the tests (no real network access).
#![allow(dead_code)]

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

#[derive(Clone, Debug)]
pub struct Req {
    pub path: String,
    pub headers: HashMap<String, String>,
}

pub type Handler = dyn Fn(&Req, &mut TcpStream, usize) + Send + Sync;

pub struct Server {
    pub port: u16,
    pub requests: Arc<Mutex<Vec<Req>>>,
    pub connections: Arc<AtomicUsize>,
    stop: Arc<AtomicBool>,
}

impl Server {
    /// `handler(req, stream, n)` where `n` is the rank of the request (0, 1, …).
    pub fn start(handler: impl Fn(&Req, &mut TcpStream, usize) + Send + Sync + 'static) -> Server {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let port = listener.local_addr().unwrap().port();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let connections = Arc::new(AtomicUsize::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let handler: Arc<Handler> = Arc::new(handler);
        let (rq, cn, st) = (requests.clone(), connections.clone(), stop.clone());
        thread::spawn(move || {
            while !st.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((mut s, _)) => {
                        cn.fetch_add(1, Ordering::SeqCst);
                        let (h, rq) = (handler.clone(), rq.clone());
                        thread::spawn(move || {
                            let _ = s.set_nonblocking(false);
                            let Some(req) = read_req(&mut s) else { return };
                            let n = {
                                let mut g = rq.lock().unwrap();
                                g.push(req.clone());
                                g.len() - 1
                            };
                            h(&req, &mut s, n);
                        });
                    }
                    Err(_) => thread::sleep(Duration::from_millis(2)),
                }
            }
        });
        Server {
            port,
            requests,
            connections,
            stop,
        }
    }

    pub fn url(&self, path: &str) -> String {
        format!("http://127.0.0.1:{}{path}", self.port)
    }

    pub fn base(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    pub fn hits(&self) -> usize {
        self.requests.lock().unwrap().len()
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

fn read_req(s: &mut TcpStream) -> Option<Req> {
    s.set_read_timeout(Some(Duration::from_secs(5))).ok()?;
    let mut buf = Vec::new();
    let mut b = [0u8; 1];
    while !buf.ends_with(b"\r\n\r\n") {
        s.read_exact(&mut b).ok()?;
        buf.push(b[0]);
    }
    let text = String::from_utf8_lossy(&buf).to_string();
    let mut lines = text.lines();
    let first = lines.next()?;
    let path = first.split_whitespace().nth(1)?.to_string();
    let headers = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_string()))
        .collect();
    Some(Req { path, headers })
}

#[derive(Clone, Default)]
pub struct FileOpts {
    pub etag: Option<String>,
    /// The server understands `Range`.
    pub range: bool,
    /// Cuts the connection after this number of body bytes (full headers sent).
    pub cut_at: Option<usize>,
    /// Pause between two 4 KiB blocks.
    pub chunk_delay: Option<Duration>,
    /// Send nothing after the headers (timeout exceeded).
    pub stall: bool,
    /// Ignores `If-Range` (server that answers 206 despite a different ETag).
    pub ignore_if_range: bool,
}

pub fn send_simple(s: &mut TcpStream, status: &str, headers: &[(&str, &str)], body: &[u8]) {
    let mut h = format!(
        "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n",
        body.len()
    );
    for (k, v) in headers {
        h.push_str(&format!("{k}: {v}\r\n"));
    }
    h.push_str("\r\n");
    let _ = s.write_all(h.as_bytes());
    let _ = s.write_all(body);
}

/// Serves `body` with or without Range according to `o`.
pub fn serve_file(req: &Req, s: &mut TcpStream, body: &[u8], o: &FileOpts) {
    let total = body.len();
    let mut start = 0usize;
    let mut status = "200 OK";
    let mut extra = String::new();
    if let Some(e) = &o.etag {
        extra.push_str(&format!("ETag: {e}\r\n"));
    }
    if o.range {
        extra.push_str("Accept-Ranges: bytes\r\n");
        if let Some(r) = req.headers.get("range") {
            let n: usize = r
                .trim_start_matches("bytes=")
                .trim_end_matches('-')
                .parse()
                .unwrap_or(0);
            let if_range_ok = o.ignore_if_range
                || match (req.headers.get("if-range"), &o.etag) {
                    (Some(ir), Some(e)) => ir == e,
                    (Some(_), None) => false,
                    (None, _) => true,
                };
            if n >= total {
                send_simple(
                    s,
                    "416 Range Not Satisfiable",
                    &[("Content-Range", &format!("bytes */{total}"))],
                    b"",
                );
                return;
            }
            if if_range_ok {
                start = n;
                status = "206 Partial Content";
                extra.push_str(&format!(
                    "Content-Range: bytes {n}-{}/{total}\r\n",
                    total - 1
                ));
            }
        }
    }
    let part = &body[start..];
    let head = format!(
        "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n{extra}\r\n",
        part.len()
    );
    let _ = s.write_all(head.as_bytes());
    if o.stall {
        thread::sleep(Duration::from_secs(3));
        return;
    }
    let limit = o.cut_at.unwrap_or(part.len()).min(part.len());
    for c in part[..limit].chunks(4096) {
        if s.write_all(c).is_err() {
            return;
        }
        if let Some(d) = o.chunk_delay {
            thread::sleep(d);
        }
    }
    let _ = s.flush();
}

/// Deterministic pseudo-random content.
pub fn make_body(len: usize) -> Vec<u8> {
    let mut x: u32 = 0x1234_5678;
    (0..len)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            (x >> 8) as u8
        })
        .collect()
}

pub fn sha256_hex(data: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(data)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}
