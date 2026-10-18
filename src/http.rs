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
    let mut stream =
        TcpStream::connect_timeout(&addr, Duration::from_secs(2)).map_err(map_connect)?;
    stream.set_read_timeout(Some(timeout)).map_err(map_io)?;
    stream.set_write_timeout(Some(timeout)).map_err(map_io)?;
    let body = body.unwrap_or("");
    let head = format!(
        "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    // Schreibfehler (z. B. Gegenseite schon zu) zählen erst, wenn keine Antwort kam.
    let write_result = stream
        .write_all(head.as_bytes())
        .and_then(|()| stream.write_all(body.as_bytes()));
    let mut raw = Vec::new();
    // Schließt die Gegenseite, ohne die Anfrage zu lesen, kommt ein Reset; bereits
    // Empfangenes bleibt gültig. Ein abgeschnittener Rumpf wird in `parse_response`
    // über Content-Length erkannt.
    if let Err(e) = stream.read_to_end(&mut raw)
        && (raw.is_empty() || e.kind() != std::io::ErrorKind::ConnectionReset)
    {
        return Err(map_io(e));
    }
    if raw.is_empty() {
        return Err(match write_result {
            Err(e) => map_io(e),
            Ok(()) => malformed("keine Antwort"),
        });
    }
    parse_response(&raw)
}

/// `Refused` heißt ausschließlich: der Verbindungsaufbau selbst wurde abgelehnt.
fn map_connect(e: std::io::Error) -> HttpError {
    use std::io::ErrorKind;
    match e.kind() {
        ErrorKind::ConnectionRefused => HttpError::Refused,
        _ => map_io(e),
    }
}

/// Fehler nach erfolgreichem Verbindungsaufbau sind nie `Refused`.
fn map_io(e: std::io::Error) -> HttpError {
    use std::io::ErrorKind;
    match e.kind() {
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
    let mut chunked = false;
    let mut content_length: Option<usize> = None;
    for l in lines {
        let l = l.to_ascii_lowercase();
        if l.starts_with("transfer-encoding:") && l.contains("chunked") {
            chunked = true;
        } else if let Some(v) = l.strip_prefix("content-length:") {
            content_length = v.trim().parse().ok();
        }
    }
    let rest = &raw[split + 4..];
    let bytes = if chunked {
        decode_chunked(rest)?
    } else if let Some(n) = content_length {
        if rest.len() < n {
            return Err(malformed("Antwort unvollständig"));
        }
        rest[..n].to_vec()
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
