//! A small HTTP/1.1 server for the phone app: one thread per connection (a household has a
//! few phones), keep-alive so a screenful of thumbnails reuses one connection, request bodies
//! with Content-Length only, and byte ranges for video. TLS (rustls) on the main port; the
//! media port is plain HTTP and only serves signed, short-lived links (see `api`).

use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// More connections than this at once are turned away; a phone opens a handful.
const MAX_CONNECTIONS: usize = 64;
const MAX_HEADER_BYTES: usize = 16 * 1024;
/// A keep-alive connection with nothing to say for this long is closed.
const IDLE: Duration = Duration::from_secs(30);

pub struct Request {
    pub method: String,
    /// Path without the query, e.g. "/api/thumb/0123abcd".
    pub path: String,
    pub query: Vec<(String, String)>,
    pub headers: Vec<(String, String)>,
    /// Bytes of the body still to read (from Content-Length).
    pub body_len: u64,
}

impl Request {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }

    pub fn param(&self, name: &str) -> Option<&str> {
        self.query.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str())
    }
}

pub enum Body {
    Bytes(Vec<u8>),
    /// A file, or the byte range `start..start + len` of it.
    File { path: PathBuf, start: u64, len: u64 },
}

pub struct Response {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Body,
}

impl Response {
    pub fn bytes(status: u16, content_type: &str, body: Vec<u8>) -> Self {
        Self { status, headers: vec![("Content-Type".into(), content_type.into())], body: Body::Bytes(body) }
    }

    pub fn json(status: u16, value: &serde_json::Value) -> Self {
        Self::bytes(status, "application/json", serde_json::to_vec(value).unwrap_or_default())
    }

    pub fn error(status: u16, message: &str) -> Self {
        Self::json(status, &serde_json::json!({ "error": message }))
    }

    /// The whole file, or the part a `Range: bytes=a-b` header asks for (206), for video seeking.
    pub fn file(path: PathBuf, content_type: &str, range: Option<&str>) -> Self {
        let Ok(total) = std::fs::metadata(&path).map(|m| m.len()) else { return Self::error(404, "file is gone") };
        let mut headers = vec![("Content-Type".into(), content_type.into()), ("Accept-Ranges".into(), "bytes".into())];
        match range.map(|r| parse_range(r, total)) {
            Some(Some((start, end))) => {
                headers.push(("Content-Range".into(), format!("bytes {start}-{end}/{total}")));
                Self { status: 206, headers, body: Body::File { path, start, len: end - start + 1 } }
            }
            Some(None) => {
                let mut r = Self::error(416, "range not satisfiable");
                r.headers.push(("Content-Range".into(), format!("bytes */{total}")));
                r
            }
            None => Self { status: 200, headers, body: Body::File { path, start: 0, len: total } },
        }
    }
}

/// `bytes=a-b`, `bytes=a-` or `bytes=-n` → inclusive (start, end) within a file of `total` bytes.
/// Only the first range of a multi-range request is served.
pub fn parse_range(header: &str, total: u64) -> Option<(u64, u64)> {
    let spec = header.trim().strip_prefix("bytes=")?.split(',').next()?.trim();
    let (a, b) = spec.split_once('-')?;
    if total == 0 {
        return None;
    }
    let (start, end) = if a.is_empty() {
        let n: u64 = b.parse().ok()?;
        (total.saturating_sub(n), total - 1)
    } else {
        let start: u64 = a.parse().ok()?;
        let end = if b.is_empty() { total - 1 } else { b.parse::<u64>().ok()?.min(total - 1) };
        (start, end)
    };
    (start <= end && start < total).then_some((start, end))
}

/// Answers requests; it may read the request body from `body` (at most `req.body_len` bytes).
pub type Handler = dyn Fn(&Request, &mut dyn Read) -> Response + Send + Sync;

/// Listens on `addr` until `stop` is set, with TLS when `tls` is given.
pub fn serve(listener: TcpListener, tls: Option<Arc<rustls::ServerConfig>>, handler: Arc<Handler>, stop: Arc<AtomicBool>) {
    let _ = listener.set_nonblocking(true);
    let open = Arc::new(AtomicUsize::new(0));
    while !stop.load(Ordering::Relaxed) {
        let (stream, _) = match listener.accept() {
            Ok(c) => c,
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(100));
                continue;
            }
            Err(_) => continue,
        };
        if open.load(Ordering::Relaxed) >= MAX_CONNECTIONS {
            continue;
        }
        let (tls, handler, stop, open) = (tls.clone(), handler.clone(), stop.clone(), open.clone());
        open.fetch_add(1, Ordering::Relaxed);
        std::thread::spawn(move || {
            let _ = stream.set_nonblocking(false);
            let _ = stream.set_read_timeout(Some(IDLE));
            let _ = stream.set_nodelay(true);
            match tls {
                Some(config) => {
                    if let Ok(conn) = rustls::ServerConnection::new(config) {
                        connection(rustls::StreamOwned::new(conn, stream), &*handler, &stop);
                    }
                }
                None => connection(stream, &*handler, &stop),
            }
            open.fetch_sub(1, Ordering::Relaxed);
        });
    }
}

trait Stream: Read + Write {}
impl<T: Read + Write> Stream for T {}

/// Serves requests on one connection until the client closes it, asks to, or goes idle.
fn connection<S: Stream>(stream: S, handler: &Handler, stop: &AtomicBool) {
    let mut reader = BufReader::new(stream);
    while !stop.load(Ordering::Relaxed) {
        let Some(req) = read_request(&mut reader) else { return };
        let keep_alive = !req.header("Connection").is_some_and(|c| c.eq_ignore_ascii_case("close"));
        let head = req.method == "HEAD";
        let mut body = (&mut reader).take(req.body_len);
        let resp = handler(&req, &mut body);
        // Whatever the handler didn't read of the body is skipped, so the next request lines up.
        if std::io::copy(&mut body, &mut std::io::sink()).is_err() {
            return;
        }
        if write_response(reader.get_mut(), resp, keep_alive, head).is_err() || !keep_alive {
            let _ = reader.get_mut().flush();
            return;
        }
    }
}

fn read_request<R: BufRead>(r: &mut R) -> Option<Request> {
    let mut raw = Vec::new();
    // Headers end at the first empty line.
    loop {
        let n = r.read_until(b'\n', &mut raw).ok()?;
        if n == 0 || raw.len() > MAX_HEADER_BYTES {
            return None;
        }
        if raw.ends_with(b"\r\n\r\n") || raw.ends_with(b"\n\n") {
            break;
        }
        // Stray blank lines before a request (allowed by RFC 9112).
        if raw == b"\r\n" || raw == b"\n" {
            raw.clear();
        }
    }
    let mut headers = [httparse::EMPTY_HEADER; 64];
    let mut parsed = httparse::Request::new(&mut headers);
    if !matches!(parsed.parse(&raw), Ok(httparse::Status::Complete(_))) {
        return None;
    }
    let (path, query) = parsed.path?.split_once('?').unwrap_or((parsed.path?, ""));
    let headers: Vec<(String, String)> =
        parsed.headers.iter().map(|h| (h.name.to_string(), String::from_utf8_lossy(h.value).trim().to_string())).collect();
    let body_len = headers.iter().find(|(k, _)| k.eq_ignore_ascii_case("Content-Length")).and_then(|(_, v)| v.parse().ok()).unwrap_or(0);
    // Chunked request bodies aren't supported; the phone app sends Content-Length.
    if headers.iter().any(|(k, v)| k.eq_ignore_ascii_case("Transfer-Encoding") && !v.eq_ignore_ascii_case("identity")) {
        return None;
    }
    Some(Request {
        method: parsed.method?.to_string(),
        path: crate::transfer::google::decode(path),
        query: query
            .split('&')
            .filter(|kv| !kv.is_empty())
            .map(|kv| {
                let (k, v) = kv.split_once('=').unwrap_or((kv, ""));
                (crate::transfer::google::decode(k), crate::transfer::google::decode(v))
            })
            .collect(),
        headers,
        body_len,
    })
}

fn write_response<W: Write>(w: &mut W, resp: Response, keep_alive: bool, head: bool) -> std::io::Result<()> {
    let len = match &resp.body {
        Body::Bytes(b) => b.len() as u64,
        Body::File { len, .. } => *len,
    };
    let mut out = format!("HTTP/1.1 {} {}\r\n", resp.status, reason(resp.status));
    for (k, v) in &resp.headers {
        out += &format!("{k}: {v}\r\n");
    }
    out += &format!("Content-Length: {len}\r\nConnection: {}\r\n\r\n", if keep_alive { "keep-alive" } else { "close" });
    w.write_all(out.as_bytes())?;
    if !head {
        match resp.body {
            Body::Bytes(b) => w.write_all(&b)?,
            Body::File { path, start, len } => {
                let mut f = std::fs::File::open(path)?;
                f.seek(SeekFrom::Start(start))?;
                let copied = std::io::copy(&mut f.take(len), w)?;
                if copied != len {
                    // The file shrank under us; the client can't trust this response.
                    return Err(std::io::ErrorKind::UnexpectedEof.into());
                }
            }
        }
    }
    w.flush()
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        206 => "Partial Content",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        409 => "Conflict",
        411 => "Length Required",
        413 => "Payload Too Large",
        416 => "Range Not Satisfiable",
        429 => "Too Many Requests",
        503 => "Service Unavailable",
        _ => "Error",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranges() {
        assert_eq!(parse_range("bytes=0-99", 1000), Some((0, 99)));
        assert_eq!(parse_range("bytes=500-", 1000), Some((500, 999)));
        assert_eq!(parse_range("bytes=-100", 1000), Some((900, 999)));
        assert_eq!(parse_range("bytes=900-5000", 1000), Some((900, 999)));
        assert_eq!(parse_range("bytes=0-0,10-20", 1000), Some((0, 0)));
        assert_eq!(parse_range("bytes=1000-", 1000), None);
        assert_eq!(parse_range("bytes=5-1", 1000), None);
        assert_eq!(parse_range("items=0-1", 1000), None);
    }

    #[test]
    fn parses_requests_and_keeps_alive() {
        let raw = b"\r\nGET /api/thumb/ab%20c?x=1&q=red+car HTTP/1.1\r\nHost: a\r\nContent-Length: 3\r\n\r\nabcGET /next HTTP/1.1\r\n\r\n";
        let mut r = BufReader::new(&raw[..]);
        let req = read_request(&mut r).unwrap();
        assert_eq!((req.method.as_str(), req.path.as_str(), req.body_len), ("GET", "/api/thumb/ab c", 3));
        assert_eq!(req.param("q"), Some("red car"));
        assert_eq!(req.header("host"), Some("a"));
        let mut body = String::new();
        (&mut r).take(req.body_len).read_to_string(&mut body).unwrap();
        assert_eq!(body, "abc");
        assert_eq!(read_request(&mut r).unwrap().path, "/next");
        assert!(read_request(&mut r).is_none());
    }

    #[test]
    fn writes_ranges_of_files() {
        let f = std::env::temp_dir().join(format!("banana-http-{}", std::process::id()));
        std::fs::write(&f, b"0123456789").unwrap();
        let mut out = Vec::new();
        write_response(&mut out, Response::file(f.clone(), "video/mp4", Some("bytes=2-4")), true, false).unwrap();
        let _ = std::fs::remove_file(&f);
        let text = String::from_utf8(out).unwrap();
        assert!(text.starts_with("HTTP/1.1 206 Partial Content\r\n"));
        assert!(text.contains("Content-Range: bytes 2-4/10\r\n") && text.contains("Content-Length: 3\r\n"));
        assert!(text.ends_with("\r\n\r\n234"));
    }
}
