//! WebSocket transport for `/captures/stream`, bridged to the driver's
//! channels. Connection starts at key-down; the driver buffers audio until
//! the server says `ready`.

use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::Message;

use super::client::Action;
use super::server::TakeServer;
use super::stream::Incoming;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
/// How long another Mac's server has to answer before a take moves to this
/// Mac's own.
const FALLBACK_CONNECT_TIMEOUT: Duration = Duration::from_millis(1500);

/// `http(s)://host:port` → `ws(s)://host:port/captures/stream`.
pub fn stream_url(server_url: &str) -> String {
    let base = server_url.trim_end_matches('/');
    let base = if let Some(rest) = base.strip_prefix("https://") {
        format!("wss://{rest}")
    } else if let Some(rest) = base.strip_prefix("http://") {
        format!("ws://{rest}")
    } else {
        base.to_string()
    };
    format!("{base}/captures/stream")
}

/// Whether the server URL points at this machine. The server accepts
/// originless (native) WebSocket clients only over loopback.
pub fn is_loopback(server_url: &str) -> bool {
    let without_scheme = server_url
        .split_once("://")
        .map(|(_, rest)| rest)
        .unwrap_or(server_url);
    let authority = without_scheme.split('/').next().unwrap_or("");
    let host = if let Some(rest) = authority.strip_prefix('[') {
        rest.split(']').next().unwrap_or("")
    } else {
        authority
            .rsplit_once(':')
            .map(|(h, _)| h)
            .unwrap_or(authority)
    };
    host == "localhost"
        || host
            .parse::<std::net::IpAddr>()
            .map(|ip| ip.is_loopback())
            .unwrap_or(false)
}

/// Connect in the background. Returns the send side and the event side.
/// `origin` is sent only for a remote server, where it must be the app's own
/// webview origin (which the server's allowlist already trusts); loopback
/// connections stay originless.
#[cfg(test)]
pub fn spawn(
    server_url: &str,
    origin: Option<String>,
) -> (UnboundedSender<Action>, UnboundedReceiver<Incoming>) {
    spawn_for(
        Arc::new(TakeServer::new(server_url.to_string(), None)),
        origin,
    )
}

type Socket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn connect(
    server_url: &str,
    origin: Option<String>,
    timeout: Duration,
) -> Result<Socket, String> {
    let url = stream_url(server_url);
    let origin = if is_loopback(server_url) {
        None
    } else {
        origin
    };
    let mut request = url
        .as_str()
        .into_client_request()
        .map_err(|e| format!("invalid stream URL {url}: {e}"))?;
    if let Some(origin) = origin.and_then(|o| HeaderValue::from_str(&o).ok()) {
        request.headers_mut().insert("Origin", origin);
    }
    for (name, value) in crate::this_mac::headers() {
        if let Ok(value) = HeaderValue::from_str(&value) {
            request.headers_mut().insert(name, value);
        }
    }
    let connect = tokio_tungstenite::connect_async_with_config(request, None, true);
    match tokio::time::timeout(timeout, connect).await {
        Ok(Ok((socket, _))) => Ok(socket),
        Ok(Err(e)) => Err(format!("stream connect to {server_url} failed: {e}")),
        Err(_) => Err(format!("stream connect to {server_url} timed out")),
    }
}

/// [`spawn`] for a take that can fall back to this Mac's own server: when
/// the server in use doesn't answer, the take connects there instead, its
/// audio still buffered, and its other requests follow ([`TakeServer`]).
pub fn spawn_for(
    server: Arc<TakeServer>,
    origin: Option<String>,
) -> (UnboundedSender<Action>, UnboundedReceiver<Incoming>) {
    let (out_tx, mut out_rx) = unbounded_channel::<Action>();
    let (in_tx, in_rx) = unbounded_channel::<Incoming>();
    tauri::async_runtime::spawn(async move {
        // Another Mac on the network answers in milliseconds; don't make a
        // take wait the full timeout when there's a server standing by.
        let timeout = if server.can_fall_back() {
            FALLBACK_CONNECT_TIMEOUT
        } else {
            CONNECT_TIMEOUT
        };
        let socket = match connect(&server.url(), origin.clone(), timeout).await {
            Ok(socket) => Ok(socket),
            Err(e) => match server.fall_back() {
                Some(fallback) => {
                    eprintln!("[dictation] {e}; dictating on this Mac instead");
                    connect(&fallback, origin, CONNECT_TIMEOUT).await
                }
                None => Err(e),
            },
        };
        let socket = match socket {
            Ok(socket) => socket,
            Err(e) => {
                eprintln!("[dictation] {e}");
                let _ = in_tx.send(Incoming::Closed);
                return;
            }
        };
        let _ = in_tx.send(Incoming::Open);
        let (mut sink, mut source) = socket.split();
        loop {
            tokio::select! {
                action = out_rx.recv() => {
                    let message = match action {
                        Some(Action::Text(text)) => Message::Text(text.into()),
                        Some(Action::Binary(bytes)) => Message::Binary(bytes.into()),
                        None => {
                            // The driver settled: close politely.
                            let _ = sink.close().await;
                            break;
                        }
                    };
                    if let Err(e) = sink.send(message).await {
                        eprintln!("[dictation] stream send failed: {e}");
                        let _ = in_tx.send(Incoming::Closed);
                        break;
                    }
                }
                message = source.next() => match message {
                    Some(Ok(Message::Text(text))) => {
                        let _ = in_tx.send(Incoming::Text(text.to_string()));
                    }
                    Some(Ok(Message::Close(_))) | None | Some(Err(_)) => {
                        let _ = in_tx.send(Incoming::Closed);
                        break;
                    }
                    Some(Ok(_)) => {}
                },
            }
        }
    });
    (out_tx, in_rx)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stream_url_from_server_url() {
        assert_eq!(
            stream_url("http://127.0.0.1:17493/"),
            "ws://127.0.0.1:17493/captures/stream"
        );
        assert_eq!(
            stream_url("https://voice.example.com"),
            "wss://voice.example.com/captures/stream"
        );
    }

    /// Real socket, fake server: the transport and driver together deliver
    /// audio recorded before `ready`, then `finish`, then return `final`.
    #[tokio::test]
    // tungstenite's header callback must return its large error type.
    #[allow(clippy::result_large_err)]
    async fn streams_a_take_over_a_real_loopback_socket() {
        use crate::dictation::client::{Outcome, StreamClient};
        use crate::dictation::protocol;
        use crate::dictation::stream::{drive, AudioMsg, Timeouts};
        use tokio_tungstenite::tungstenite::handshake::server::{Request, Response};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.unwrap();
            let mut origin = None;
            let mut socket =
                tokio_tungstenite::accept_hdr_async(tcp, |req: &Request, res: Response| {
                    origin = req
                        .headers()
                        .get("origin")
                        .map(|v| v.to_str().unwrap().to_string());
                    Ok(res)
                })
                .await
                .unwrap();
            let start = socket.next().await.unwrap().unwrap().into_text().unwrap();
            // Connect slowly: audio must be buffered meanwhile.
            tokio::time::sleep(Duration::from_millis(50)).await;
            socket
                .send(Message::Text(
                    r#"{"type":"ready","session_id":"s1"}"#.into(),
                ))
                .await
                .unwrap();
            let mut samples = 0usize;
            let mut sequences = Vec::new();
            loop {
                match socket.next().await.unwrap().unwrap() {
                    Message::Binary(frame) => {
                        sequences.push(u32::from_le_bytes(frame[0..4].try_into().unwrap()));
                        let offset = u32::from_le_bytes(frame[4..8].try_into().unwrap());
                        assert_eq!(offset as usize, samples);
                        samples += (frame.len() - protocol::HEADER_BYTES) / 2;
                    }
                    Message::Text(text) => {
                        assert_eq!(text.as_str(), protocol::finish_message(None));
                        break;
                    }
                    other => panic!("unexpected {other:?}"),
                }
            }
            let final_event = serde_json::json!({
                "type": "final",
                "refinement_complete": true,
                "capture": {"id": "s1", "transcript_raw": "hi"}
            });
            socket
                .send(Message::Text(final_event.to_string().into()))
                .await
                .unwrap();
            (origin, start.to_string(), samples, sequences)
        });

        let (out, incoming) = spawn(
            &format!("http://127.0.0.1:{port}"),
            Some("tauri://localhost".into()),
        );
        let (audio_tx, audio_rx) = tokio::sync::mpsc::unbounded_channel();
        audio_tx.send(AudioMsg::Format(16_000)).unwrap();
        for _ in 0..3 {
            audio_tx.send(AudioMsg::Frame(vec![5; 1600])).unwrap();
        }
        audio_tx.send(AudioMsg::Frame(vec![5; 100])).unwrap();
        audio_tx.send(AudioMsg::End).unwrap();
        let outcome = drive(
            StreamClient::new(1 << 20),
            out,
            incoming,
            audio_rx,
            Timeouts::default(),
            std::future::pending(),
        )
        .await;
        assert!(matches!(outcome, Outcome::Final(_)));
        let (origin, start, samples, sequences) = server.await.unwrap();
        assert_eq!(origin, None, "loopback clients must be originless");
        assert!(start.contains("\"sample_rate\":16000"));
        assert_eq!(samples, 4900);
        assert_eq!(sequences, vec![0, 1, 2, 3]);
    }

    /// Opt-in smoke test against a real Kass server. Streams a mono
    /// 16-bit WAV in real time, as the microphone would, and reports the
    /// release-to-final time. Never point it at a server holding real data:
    /// it saves a capture.
    ///
    /// KASS_SMOKE_SERVER=http://127.0.0.1:17593 KASS_SMOKE_WAV=/path.wav \
    ///   cargo test --bin kass real_server_smoke -- --ignored --nocapture
    #[tokio::test]
    #[ignore]
    async fn real_server_smoke() {
        use crate::dictation::client::{Outcome, StreamClient};
        use crate::dictation::stream::{drive, AudioMsg, Timeouts};

        let server = std::env::var("KASS_SMOKE_SERVER").expect("KASS_SMOKE_SERVER");
        let wav = std::env::var("KASS_SMOKE_WAV").expect("KASS_SMOKE_WAV");
        let mut reader = hound::WavReader::open(wav).unwrap();
        let rate = reader.spec().sample_rate;
        assert_eq!(reader.spec().channels, 1);
        let pcm: Vec<i16> = reader.samples::<i16>().map(Result::unwrap).collect();

        let keydown = std::time::Instant::now();
        let (out, incoming) = spawn(&server, None);
        let (audio_tx, audio_rx) = tokio::sync::mpsc::unbounded_channel();
        let driver = tokio::spawn(drive(
            StreamClient::new(1 << 24),
            out,
            incoming,
            audio_rx,
            Timeouts::default(),
            std::future::pending(),
        ));
        audio_tx.send(AudioMsg::Format(rate)).unwrap();
        let frame = (rate / 10) as usize;
        for chunk in pcm.chunks(frame) {
            audio_tx.send(AudioMsg::Frame(chunk.to_vec())).unwrap();
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        let released = std::time::Instant::now();
        audio_tx.send(AudioMsg::End).unwrap();
        let outcome = driver.await.unwrap();
        let release_to_final = released.elapsed();
        match outcome {
            Outcome::Final(event) => {
                eprintln!(
                    "[smoke] audio {:.2}s wall {:.2}s release→final {:.0}ms samples {}",
                    pcm.len() as f64 / rate as f64,
                    released.duration_since(keydown).as_secs_f64(),
                    release_to_final.as_secs_f64() * 1000.0,
                    event["covered_samples"],
                );
                eprintln!("[smoke] raw: {}", event["capture"]["transcript_raw"]);
                eprintln!(
                    "[smoke] refined: {}",
                    event["capture"]["transcript_refined"]
                );
                assert_eq!(event["covered_samples"].as_u64(), Some(pcm.len() as u64));
            }
            other => panic!("unexpected outcome {other:?}"),
        }
    }

    /// Another Mac's server that refuses the take: it connects to the server
    /// standing by on this Mac instead, and the take's other requests follow.
    #[tokio::test]
    async fn falls_back_to_this_macs_server_when_the_other_doesnt_answer() {
        let refused = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let away = format!("http://127.0.0.1:{}", refused.local_addr().unwrap().port());
        drop(refused);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let here = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
        let accepted = tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.unwrap();
            tokio_tungstenite::accept_async(tcp).await.unwrap()
        });

        let server = Arc::new(TakeServer::new(away, Some(here.clone())));
        let (_out, mut incoming) = spawn_for(server.clone(), None);

        assert!(matches!(incoming.recv().await, Some(Incoming::Open)));
        accepted.await.unwrap();
        assert_eq!(server.url(), here);
    }

    #[test]
    fn loopback_detection() {
        assert!(is_loopback("http://127.0.0.1:17493"));
        assert!(is_loopback("http://localhost:17493"));
        assert!(is_loopback("http://[::1]:17493"));
        assert!(!is_loopback("http://192.168.1.4:17493"));
        assert!(!is_loopback("https://voice.example.com"));
        assert!(!is_loopback("http://localhost.evil.com:17493"));
    }
}
