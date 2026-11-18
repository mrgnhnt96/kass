//! Kass's HTTP and WebSocket API, served by the app on port 17493.
//!
//! Routes move here from the Python model worker one group at a time
//! (`routes/`); any path not served here is proxied to the worker
//! unchanged. Each group's module lists its paths; a ported route swaps
//! `proxy::to_worker()` for a handler of its own. Streamed dictation
//! (`/captures/stream`) runs here and calls the worker only for its models.

pub mod captures;
pub mod cleanup_inputs;
pub mod correction_notes;
mod cors;
pub mod db;
pub mod dictionary;
mod events;
pub mod known_names;
pub mod learned;
pub mod learning;
pub mod models;
pub mod proxy;
pub mod routes;
pub mod settings;
pub mod stream;
pub mod styles;
pub mod sync;
pub mod usage;
mod warmth;
mod worker;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

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
    /// `kass.db`, created or brought up to date (`db::prepare`) on start.
    pub database: PathBuf,
    /// The shared Kass this Mac stands in for (beta network), or None when
    /// this Mac's Kass is the one in use. While standing in, every change
    /// is recorded to send to the shared Kass (`sync_changes`).
    pub shared_server: Option<String>,
    /// Silero's voice detector (`silero_vad.onnx`), for streamed dictation.
    /// Without it every take counts as speech and no pause ends a phrase.
    pub voice_model: Option<PathBuf>,
    /// The folder models download to, as the worker was given it
    /// (`KASS_MODELS_DIR`); None for Hugging Face's own cache.
    pub models_dir: Option<PathBuf>,
    /// The app's version, for the daily usage report.
    pub app_version: String,
}

/// What every route handler can reach.
#[derive(Clone)]
pub struct AppState {
    pub proxy: proxy::Proxy,
    pub db: db::Db,
    pub sync: dictionary::store::Sync,
    events: events::Events,
    /// Each app's dictionary, built once per change.
    pub dictionaries: Arc<dictionary::matching::Dictionaries>,
    pub known_names: Arc<known_names::KnownNames>,
    /// Silero, shared by every dictation.
    pub voice: Option<Arc<stream::speech::VoiceModel>>,
    /// Streamed dictations running, and the results of finished ones.
    pub streams: Arc<stream::Streams>,
    /// The model files, and downloads in flight.
    pub models: models::Models,
    /// Syncing with the shared Kass this server stands in for, if any.
    pub stand_in: Option<Arc<sync::StandIn>>,
    /// Captures being refined or retranscribed, which the history sweep
    /// leaves alone.
    pub in_use: Arc<captures::in_use::InUse>,
}

/// A server that's listening. It stops when this is stopped or dropped.
pub struct Running {
    address: SocketAddr,
    _shutdown: oneshot::Sender<()>,
    _relay: Relay,
    _learning: Relay,
    _warmth: Relay,
    _sync: Option<Relay>,
    _usage: Relay,
    _notes: Relay,
}

/// A background task that stops when the server stops: passing on the
/// worker's capture events, learning from reports, keeping models warm,
/// syncing with the shared Kass, and the daily usage report.
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

/// A server's state over `database` (already prepared) and a worker at
/// `worker`, without serving: for tests of a background job.
#[cfg(test)]
pub(crate) fn test_state(worker: SocketAddr, database: std::path::PathBuf) -> AppState {
    AppState {
        proxy: proxy::Proxy::new(worker),
        db: db::Db::new(database),
        sync: dictionary::store::Sync { standing_in: false },
        events: events::Events::default(),
        dictionaries: Arc::default(),
        known_names: Arc::default(),
        voice: None,
        streams: Arc::default(),
        models: models::Models::new(None),
        stand_in: None,
    }
}

/// Bind the port and serve in the background on the
/// current Tokio runtime.
pub async fn start(config: Config) -> Result<Running, String> {
    let database = config.database.clone();
    tokio::task::spawn_blocking(move || db::prepare(&database))
        .await
        .map_err(|e| e.to_string())??;
    let shared_server = sync::shared_server(config.shared_server.as_deref());
    let state = AppState {
        proxy: proxy::Proxy::new(config.worker),
        db: db::Db::new(config.database),
        sync: dictionary::store::Sync {
            standing_in: shared_server.is_some(),
        },
        events: events::Events::default(),
        dictionaries: Arc::default(),
        known_names: Arc::default(),
        voice: config.voice_model.as_deref().and_then(|path| {
            stream::speech::VoiceModel::load(path)
                .map_err(|error| eprintln!("[server] {error}; every take counts as speech"))
                .ok()
        }),
        streams: Arc::default(),
        models: models::Models::new(config.models_dir.as_deref()),
        stand_in: shared_server
            .as_deref()
            .map(|url| Arc::new(sync::StandIn::new(url))),
        in_use: Arc::default(),
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
    // Habits an older version counted are counted again, once.
    tokio::spawn(learned::recount_if_stale(state.clone()));
    let learning = Relay(tokio::spawn(learning::watch(state.clone())));
    let warmth = Relay(tokio::spawn(warmth::watch(state.clone())));
    let notes = Relay(tokio::spawn(correction_notes::watch(state.clone())));
    let sync = state
        .stand_in
        .clone()
        .map(|stand_in| Relay(tokio::spawn(sync::job::run(stand_in, state.clone()))));
    let usage = Relay(tokio::spawn(usage::report::watch(
        state.clone(),
        config.app_version,
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
        _learning: learning,
        _warmth: warmth,
        _sync: sync,
        _usage: usage,
        _notes: notes,
    })
}
