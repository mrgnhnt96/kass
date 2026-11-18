//! The connection to the Python model worker, which only runs the models
//! (Whisper, Qwen3, training) for this server. Nothing from outside reaches
//! it: every request to the API is answered here, and this server calls the
//! worker's `/worker` endpoints itself (`worker`, `stream::models`).

use std::net::SocketAddr;
use std::time::Duration;

use axum::body::Body;
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::client::legacy::Client;
use hyper_util::rt::TokioExecutor;

/// Uvicorn closes a connection that's idle for 5 seconds; dropping ours well
/// before that means a request never lands on one it's closing.
const IDLE_CONNECTION: Duration = Duration::from_secs(1);

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

    /// The pooled connections to the worker.
    pub(crate) fn client(&self) -> &Client<HttpConnector, Body> {
        &self.client
    }
}
