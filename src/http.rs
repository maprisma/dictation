//! A tiny HTTP/1.1 client for the local whisper-server only (127.0.0.1, plain TCP), and
//! multipart/form-data bodies. Online engines use curl (TLS, key through stdin).

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::time::Duration;

pub enum Part<'a> {
    Text(&'a str, &'a str),
    /// name, file name, content type, bytes
    File(&'a str, &'a str, &'a str, &'a [u8]),
}

/// A multipart body and its content type.
pub fn multipart(parts: &[Part<'_>]) -> (Vec<u8>, String) {
    let boundary = format!("maprisma-voice-{:x}-{:x}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0));
    let mut body = Vec::new();
    for part in parts {
        body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
        match part {
            Part::Text(name, value) => {
                body.extend_from_slice(format!("Content-Disposition: form-data; name=\"{name}\"\r\n\r\n{value}\r\n").as_bytes());
            }
            Part::File(name, file, kind, bytes) => {
                body.extend_from_slice(format!("Content-Disposition: form-data; name=\"{name}\"; filename=\"{file}\"\r\nContent-Type: {kind}\r\n\r\n").as_bytes());
                body.extend_from_slice(bytes);
                body.extend_from_slice(b"\r\n");
            }
        }
    }
    body.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
    (body, format!("multipart/form-data; boundary={boundary}"))
}

#[derive(Debug)]
pub struct Response {
    pub status: u16,
    pub body: Vec<u8>,
}

/// One request (`Connection: close`) to a loopback address.
pub fn request(addr: SocketAddr, method: &str, path: &str, content_type: Option<&str>, body: &[u8], timeout: Duration) -> std::io::Result<Response> {
    if !addr.ip().is_loopback() {
        return Err(std::io::Error::other("only loopback addresses"));
    }
    let mut stream = TcpStream::connect_timeout(&addr, Duration::from_secs(2).min(timeout))?;
    stream.set_read_timeout(Some(timeout))?;
    stream.set_write_timeout(Some(timeout))?;
    let mut head = format!("{method} {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\nAccept: application/json\r\nContent-Length: {}\r\n", body.len());
    if let Some(kind) = content_type {
        head.push_str(&format!("Content-Type: {kind}\r\n"));
    }
    head.push_str("\r\n");
    stream.write_all(head.as_bytes())?;
    stream.write_all(body)?;
    stream.flush()?;
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw)?;
    parse_response(&raw)
}

pub fn parse_response(raw: &[u8]) -> std::io::Result<Response> {
    let bad = || std::io::Error::new(std::io::ErrorKind::InvalidData, "bad HTTP response");
    let split = raw.windows(4).position(|w| w == b"\r\n\r\n").ok_or_else(bad)?;
    let head = std::str::from_utf8(&raw[..split]).map_err(|_| bad())?;
    let mut lines = head.split("\r\n");
    let status: u16 = lines.next().and_then(|l| l.split_whitespace().nth(1)).and_then(|s| s.parse().ok()).ok_or_else(bad)?;
    let mut chunked = false;
    let mut length: Option<usize> = None;
    for line in lines {
        if let Some((k, v)) = line.split_once(':') {
            let (k, v) = (k.trim().to_ascii_lowercase(), v.trim());
            if k == "transfer-encoding" && v.to_ascii_lowercase().contains("chunked") {
                chunked = true;
            } else if k == "content-length" {
                length = v.parse().ok();
            }
        }
    }
    let rest = &raw[split + 4..];
    let body = if chunked {
        let mut out = Vec::new();
        let mut pos = 0;
        loop {
            let end = rest[pos..].windows(2).position(|w| w == b"\r\n").ok_or_else(bad)? + pos;
            let size_text = std::str::from_utf8(&rest[pos..end]).map_err(|_| bad())?;
            let size = usize::from_str_radix(size_text.split(';').next().unwrap_or("").trim(), 16).map_err(|_| bad())?;
            pos = end + 2;
            if size == 0 {
                break;
            }
            out.extend_from_slice(rest.get(pos..pos + size).ok_or_else(bad)?);
            pos += size + 2;
        }
        out
    } else {
        match length {
            Some(n) => rest.get(..n).ok_or_else(bad)?.to_vec(),
            None => rest.to_vec(),
        }
    };
    Ok(Response { status, body })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn responses() {
        let r = parse_response(b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nhello").unwrap();
        assert_eq!((r.status, r.body.as_slice()), (200, &b"hello"[..]));
        let r = parse_response(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n3\r\nabc\r\n2;x=y\r\nde\r\n0\r\n\r\n").unwrap();
        assert_eq!(r.body, b"abcde");
        let r = parse_response(b"HTTP/1.0 503 Busy\r\n\r\nloading").unwrap();
        assert_eq!((r.status, r.body.as_slice()), (503, &b"loading"[..]));
        assert!(parse_response(b"garbage").is_err());
    }

    #[test]
    fn multipart_body() {
        let (body, kind) = multipart(&[Part::Text("language", "de"), Part::File("file", "a.wav", "audio/wav", b"RIFF..")]);
        let boundary = kind.strip_prefix("multipart/form-data; boundary=").unwrap();
        let text = String::from_utf8_lossy(&body);
        assert!(text.starts_with(&format!("--{boundary}\r\n")));
        assert!(text.contains("name=\"language\"\r\n\r\nde\r\n"));
        assert!(text.contains("name=\"file\"; filename=\"a.wav\"\r\nContent-Type: audio/wav\r\n\r\nRIFF..\r\n"));
        assert!(text.ends_with(&format!("--{boundary}--\r\n")));
    }

    #[test]
    fn refuses_non_loopback() {
        let addr: SocketAddr = "192.0.2.1:80".parse().unwrap();
        assert!(request(addr, "GET", "/", None, b"", Duration::from_millis(100)).is_err());
    }
}
