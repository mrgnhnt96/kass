//! Kass's HTTP and WebSocket API, served by the app on port 17493.
//!
//! The Python model worker still answers every route: this server proxies
//! each request and the `/captures/stream` WebSocket to it unchanged, so
//! routes can move here one group at a time (`routes/`). Each group's module
//! lists its paths; a ported route swaps `proxy::to_worker()` for a handler
//! of its own.

pub mod db;
pub mod proxy;
pub mod routes;

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
}

/// What every route handler can reach.
#[derive(Clone)]
pub struct AppState {
    pub proxy: proxy::Proxy,
    pub db: db::Db,
}

/// A server that's listening. It stops when this is stopped or dropped.
pub struct Running {
    address: SocketAddr,
    _shutdown: oneshot::Sender<()>,
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
    };
    let listener = TcpListener::bind(config.listen)
        .await
        .map_err(|e| format!("Could not listen on {}: {e}", config.listen))?;
    let address = listener.local_addr().map_err(|e| e.to_string())?;
    let (shutdown, stopped) = oneshot::channel::<()>();
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
    })
}
