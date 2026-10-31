//! Serveur HTTP/1.1 minimal pour l'interface RPC locale.
//!
//! Volontairement restreint : une requête par connexion, corps borné par
//! `Content-Length` (pas d'encodage « chunked »), en-têtes limités en taille.

use std::io::{self, Read, Write};
use std::net::TcpStream;
use std::time::Instant;

/// Lecture avec un délai **total** (et non par appel) : un client qui
/// enverrait sa requête octet par octet ne peut pas occuper une place de
/// connexion plus longtemps que ce délai.
pub struct DeadlineReader<'a> {
    stream: &'a TcpStream,
    deadline: Instant,
}

impl<'a> DeadlineReader<'a> {
    pub fn new(stream: &'a TcpStream, deadline: Instant) -> Self {
        DeadlineReader { stream, deadline }
    }
}

impl Read for DeadlineReader<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let remaining = self.deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(io::Error::new(io::ErrorKind::TimedOut, "délai de lecture de la requête dépassé"));
        }
        self.stream.set_read_timeout(Some(remaining))?;
        let mut stream = self.stream;
        stream.read(buf)
    }
}

/// Taille maximale de la ligne de requête + en-têtes.
pub const MAX_HEAD: usize = 16 * 1024;
/// Taille maximale du corps.
pub const MAX_BODY: usize = 4 * 1024 * 1024;
const MAX_HEADERS: usize = 64;

/// Requête HTTP.
#[derive(Debug, Default)]
pub struct Request {
    pub method: String,
    pub path: String,
    pub query: Vec<(String, String)>,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Request {
    /// Valeur d'un en-tête (nom insensible à la casse).
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }

    pub fn query_param(&self, name: &str) -> Option<&str> {
        self.query.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str())
    }
}

/// Réponse HTTP (corps JSON).
#[derive(Debug)]
pub struct Response {
    pub status: u16,
    pub body: String,
}

/// Erreur de lecture de requête, avec le code HTTP à renvoyer.
#[derive(Debug)]
pub enum HttpError {
    Status(u16, &'static str),
    Io(io::Error),
}

impl From<io::Error> for HttpError {
    fn from(e: io::Error) -> Self {
        HttpError::Io(e)
    }
}

fn find_head_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n")
}

/// Lit une requête complète.
pub fn read_request<R: Read>(stream: &mut R) -> Result<Request, HttpError> {
    let mut buf = Vec::with_capacity(1024);
    let mut chunk = [0u8; 4096];
    let head_end = loop {
        if let Some(pos) = find_head_end(&buf) {
            break pos;
        }
        if buf.len() > MAX_HEAD {
            return Err(HttpError::Status(431, "en-têtes trop grands"));
        }
        let n = stream.read(&mut chunk)?;
        if n == 0 {
            return Err(HttpError::Status(400, "requête incomplète"));
        }
        buf.extend_from_slice(&chunk[..n]);
    };
    if head_end > MAX_HEAD {
        return Err(HttpError::Status(431, "en-têtes trop grands"));
    }
    let head = std::str::from_utf8(&buf[..head_end]).map_err(|_| HttpError::Status(400, "en-têtes non UTF-8"))?;
    let mut lines = head.split("\r\n");
    let request_line = lines.next().unwrap_or_default();
    let mut parts = request_line.split(' ');
    let (Some(method), Some(target), Some(version), None) = (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err(HttpError::Status(400, "ligne de requête invalide"));
    };
    if !method.bytes().all(|c| c.is_ascii_uppercase()) || method.is_empty() {
        return Err(HttpError::Status(400, "méthode invalide"));
    }
    if version != "HTTP/1.1" && version != "HTTP/1.0" {
        return Err(HttpError::Status(505, "version HTTP non prise en charge"));
    }
    if !target.starts_with('/') {
        return Err(HttpError::Status(400, "chemin invalide"));
    }
    let mut headers = Vec::new();
    for line in lines {
        let (name, value) = line.split_once(':').ok_or(HttpError::Status(400, "en-tête invalide"))?;
        if name.is_empty() || name.bytes().any(|c| c.is_ascii_whitespace()) {
            return Err(HttpError::Status(400, "nom d'en-tête invalide"));
        }
        headers.push((name.to_string(), value.trim().to_string()));
        if headers.len() > MAX_HEADERS {
            return Err(HttpError::Status(431, "trop d'en-têtes"));
        }
    }
    let mut request = Request { method: method.to_string(), headers, ..Request::default() };
    if request.header("transfer-encoding").is_some() {
        return Err(HttpError::Status(501, "Transfer-Encoding non pris en charge"));
    }
    let content_length = match request.header("content-length") {
        None => 0,
        Some(v) => v.parse::<usize>().map_err(|_| HttpError::Status(400, "Content-Length invalide"))?,
    };
    if content_length > MAX_BODY {
        return Err(HttpError::Status(413, "corps de requête trop grand"));
    }
    let mut body = buf[head_end + 4..].to_vec();
    if body.len() > content_length {
        return Err(HttpError::Status(400, "corps plus long que Content-Length"));
    }
    while body.len() < content_length {
        let n = stream.read(&mut chunk)?;
        if n == 0 {
            return Err(HttpError::Status(400, "corps incomplet"));
        }
        body.extend_from_slice(&chunk[..n]);
        if body.len() > content_length {
            return Err(HttpError::Status(400, "corps plus long que Content-Length"));
        }
    }
    request.body = body;
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    request.path = path.to_string();
    request.query = query
        .split('&')
        .filter(|p| !p.is_empty())
        .map(|p| match p.split_once('=') {
            Some((k, v)) => (k.to_string(), v.to_string()),
            None => (p.to_string(), String::new()),
        })
        .collect();
    Ok(request)
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        400 => "Bad Request",
        401 => "Unauthorized",
        404 => "Not Found",
        405 => "Method Not Allowed",
        409 => "Conflict",
        413 => "Payload Too Large",
        431 => "Request Header Fields Too Large",
        500 => "Internal Server Error",
        501 => "Not Implemented",
        503 => "Service Unavailable",
        505 => "HTTP Version Not Supported",
        _ => "Unknown",
    }
}

/// Écrit une réponse JSON et ferme logiquement la connexion.
pub fn write_response(stream: &mut TcpStream, response: &Response) -> io::Result<()> {
    let head = format!(
        "HTTP/1.1 {} {}\r\nContent-Type: application/json; charset=utf-8\r\nContent-Length: {}\r\nCache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\nConnection: close\r\n\r\n",
        response.status,
        reason(response.status),
        response.body.len()
    );
    stream.write_all(head.as_bytes())?;
    stream.write_all(response.body.as_bytes())?;
    stream.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(raw: &str) -> Result<Request, HttpError> {
        read_request(&mut raw.as_bytes())
    }

    #[test]
    fn simple_get_and_post() {
        let r = parse("GET /v1/info?x=1&y HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer abc\r\n\r\n").unwrap();
        assert_eq!(r.method, "GET");
        assert_eq!(r.path, "/v1/info");
        assert_eq!(r.query_param("x"), Some("1"));
        assert_eq!(r.query_param("y"), Some(""));
        assert_eq!(r.header("authorization"), Some("Bearer abc"));
        let r = parse("POST /v1/tx HTTP/1.1\r\nContent-Length: 4\r\n\r\nabcd").unwrap();
        assert_eq!(r.body, b"abcd");
    }

    #[test]
    fn rejections() {
        let status = |raw: &str| match parse(raw) {
            Err(HttpError::Status(s, _)) => s,
            other => panic!("{other:?}"),
        };
        assert_eq!(status("GET /x HTTP/2.0\r\n\r\n"), 505);
        assert_eq!(status("get /x HTTP/1.1\r\n\r\n"), 400);
        assert_eq!(status("GET x HTTP/1.1\r\n\r\n"), 400);
        assert_eq!(status("GET /x HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n"), 501);
        assert_eq!(status(&format!("POST /x HTTP/1.1\r\nContent-Length: {}\r\n\r\n", MAX_BODY + 1)), 413);
        assert_eq!(status("POST /x HTTP/1.1\r\nContent-Length: 10\r\n\r\nabc"), 400);
        assert_eq!(status("POST /x HTTP/1.1\r\nContent-Length: 1\r\n\r\nabc"), 400);
        assert_eq!(status(&format!("GET /x HTTP/1.1\r\nA: {}\r\n\r\n", "x".repeat(MAX_HEAD))), 431);
        assert_eq!(status("GET /x HTTP/1.1\r\nPasDeuxPoints\r\n\r\n"), 400);
    }

    /// Un client qui envoie sa requête au compte-gouttes est coupé au bout du
    /// délai total, même si chaque octet arrive avant le délai par lecture.
    #[test]
    fn slow_client_hits_total_deadline() {
        use std::net::TcpListener;
        use std::time::Duration;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let client = std::thread::spawn(move || {
            let mut s = TcpStream::connect(addr).unwrap();
            for _ in 0..40 {
                if s.write_all(b"G").is_err() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        });
        let (server, _) = listener.accept().unwrap();
        let start = Instant::now();
        let mut reader = DeadlineReader::new(&server, start + Duration::from_millis(400));
        let result = read_request(&mut reader);
        let elapsed = start.elapsed();
        assert!(matches!(result, Err(HttpError::Io(_))), "{result:?}");
        assert!(elapsed < Duration::from_millis(1500), "{elapsed:?}");
        drop(server);
        let _ = client.join();
    }
}
