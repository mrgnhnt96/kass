//! Opening an event stream. Over http the request is written by hand on a
//! socket the cancel handle can shut, so a blocked read ends at once; ureq
//! doesn't hand out its socket. Over https it goes through ureq, and a
//! cancelled stream ends at the next event or keepalive.

use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::time::Duration;

use url::{Host, Url};

use super::{SseCancel, SseReader, SseStream};
use crate::error::{Error, Result};

/// The most of an error response read for its message.
const ERROR_BODY_LIMIT: u64 = 64 * 1024;

pub(crate) fn open(
    url: &str,
    headers: &[(&str, String)],
    connect_timeout: Duration,
    cancel: &SseCancel,
) -> Result<SseStream> {
    let parsed = Url::parse(url).map_err(Error::transport)?;
    let opened = match parsed.scheme() {
        _ if cancel.is_cancelled() => Ok(ended()),
        "http" => open_http(&parsed, headers, connect_timeout, cancel),
        _ => open_ureq(url, headers, connect_timeout),
    };
    let reader = match opened {
        Err(_) if cancel.is_cancelled() => ended(),
        opened => opened?,
    };
    Ok(SseStream::new(SseReader::new(reader), cancel.clone()))
}

fn ended() -> Box<dyn BufRead + Send> {
    Box::new(io::empty())
}

fn open_ureq(
    url: &str,
    headers: &[(&str, String)],
    connect_timeout: Duration,
) -> Result<Box<dyn BufRead + Send>> {
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(connect_timeout)
        .build();
    let mut request = agent
        .get(url)
        .set("Accept", "text/event-stream")
        .set("Cache-Control", "no-cache");
    for (name, value) in headers {
        request = request.set(name, value);
    }
    let response = request.call()?;
    Ok(Box::new(BufReader::new(response.into_reader())))
}

fn open_http(
    url: &Url,
    headers: &[(&str, String)],
    connect_timeout: Duration,
    cancel: &SseCancel,
) -> Result<Box<dyn BufRead + Send>> {
    let port = url.port_or_known_default().unwrap_or(80);
    let addrs: Vec<SocketAddr> = match url.host() {
        Some(Host::Domain(domain)) => (domain, port).to_socket_addrs()?.collect(),
        Some(Host::Ipv4(ip)) => vec![SocketAddr::new(ip.into(), port)],
        Some(Host::Ipv6(ip)) => vec![SocketAddr::new(ip.into(), port)],
        None => return Err(Error::transport(io::Error::other("no host"))),
    };
    let mut stream = connect(&addrs, connect_timeout)?;
    stream.set_write_timeout(Some(connect_timeout))?;
    cancel.set_socket(stream.try_clone()?);

    let host = url.host_str().unwrap_or_default();
    let host = match url.port() {
        Some(port) => format!("{host}:{port}"),
        None => host.to_string(),
    };
    let target = match url.query() {
        Some(query) => format!("{}?{query}", url.path()),
        None => url.path().to_string(),
    };
    let mut request = format!(
        "GET {target} HTTP/1.1\r\nHost: {host}\r\nAccept: text/event-stream\r\n\
         Cache-Control: no-cache\r\nConnection: close\r\n"
    );
    for (name, value) in headers {
        request.push_str(&format!("{name}: {value}\r\n"));
    }
    request.push_str("\r\n");
    stream.write_all(request.as_bytes())?;

    let mut reader = BufReader::new(stream);
    let head = read_head(&mut reader)?;
    let body: Box<dyn BufRead + Send> = if head.chunked {
        Box::new(BufReader::new(Chunked::new(reader)))
    } else if let Some(length) = head.content_length {
        Box::new(reader.take(length))
    } else {
        Box::new(reader)
    };
    if head.status != 200 {
        let mut text = Vec::new();
        body.take(ERROR_BODY_LIMIT).read_to_end(&mut text)?;
        return Err(Error::from_response(head.status, &head.reason, &text));
    }
    Ok(body)
}

fn connect(addrs: &[SocketAddr], timeout: Duration) -> io::Result<TcpStream> {
    let mut last = io::Error::new(io::ErrorKind::NotFound, "the address didn't resolve");
    for addr in addrs {
        match TcpStream::connect_timeout(addr, timeout) {
            Ok(stream) => return Ok(stream),
            Err(error) => last = error,
        }
    }
    Err(last)
}

struct Head {
    status: u16,
    reason: String,
    chunked: bool,
    content_length: Option<u64>,
}

fn read_head(reader: &mut impl BufRead) -> io::Result<Head> {
    let invalid = |what: &str| io::Error::new(io::ErrorKind::InvalidData, what.to_string());
    let mut line = String::new();
    reader.read_line(&mut line)?;
    let mut parts = line.trim_end().splitn(3, ' ');
    let version = parts.next().unwrap_or_default();
    if !version.starts_with("HTTP/") {
        return Err(invalid("not an HTTP response"));
    }
    let status = parts
        .next()
        .and_then(|code| code.parse().ok())
        .ok_or_else(|| invalid("no HTTP status"))?;
    let mut head = Head {
        status,
        reason: parts.next().unwrap_or_default().to_string(),
        chunked: false,
        content_length: None,
    };
    loop {
        line.clear();
        if reader.read_line(&mut line)? == 0 {
            return Err(invalid("the response ended in its headers"));
        }
        let line = line.trim_end();
        if line.is_empty() {
            return Ok(head);
        }
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim();
        if name.eq_ignore_ascii_case("transfer-encoding") {
            head.chunked = value.to_ascii_lowercase().contains("chunked");
        } else if name.eq_ignore_ascii_case("content-length") {
            head.content_length = value.parse().ok();
        }
    }
}

/// A `Transfer-Encoding: chunked` body.
struct Chunked<R> {
    inner: R,
    remaining: u64,
    done: bool,
}

impl<R: BufRead> Chunked<R> {
    fn new(inner: R) -> Self {
        Self {
            inner,
            remaining: 0,
            done: false,
        }
    }
}

impl<R: BufRead> Read for Chunked<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.done || buf.is_empty() {
            return Ok(0);
        }
        if self.remaining == 0 {
            let mut line = String::new();
            if self.inner.read_line(&mut line)? == 0 {
                self.done = true;
                return Ok(0);
            }
            let size = line.trim().split(';').next().unwrap_or_default().trim();
            self.remaining = u64::from_str_radix(size, 16)
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "bad chunk size"))?;
            if self.remaining == 0 {
                self.done = true;
                return Ok(0);
            }
        }
        let most = buf
            .len()
            .min(usize::try_from(self.remaining).unwrap_or(usize::MAX));
        let read = self.inner.read(&mut buf[..most])?;
        if read == 0 {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        self.remaining -= read as u64;
        if self.remaining == 0 {
            let mut crlf = String::new();
            self.inner.read_line(&mut crlf)?;
        }
        Ok(read)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_chunks() {
        let body = "5\r\nhello\r\n7;ext=1\r\n, world\r\n0\r\n\r\n";
        let mut text = String::new();
        Chunked::new(body.as_bytes())
            .read_to_string(&mut text)
            .unwrap();
        assert_eq!(text, "hello, world");
    }

    #[test]
    fn reads_the_head() {
        let mut reader = "HTTP/1.1 404 Not Found\r\ncontent-length: 2\r\n\r\n{}".as_bytes();
        let head = read_head(&mut reader).unwrap();
        assert_eq!((head.status, head.reason.as_str()), (404, "Not Found"));
        assert_eq!(head.content_length, Some(2));
        assert!(!head.chunked);
    }
}
