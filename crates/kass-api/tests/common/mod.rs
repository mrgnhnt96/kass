//! A stand-in Kass server on 127.0.0.1: it records each request and
//! answers with the next scripted reply.

#![allow(dead_code)]

use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::Duration;

#[derive(Debug)]
pub struct Request {
    pub method: String,
    /// The path and query, as sent.
    pub target: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Request {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    pub fn json(&self) -> serde_json::Value {
        serde_json::from_slice(&self.body).expect("a JSON body")
    }

    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
}

pub enum Reply {
    /// A whole response: status line, headers and body.
    Full {
        status: u16,
        reason: &'static str,
        content_type: &'static str,
        body: Vec<u8>,
    },
    /// Write anything to the socket after reading the request.
    Raw(Box<dyn FnOnce(TcpStream) + Send>),
}

impl Reply {
    pub fn json(status: u16, body: &str) -> Self {
        Reply::Full {
            status,
            reason: reason(status),
            content_type: "application/json",
            body: body.as_bytes().to_vec(),
        }
    }

    pub fn ok(body: &str) -> Self {
        Self::json(200, body)
    }

    pub fn no_content() -> Self {
        Reply::Full {
            status: 204,
            reason: "No Content",
            content_type: "",
            body: Vec::new(),
        }
    }

    pub fn text(status: u16, reason: &'static str, body: &str) -> Self {
        Reply::Full {
            status,
            reason,
            content_type: "text/plain",
            body: body.as_bytes().to_vec(),
        }
    }

    pub fn bytes(content_type: &'static str, body: Vec<u8>) -> Self {
        Reply::Full {
            status: 200,
            reason: "OK",
            content_type,
            body,
        }
    }

    pub fn raw(write: impl FnOnce(TcpStream) + Send + 'static) -> Self {
        Reply::Raw(Box::new(write))
    }
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        404 => "Not Found",
        409 => "Conflict",
        422 => "Unprocessable Entity",
        500 => "Internal Server Error",
        503 => "Service Unavailable",
        _ => "Status",
    }
}

pub struct Server {
    pub url: String,
    requests: Receiver<Request>,
}

impl Server {
    /// Serves one connection per reply, in order.
    pub fn start(replies: Vec<Reply>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let (sender, requests) = mpsc::channel();
        thread::spawn(move || {
            for reply in replies {
                let Ok((stream, _)) = listener.accept() else {
                    return;
                };
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let Some(request) = read_request(&mut reader) else {
                    return;
                };
                let _ = sender.send(request);
                answer(stream, reply);
            }
        });
        Self { url, requests }
    }

    pub fn request(&self) -> Request {
        self.requests
            .recv_timeout(Duration::from_secs(5))
            .expect("the client sent a request")
    }
}

fn answer(mut stream: TcpStream, reply: Reply) {
    match reply {
        Reply::Full {
            status,
            reason,
            content_type,
            body,
        } => {
            let mut head = format!(
                "HTTP/1.1 {status} {reason}\r\nContent-Length: {}\r\nConnection: close\r\n",
                body.len()
            );
            if !content_type.is_empty() {
                head.push_str(&format!("Content-Type: {content_type}\r\n"));
            }
            head.push_str("\r\n");
            let _ = stream.write_all(head.as_bytes());
            let _ = stream.write_all(&body);
            let _ = stream.flush();
        }
        Reply::Raw(write) => write(stream),
    }
}

fn read_request(reader: &mut impl BufRead) -> Option<Request> {
    let mut line = String::new();
    reader.read_line(&mut line).ok()?;
    let mut parts = line.split_whitespace();
    let method = parts.next()?.to_string();
    let target = parts.next()?.to_string();
    let mut headers = Vec::new();
    loop {
        line.clear();
        reader.read_line(&mut line).ok()?;
        let header = line.trim_end();
        if header.is_empty() {
            break;
        }
        let (name, value) = header.split_once(':')?;
        headers.push((name.trim().to_string(), value.trim().to_string()));
    }
    let length = headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
        .and_then(|(_, value)| value.parse().ok())
        .unwrap_or(0);
    let mut body = vec![0; length];
    reader.read_exact(&mut body).ok()?;
    Some(Request {
        method,
        target,
        headers,
        body,
    })
}

/// A capture as the server returns one.
pub const CAPTURE: &str = r#"{
    "id": "c-1",
    "audio_path": "/Users/me/Library/Application Support/Kass/captures/c-1.wav",
    "source": "dictation",
    "language": "en",
    "duration_ms": 4210,
    "transcript_raw": "um so lets ship it tomorrow",
    "transcript_refined": "Let's ship it tomorrow.",
    "stt_model": "turbo",
    "llm_model": "1.7B",
    "refinement_flags": {
        "smart_cleanup": true,
        "self_correction": true,
        "preserve_technical": true,
        "punctuation_style": "learned",
        "capitalize_first": true,
        "style": "s-default"
    },
    "refinement_review": null,
    "app_bundle_id": "com.tinyspeck.slackmacgap",
    "app_name": "Slack",
    "command_selection": null,
    "command_instruction": null,
    "command_transform": null,
    "style_id": "s-default",
    "audio_deleted": false,
    "mac_id": "m-1",
    "mac_name": "Studio",
    "created_at": "2026-11-13T09:12:44.512000"
}"#;
