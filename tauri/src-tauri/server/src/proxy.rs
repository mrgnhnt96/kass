//! Passes requests the Rust server doesn't answer yet to the Python model
//! worker, unchanged: same method, path, query, headers and body, streamed
//! both ways (server-sent events and uploads included). A WebSocket upgrade
//! is relayed as raw bytes once both sides switch protocols, so frames, close
//! codes and a refused handshake reach the client exactly as the worker sent
//! them.
//!
//! The worker sees every connection come from loopback, so the proxy tells it
//! the real client in `X-Forwarded-For`, replacing whatever the client sent.
//! Uvicorn trusts that header from loopback only, which keeps the worker's
//! own checks (originless WebSockets only from this Mac) working as before.

use std::net::SocketAddr;
use std::time::Duration;

use axum::body::Body;
use axum::extract::{ConnectInfo, Request, State};
use axum::http::header::{self, HeaderMap, HeaderName, HeaderValue};
use axum::http::{StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::routing::{any, MethodRouter};
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::client::legacy::Client;
use hyper_util::rt::{TokioExecutor, TokioIo};
use tokio::net::TcpStream;

use crate::AppState;

/// Uvicorn closes a connection that's idle for 5 seconds; dropping ours well
/// before that means a request never lands on one it's closing.
const IDLE_CONNECTION: Duration = Duration::from_secs(1);

const X_FORWARDED_FOR: HeaderName = HeaderName::from_static("x-forwarded-for");
const X_FORWARDED_PROTO: HeaderName = HeaderName::from_static("x-forwarded-proto");

/// Headers about one hop's connection, never passed on (RFC 9110 §7.6.1).
const HOP_BY_HOP: [HeaderName; 8] = [
    header::CONNECTION,
    HeaderName::from_static("keep-alive"),
    HeaderName::from_static("proxy-connection"),
    header::PROXY_AUTHENTICATE,
    header::PROXY_AUTHORIZATION,
    header::TE,
    header::TRAILER,
    header::TRANSFER_ENCODING,
];

#[derive(Clone)]
pub struct Proxy {
    worker: SocketAddr,
    client: Client<HttpConnector, Body>,
}

impl Proxy {
    pub fn new(worker: SocketAddr) -> Self {
        let mut connector = HttpConnector::new();
        connector.set_nodelay(true);
        let client = Client::builder(TokioExecutor::new())
            .pool_idle_timeout(IDLE_CONNECTION)
            .build(connector);
        Self { worker, client }
    }

    pub fn worker(&self) -> SocketAddr {
        self.worker
    }

    /// The pooled connections to the worker, for calls of this server's own.
    pub(crate) fn client(&self) -> &Client<HttpConnector, Body> {
        &self.client
    }
}

/// A route still answered by the worker, for every method: CORS preflights
/// and methods it doesn't allow get the worker's own answers.
pub fn to_worker() -> MethodRouter<AppState> {
    any(forward)
}

/// Send the request to the worker and return its response.
pub async fn forward(
    State(state): State<AppState>,
    ConnectInfo(client): ConnectInfo<SocketAddr>,
    request: Request,
) -> Response {
    let proxy = &state.proxy;
    let result = if is_upgrade(request.headers()) {
        upgrade(proxy, client, request).await
    } else {
        http(proxy, client, request).await
    };
    result.unwrap_or_else(|error| {
        eprintln!("[server] the model worker didn't answer: {error}");
        (
            StatusCode::BAD_GATEWAY,
            axum::Json(serde_json::json!({ "detail": "The Kass model worker isn't answering" })),
        )
            .into_response()
    })
}

type BoxError = Box<dyn std::error::Error + Send + Sync>;

async fn http(proxy: &Proxy, client: SocketAddr, request: Request) -> Result<Response, BoxError> {
    let (mut parts, body) = request.into_parts();
    parts.uri = worker_uri(proxy.worker, &parts.uri)?;
    strip_hop_by_hop(&mut parts.headers);
    parts.headers.remove(header::UPGRADE);
    set_forwarded(&mut parts.headers, client);
    let response = proxy
        .client
        .request(Request::from_parts(parts, body))
        .await?;
    let (mut parts, body) = response.into_parts();
    strip_hop_by_hop(&mut parts.headers);
    Ok(Response::from_parts(parts, Body::new(body)))
}

/// Open the upgrade with the worker first; only when it switches protocols
/// does the client switch too, and from then on bytes pass straight through.
async fn upgrade(
    proxy: &Proxy,
    client: SocketAddr,
    mut request: Request,
) -> Result<Response, BoxError> {
    let client_upgrade = hyper::upgrade::on(&mut request);
    let (mut parts, _) = request.into_parts();
    let protocol = parts.headers.get(header::UPGRADE).cloned();
    strip_hop_by_hop(&mut parts.headers);
    set_forwarded(&mut parts.headers, client);
    if let Some(protocol) = protocol {
        parts
            .headers
            .insert(header::CONNECTION, HeaderValue::from_static("upgrade"));
        parts.headers.insert(header::UPGRADE, protocol);
    }
    parts.uri = parts
        .uri
        .path_and_query()
        .map(|p| p.as_str())
        .unwrap_or("/")
        .parse()?;

    let stream = TcpStream::connect(proxy.worker).await?;
    stream.set_nodelay(true)?;
    let (mut sender, connection) =
        hyper::client::conn::http1::handshake(TokioIo::new(stream)).await?;
    tokio::spawn(async move {
        let _ = connection.with_upgrades().await;
    });
    let mut response = sender
        .send_request(Request::from_parts(parts, Body::empty()))
        .await?;

    if response.status() != StatusCode::SWITCHING_PROTOCOLS {
        let (mut parts, body) = response.into_parts();
        strip_hop_by_hop(&mut parts.headers);
        return Ok(Response::from_parts(parts, Body::new(body)));
    }

    let worker_upgrade = hyper::upgrade::on(&mut response);
    tokio::spawn(async move {
        let (Ok(client_io), Ok(worker_io)) = tokio::join!(client_upgrade, worker_upgrade) else {
            return;
        };
        let _ = tokio::io::copy_bidirectional(
            &mut TokioIo::new(client_io),
            &mut TokioIo::new(worker_io),
        )
        .await;
    });
    let (parts, _) = response.into_parts();
    Ok(Response::from_parts(parts, Body::empty()))
}

fn is_upgrade(headers: &HeaderMap) -> bool {
    headers.contains_key(header::UPGRADE)
        && headers
            .get_all(header::CONNECTION)
            .iter()
            .filter_map(|v| v.to_str().ok())
            .flat_map(|v| v.split(','))
            .any(|token| token.trim().eq_ignore_ascii_case("upgrade"))
}

fn worker_uri(worker: SocketAddr, uri: &Uri) -> Result<Uri, BoxError> {
    let path = uri.path_and_query().map(|p| p.as_str()).unwrap_or("/");
    Ok(format!("http://{worker}{path}").parse()?)
}

/// Remove hop-by-hop headers, including any the `Connection` header names.
fn strip_hop_by_hop(headers: &mut HeaderMap) {
    let named: Vec<HeaderName> = headers
        .get_all(header::CONNECTION)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .filter_map(|token| HeaderName::from_bytes(token.trim().as_bytes()).ok())
        .collect();
    for name in named.iter().chain(HOP_BY_HOP.iter()) {
        headers.remove(name);
    }
}

fn set_forwarded(headers: &mut HeaderMap, client: SocketAddr) {
    let address = client.ip().to_canonical().to_string();
    if let Ok(value) = HeaderValue::from_str(&address) {
        headers.insert(X_FORWARDED_FOR, value);
    }
    headers.insert(X_FORWARDED_PROTO, HeaderValue::from_static("http"));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spots_a_websocket_upgrade() {
        let mut headers = HeaderMap::new();
        headers.insert(header::UPGRADE, HeaderValue::from_static("websocket"));
        headers.insert(
            header::CONNECTION,
            HeaderValue::from_static("keep-alive, Upgrade"),
        );
        assert!(is_upgrade(&headers));
        headers.insert(header::CONNECTION, HeaderValue::from_static("keep-alive"));
        assert!(!is_upgrade(&headers));
    }

    #[test]
    fn drops_hop_by_hop_headers_and_the_ones_connection_names() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::CONNECTION,
            HeaderValue::from_static("close, x-private"),
        );
        headers.insert("x-private", HeaderValue::from_static("1"));
        headers.insert(
            header::TRANSFER_ENCODING,
            HeaderValue::from_static("chunked"),
        );
        headers.insert("x-kass-mac", HeaderValue::from_static("abc"));
        strip_hop_by_hop(&mut headers);
        assert_eq!(headers.len(), 1);
        assert_eq!(headers["x-kass-mac"], "abc");
    }

    #[test]
    fn replaces_a_forwarded_address_the_client_made_up() {
        let mut headers = HeaderMap::new();
        headers.insert(X_FORWARDED_FOR, HeaderValue::from_static("127.0.0.1"));
        headers.insert(X_FORWARDED_PROTO, HeaderValue::from_static("https"));
        set_forwarded(&mut headers, "192.168.1.20:50000".parse().unwrap());
        assert_eq!(headers.get_all(X_FORWARDED_FOR).iter().count(), 1);
        assert_eq!(headers[X_FORWARDED_FOR], "192.168.1.20");
        assert_eq!(headers[X_FORWARDED_PROTO], "http");
    }

    #[test]
    fn keeps_the_path_and_query() {
        let uri: Uri = "/captures?limit=5&mac=abc".parse().unwrap();
        assert_eq!(
            worker_uri("127.0.0.1:17494".parse().unwrap(), &uri).unwrap(),
            "http://127.0.0.1:17494/captures?limit=5&mac=abc"
        );
    }
}
