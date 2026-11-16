//! Kass's HTTP and WebSocket API, served by the app on port 17493.
//!
//! The Python model worker still answers every route: this server proxies
//! each request and the `/captures/stream` WebSocket to it unchanged, so
//! routes can move here one group at a time (`routes/`). Each group's module
//! lists its paths; a ported route swaps `proxy::to_worker()` for a handler
//! of its own.

pub mod captures;
mod cors;
pub mod db;
pub mod dictionary;
mod events;
pub mod proxy;
pub mod routes;
pub mod settings;
pub mod stream;
mod worker;

use std::net::SocketAddr;
use std::path::PathBuf;

use axum::serve::ListenerExt;
use tokio::net::TcpListener;
use tokio::sync::oneshot;

/// Where the server listens, the worker it proxies to, and the database.
#[derive(Clone, Debug)]
pub struct Config {
    /// `127.0.0.1:17493`, or `0.0.0.0:17493` while Kass is shared with other
    /// Macs on the network.
    pub listen: SocketAddr,
    /// The Python model worker, on a private loopback port.
    pub worker: SocketAddr,
    /// `kass.db`, which the worker creates and migrates.
    pub database: PathBuf,
    /// The shared Kass this Mac stands in for (beta network), or None when
    /// this Mac's Kass is the one in use. While standing in, every change
    /// is recorded to send to the shared Kass (`sync_changes`).
    pub shared_server: Option<String>,
}

/// What every route handler can reach.
#[derive(Clone)]
pub struct AppState {
    pub proxy: proxy::Proxy,
    pub db: db::Db,
    pub sync: dictionary::store::Sync,
    events: events::Events,
}

/// A server that's listening. It stops when this is stopped or dropped.
pub struct Running {
    address: SocketAddr,
    _shutdown: oneshot::Sender<()>,
    _relay: Relay,
}

/// Stops passing on the worker's capture events when the server stops.
struct Relay(tokio::task::JoinHandle<()>);

impl Drop for Relay {
    fn drop(&mut self) {
        self.0.abort();
    }
}

impl Running {
    /// The address it listens on, with the real port when 0 was asked for.
    pub fn address(&self) -> SocketAddr {
        self.address
    }

    /// Stop accepting connections and close the listening socket, so the
    /// port is free again. Requests already running finish on their own.
    pub fn stop(self) {}
}

/// Bind the port and serve in the background on the
/// current Tokio runtime.
pub async fn start(config: Config) -> Result<Running, String> {
    let state = AppState {
        proxy: proxy::Proxy::new(config.worker),
        db: db::Db::new(config.database),
        sync: dictionary::store::Sync {
            standing_in: config
                .shared_server
                .is_some_and(|url| !url.trim().trim_end_matches('/').is_empty()),
        },
        events: events::Events::default(),
    };
    let listener = TcpListener::bind(config.listen)
        .await
        .map_err(|e| format!("Could not listen on {}: {e}", config.listen))?;
    let address = listener.local_addr().map_err(|e| e.to_string())?;
    let (shutdown, stopped) = oneshot::channel::<()>();
    let relay = Relay(tokio::spawn(events::relay(
        state.events.clone(),
        state.proxy.clone(),
    )));
    let app = routes::router(state).into_make_service_with_connect_info::<SocketAddr>();
    // Small dictation frames shouldn't wait on Nagle's algorithm.
    let listener = listener.tap_io(|tcp| {
        let _ = tcp.set_nodelay(true);
    });
    tokio::spawn(async move {
        let served = axum::serve(listener, app)
            // Fires when `Running` is stopped or dropped: its sender goes.
            .with_graceful_shutdown(async move {
                let _ = stopped.await;
            })
            .await;
        if let Err(error) = served {
            eprintln!("Kass server on {address} stopped: {error}");
        }
    });
    Ok(Running {
        address,
        _shutdown: shutdown,
        _relay: relay,
    })
}
