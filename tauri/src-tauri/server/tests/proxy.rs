//! The server in front of a stand-in worker: requests, streams and
//! WebSockets reach it and come back unchanged.

use std::net::SocketAddr;
use std::time::Duration;

use axum::body::Body;
use axum::extract::ws::{CloseFrame, Message as WsMessage, WebSocket, WebSocketUpgrade};
use axum::extract::{ConnectInfo, Request};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{any, get, post};
use axum::Router;
use futures_util::{SinkExt, StreamExt};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use tokio_tungstenite::tungstenite::Message;

async fn echo_request(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    request: Request,
) -> Response {
    let method = request.method().to_string();
    let uri = request.uri().to_string();
    let body = axum::body::to_bytes(request.into_body(), usize::MAX)
        .await
        .unwrap();
    let header = |name: &str| {
        headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string()
    };
    (
        StatusCode::CREATED,
        [("x-worker", "yes")],
        axum::Json(serde_json::json!({
            "method": method,
            "uri": uri,
            "body": String::from_utf8_lossy(&body),
            "host": header("host"),
            "mac": header("x-kass-mac"),
            "forwarded_for": header("x-forwarded-for"),
            "peer_is_loopback": peer.ip().is_loopback(),
        })),
    )
        .into_response()
}

/// Sends one event, then holds the stream open.
async fn events() -> Response {
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<String, std::io::Error>>(1);
    tokio::spawn(async move {
        let _ = tx.send(Ok("data: first\n\n".into())).await;
        tokio::time::sleep(Duration::from_secs(30)).await;
        drop(tx);
    });
    let stream = tokio_stream_from(rx);
    Response::builder()
        .header("content-type", "text/event-stream")
        .body(Body::from_stream(stream))
        .unwrap()
}

fn tokio_stream_from(
    mut rx: tokio::sync::mpsc::Receiver<Result<String, std::io::Error>>,
) -> impl futures_util::Stream<Item = Result<String, std::io::Error>> {
    futures_util::stream::poll_fn(move |cx| rx.poll_recv(cx))
}

async fn stream_socket(headers: HeaderMap, upgrade: WebSocketUpgrade) -> Response {
    if headers.get("origin").map(|o| o == "https://evil.example") == Some(true) {
        return StatusCode::FORBIDDEN.into_response();
    }
    upgrade.on_upgrade(|mut socket: WebSocket| async move {
        while let Some(Ok(message)) = socket.recv().await {
            match message {
                WsMessage::Text(text) if text.as_str() == "bye" => {
                    let _ = socket
                        .send(WsMessage::Close(Some(CloseFrame {
                            code: 4001,
                            reason: "done".into(),
                        })))
                        .await;
                    return;
                }
                WsMessage::Text(text) => {
                    let _ = socket.send(WsMessage::Text(text)).await;
                }
                WsMessage::Binary(bytes) => {
                    let _ = socket
                        .send(WsMessage::Text(bytes.len().to_string().into()))
                        .await;
                }
                _ => {}
            }
        }
    })
}

async fn start_worker() -> SocketAddr {
    let app = Router::new()
        .route("/stand-in/socket", get(stream_socket))
        .route("/models/progress/{model_name}", get(events))
        .route("/upload", post(echo_request))
        .fallback(any(echo_request));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .unwrap();
    });
    address
}

async fn start_server(worker: SocketAddr) -> kass_server::Running {
    kass_server::start(kass_server::Config {
        listen: "127.0.0.1:0".parse().unwrap(),
        worker,
        database: std::env::temp_dir().join("kass-proxy-test-unused.db"),
        shared_server: None,
        voice_model: None,
    })
    .await
    .unwrap()
}

async fn raw_request(server: SocketAddr, request: &str) -> String {
    let mut stream = TcpStream::connect(server).await.unwrap();
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut response = Vec::new();
    stream.read_to_end(&mut response).await.unwrap();
    String::from_utf8_lossy(&response).into_owned()
}

fn json_body(response: &str) -> serde_json::Value {
    let body = response.split("\r\n\r\n").nth(1).unwrap();
    serde_json::from_str(body).unwrap()
}

#[tokio::test]
async fn passes_a_request_through_unchanged() {
    let server = start_server(start_worker().await).await;
    let response = raw_request(
        server.address(),
        "PATCH /models/7?mac=abc&x=%20y HTTP/1.1\r\nHost: studio.local:17493\r\n\
         X-Kass-Mac: mac-1\r\nX-Forwarded-For: 10.0.0.9\r\nContent-Length: 5\r\n\
         Connection: close\r\n\r\nhello",
    )
    .await;
    assert!(response.starts_with("HTTP/1.1 201"), "{response}");
    assert!(response.to_lowercase().contains("x-worker: yes"));
    let body = json_body(&response);
    assert_eq!(body["method"], "PATCH");
    assert_eq!(body["uri"], "/models/7?mac=abc&x=%20y");
    assert_eq!(body["body"], "hello");
    assert_eq!(body["host"], "studio.local:17493");
    assert_eq!(body["mac"], "mac-1");
    // The real client, not what it claimed.
    assert_eq!(body["forwarded_for"], "127.0.0.1");
    assert_eq!(body["peer_is_loopback"], true);
}

#[tokio::test]
async fn passes_a_path_no_module_lists() {
    let server = start_server(start_worker().await).await;
    let response = raw_request(
        server.address(),
        "OPTIONS /dictionary/ HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n",
    )
    .await;
    let body = json_body(&response);
    assert_eq!(body["method"], "OPTIONS");
    assert_eq!(body["uri"], "/dictionary/");
}

#[tokio::test]
async fn streams_events_as_they_come() {
    let server = start_server(start_worker().await).await;
    let mut stream = TcpStream::connect(server.address()).await.unwrap();
    stream
        .write_all(b"GET /models/progress/turbo HTTP/1.1\r\nHost: x\r\n\r\n")
        .await
        .unwrap();
    let mut seen = Vec::new();
    let mut buffer = [0u8; 1024];
    tokio::time::timeout(Duration::from_secs(5), async {
        while !String::from_utf8_lossy(&seen).contains("data: first") {
            let n = stream.read(&mut buffer).await.unwrap();
            assert!(n > 0, "stream ended early");
            seen.extend_from_slice(&buffer[..n]);
        }
    })
    .await
    .expect("the first event arrived while the stream was still open");
}

#[tokio::test]
async fn relays_a_websocket_with_its_close_code() {
    let server = start_server(start_worker().await).await;
    let url = format!("ws://{}/stand-in/socket?mac=abc", server.address());
    let (mut socket, _) = tokio_tungstenite::connect_async(url).await.unwrap();
    socket.send(Message::text("hello")).await.unwrap();
    assert_eq!(
        socket.next().await.unwrap().unwrap(),
        Message::text("hello")
    );
    socket
        .send(Message::binary(vec![0u8; 64_000]))
        .await
        .unwrap();
    assert_eq!(
        socket.next().await.unwrap().unwrap(),
        Message::text("64000")
    );
    socket.send(Message::text("bye")).await.unwrap();
    match socket.next().await.unwrap().unwrap() {
        Message::Close(Some(frame)) => {
            assert_eq!(frame.code, CloseCode::from(4001));
            assert_eq!(frame.reason.as_str(), "done");
        }
        other => panic!("expected a close, got {other:?}"),
    }
}

#[tokio::test]
async fn passes_on_a_refused_websocket() {
    let server = start_server(start_worker().await).await;
    let mut request = format!("ws://{}/stand-in/socket", server.address())
        .into_client_request()
        .unwrap();
    request
        .headers_mut()
        .insert("origin", "https://evil.example".parse().unwrap());
    match tokio_tungstenite::connect_async(request).await {
        Err(tokio_tungstenite::tungstenite::Error::Http(response)) => {
            assert_eq!(response.status(), StatusCode::FORBIDDEN);
        }
        other => panic!("expected the worker's refusal, got {other:?}"),
    }
}

#[tokio::test]
async fn answers_bad_gateway_while_the_worker_is_down() {
    let unused = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let worker = unused.local_addr().unwrap();
    drop(unused);
    let server = start_server(worker).await;
    let response = raw_request(
        server.address(),
        "GET /health HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n",
    )
    .await;
    assert!(response.starts_with("HTTP/1.1 502"), "{response}");
}

#[tokio::test]
async fn frees_the_port_when_stopped() {
    let server = start_server(start_worker().await).await;
    let address = server.address();
    server.stop();
    tokio::time::timeout(Duration::from_secs(5), async {
        while TcpStream::connect(address).await.is_ok() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the port closed");
    TcpListener::bind(address).await.expect("the port is free");
}
