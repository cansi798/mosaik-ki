//! Minimaler HTTP/1.1-Client nur für 127.0.0.1 (llama-server). Keine TLS, keine Weiterleitungen.
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpResponse {
    pub status: u16,
    pub body: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HttpError {
    Refused,
    Timeout,
    Io(String),
    Malformed(String),
}

pub fn request(
    port: u16,
    method: &str,
    path: &str,
    body: Option<&str>,
    timeout: Duration,
) -> Result<HttpResponse, HttpError> {
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let mut stream = TcpStream::connect_timeout(&addr, Duration::from_secs(2)).map_err(map_io)?;
    stream.set_read_timeout(Some(timeout)).map_err(map_io)?;
    stream.set_write_timeout(Some(timeout)).map_err(map_io)?;
    let body = body.unwrap_or("");
    let head = format!(
        "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(head.as_bytes()).map_err(map_io)?;
    stream.write_all(body.as_bytes()).map_err(map_io)?;
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).map_err(map_io)?;
    parse_response(&raw)
}

fn map_io(e: std::io::Error) -> HttpError {
    use std::io::ErrorKind;
    match e.kind() {
        ErrorKind::ConnectionRefused | ErrorKind::ConnectionReset | ErrorKind::NotConnected => {
            HttpError::Refused
        }
        ErrorKind::TimedOut | ErrorKind::WouldBlock => HttpError::Timeout,
        _ => HttpError::Io(e.to_string()),
    }
}

fn malformed(why: impl ToString) -> HttpError {
    HttpError::Malformed(why.to_string())
}

pub fn parse_response(raw: &[u8]) -> Result<HttpResponse, HttpError> {
    let split = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or_else(|| malformed("kein Header-Ende"))?;
    let head = std::str::from_utf8(&raw[..split]).map_err(malformed)?;
    let mut lines = head.split("\r\n");
    let status = lines
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|s| s.parse::<u16>().ok())
        .ok_or_else(|| malformed("Statuszeile"))?;
    let chunked = lines.any(|l| {
        let l = l.to_ascii_lowercase();
        l.starts_with("transfer-encoding:") && l.contains("chunked")
    });
    let rest = &raw[split + 4..];
    let bytes = if chunked {
        decode_chunked(rest)?
    } else {
        rest.to_vec()
    };
    let body = String::from_utf8(bytes).map_err(malformed)?;
    Ok(HttpResponse { status, body })
}

fn decode_chunked(mut data: &[u8]) -> Result<Vec<u8>, HttpError> {
    let mut out = Vec::new();
    loop {
        let end = data
            .windows(2)
            .position(|w| w == b"\r\n")
            .ok_or_else(|| malformed("Chunk-Länge"))?;
        let line = std::str::from_utf8(&data[..end]).map_err(malformed)?;
        let size = usize::from_str_radix(line.split(';').next().unwrap_or("").trim(), 16)
            .map_err(malformed)?;
        data = &data[end + 2..];
        if size == 0 {
            return Ok(out);
        }
        let chunk = data.get(..size).ok_or_else(|| malformed("Chunk zu kurz"))?;
        out.extend_from_slice(chunk);
        data = data.get(size + 2..).unwrap_or(&[]);
    }
}
