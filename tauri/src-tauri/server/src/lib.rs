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
pub mod model_improvement;
pub mod models;
pub mod proxy;
pub mod routes;
pub mod settings;
pub mod stream;
pub mod styles;
pub mod sync;
pub mod teach;
pub mod usage;
mod voice_sounds;
mod warmth;
mod worker;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use axum::serve::ListenerExt;
use tokio::net::TcpListener;
use tokio::sync::{oneshot, watch};

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
    /// The app's version, for the daily usage report, `/` and `/health`.
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
    /// Teach sessions that are open.
    pub teach: Arc<teach::Sessions>,
    /// Model improvement's runs and schedule.
    pub model_improvement: Arc<model_improvement::Runtime>,
    /// The app's version.
    pub version: Arc<str>,
    /// Stopping from inside, for `POST /shutdown`.
    pub stopping: Stopping,
}

/// The server stopping itself (`POST /shutdown`) rather than being stopped
/// by the app that started it.
#[derive(Clone)]
pub struct Stopping {
    asked: Arc<watch::Sender<bool>>,
    served: watch::Receiver<bool>,
}

impl Stopping {
    /// Stop listening and stop the background tasks; requests already
    /// running finish on their own.
    pub fn stop(&self) {
        self.asked.send_replace(true);
    }

    /// Whether it was asked to stop, or isn't serving any more.
    pub fn stopped(&self) -> bool {
        *self.asked.borrow() || *self.served.borrow()
    }

    /// Wait up to `within` for the requests still running when it stopped
    /// listening to finish; whether they did. A stream that never ends (an
    /// events listener) holds this up to `within`.
    pub async fn finished(&self, within: Duration) -> bool {
        let mut served = self.served.clone();
        tokio::time::timeout(within, served.wait_for(|done| *done))
            .await
            .is_ok_and(|done| done.is_ok())
    }

    async fn asked(&self) {
        let mut asked = self.asked.subscribe();
        let _ = asked.wait_for(|asked| *asked).await;
    }
}

/// A server that's listening. It stops when this is stopped or dropped.
pub struct Running {
    address: SocketAddr,
    _shutdown: oneshot::Sender<()>,
    stopping: Stopping,
    _relays: Vec<Relay>,
}

/// A background task that stops when the server stops: passing on the
/// worker's capture events, learning from reports, model improvement's
/// schedule, keeping models warm, syncing with the shared Kass, the daily
/// usage report, and deleting captures past the history window.
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

    /// Whether it stopped on its own (`POST /shutdown`), so isn't serving
    /// any more though it hasn't been stopped or dropped.
    pub fn stopped(&self) -> bool {
        self.stopping.stopped()
    }
}

/// A server's state over `database` (already prepared) and a worker at
/// `worker`, without serving: for tests of a background job.
#[cfg(test)]
pub(crate) fn test_state(worker: SocketAddr, database: std::path::PathBuf) -> AppState {
    let (asked, _) = watch::channel(false);
    let (_, served) = watch::channel(false);
    AppState {
        version: Arc::from("test"),
        stopping: Stopping {
            asked: Arc::new(asked),
            served,
        },
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
        in_use: Arc::default(),
        teach: Arc::default(),
        model_improvement: Arc::default(),
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
    let (asked, _) = watch::channel(false);
    let (served_tx, served) = watch::channel(false);
    let stopping = Stopping {
        asked: Arc::new(asked),
        served,
    };
    let state = AppState {
        version: config.app_version.as_str().into(),
        stopping: stopping.clone(),
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
        teach: Arc::default(),
        model_improvement: Arc::default(),
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
    let model_improvement = Relay(tokio::spawn(model_improvement::watch(state.clone())));
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
    // A stand-in keeps no history of its own: the shared Kass does.
    let retention = state
        .stand_in
        .is_none()
        .then(|| Relay(tokio::spawn(captures::retention::watch(state.clone()))));
    let mut relays: Vec<Relay> = [relay, learning, model_improvement, warmth, usage, notes]
        .into_iter()
        .chain(sync)
        .chain(retention)
        .collect();
    // Stopped from inside, the background tasks stop with it.
    let background: Vec<_> = relays.iter().map(|relay| relay.0.abort_handle()).collect();
    let on_stop = stopping.clone();
    relays.push(Relay(tokio::spawn(async move {
        on_stop.asked().await;
        background.iter().for_each(tokio::task::AbortHandle::abort);
    })));
    let app = routes::router(state).into_make_service_with_connect_info::<SocketAddr>();
    // Small dictation frames shouldn't wait on Nagle's algorithm.
    let listener = listener.tap_io(|tcp| {
        let _ = tcp.set_nodelay(true);
    });
    let asked = stopping.clone();
    tokio::spawn(async move {
        let served = axum::serve(listener, app)
            // Fires when `Running` is stopped or dropped (its sender goes),
            // or on `POST /shutdown`.
            .with_graceful_shutdown(async move {
                tokio::select! {
                    _ = stopped => {}
                    () = asked.asked() => {}
                }
            })
            .await;
        if let Err(error) = served {
            eprintln!("Kass server on {address} stopped: {error}");
        }
        served_tx.send_replace(true);
    });
    Ok(Running {
        address,
        _shutdown: shutdown,
        stopping,
        _relays: relays,
    })
}
