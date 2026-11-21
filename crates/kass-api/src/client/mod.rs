//! `ApiClient`: one method per call in app/src/lib/api/client.ts, plus the
//! few the app makes outside it. Names drop the `get` prefix.

mod captures;
mod dictionary;
mod learning;
mod models;
pub use models::hugging_face_model_info;
mod server;
mod settings;
mod writing_style;

use std::sync::{Arc, Mutex, PoisonError, RwLock};
use std::time::Duration;

use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::address::LOCAL_SERVER_URL;
use crate::error::{Error, Result, read_body};
use crate::mac::Mac;
use crate::sse::{self, SseCancel, SseStream};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
/// The longest the server may go quiet mid-request. Transcribing a long
/// upload or cleaning up with the largest model can take minutes.
const READ_TIMEOUT: Duration = Duration::from_secs(300);
const WRITE_TIMEOUT: Duration = Duration::from_secs(60);

/// A client for one Kass server at a time. Clones share the server address
/// and the Mac, so switching servers switches every clone.
#[derive(Clone)]
pub struct ApiClient {
    inner: Arc<Inner>,
}

struct Inner {
    agent: ureq::Agent,
    base_url: RwLock<String>,
    mac: RwLock<Option<Mac>>,
    /// Event streams opened through any clone, ended when the server
    /// changes.
    streams: Mutex<Vec<SseCancel>>,
}

impl ApiClient {
    pub fn new(base_url: impl Into<String>, mac: Option<Mac>) -> Self {
        let agent = ureq::AgentBuilder::new()
            .timeout_connect(CONNECT_TIMEOUT)
            .timeout_read(READ_TIMEOUT)
            .timeout_write(WRITE_TIMEOUT)
            .build();
        Self {
            inner: Arc::new(Inner {
                agent,
                base_url: RwLock::new(trim_base(base_url.into())),
                mac: RwLock::new(mac),
                streams: Mutex::new(Vec::new()),
            }),
        }
    }

    /// This Mac's own server.
    pub fn local(mac: Option<Mac>) -> Self {
        Self::new(LOCAL_SERVER_URL, mac)
    }

    pub fn base_url(&self) -> String {
        self.inner
            .base_url
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Send every request from now on to `base_url`: another Mac's server,
    /// or this Mac's while that one is away.
    ///
    /// Open event streams belong to the old server. One to another Mac
    /// that went away may block forever, and one to this Mac's never ends
    /// when the other comes back, so each ends and its reader opens a new
    /// one here.
    pub fn set_base_url(&self, base_url: impl Into<String>) {
        let base_url = trim_base(base_url.into());
        {
            let mut current = self
                .inner
                .base_url
                .write()
                .unwrap_or_else(PoisonError::into_inner);
            if *current == base_url {
                return;
            }
            *current = base_url;
        }
        for stream in self.streams().iter() {
            stream.sever();
        }
    }

    /// The tracked streams, without those nothing holds any more.
    fn streams(&self) -> std::sync::MutexGuard<'_, Vec<SseCancel>> {
        let mut streams = self
            .inner
            .streams
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        streams.retain(|stream| !stream.is_cancelled() && !stream.is_orphaned());
        streams
    }

    pub fn mac(&self) -> Option<Mac> {
        self.inner
            .mac
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// The Mac requests say they come from; None sends no Mac headers.
    pub fn set_mac(&self, mac: Option<Mac>) {
        *self
            .inner
            .mac
            .write()
            .unwrap_or_else(PoisonError::into_inner) = mac;
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base_url())
    }

    fn mac_headers(&self) -> Vec<(&'static str, String)> {
        self.mac().map(|mac| mac.headers()).unwrap_or_default()
    }

    fn request(&self, method: &str, url: &str) -> ureq::Request {
        self.mac_headers().into_iter().fold(
            self.inner.agent.request(method, url),
            |request, (name, value)| request.set(name, &value),
        )
    }

    fn call_url(
        &self,
        method: &str,
        url: &str,
        body: Option<serde_json::Value>,
    ) -> Result<ureq::Response> {
        let request = self
            .request(method, url)
            .set("Content-Type", "application/json");
        let response = match body {
            Some(body) => request.send_json(body),
            None if method == "GET" || method == "DELETE" => request.call(),
            None => request.send_bytes(&[]),
        };
        Ok(response?)
    }

    fn call(
        &self,
        method: &str,
        path: &str,
        body: Option<serde_json::Value>,
    ) -> Result<ureq::Response> {
        self.call_url(method, &self.url(path), body)
    }

    /// A call without a body whose answer is JSON.
    fn fetch<T: DeserializeOwned>(&self, method: &str, path: &str) -> Result<T> {
        decode(self.call(method, path, None)?)
    }

    /// A call with a JSON body whose answer is JSON.
    fn send<T: DeserializeOwned>(
        &self,
        method: &str,
        path: &str,
        body: &impl Serialize,
    ) -> Result<T> {
        decode(self.call(method, path, Some(serde_json::to_value(body)?))?)
    }

    /// A call whose answer doesn't matter (often 204 No Content).
    fn run(&self, method: &str, path: &str) -> Result<()> {
        self.call(method, path, None).map(drop)
    }

    fn events(&self, url: &str, cancel: &SseCancel) -> Result<SseStream> {
        let base_url = self.base_url();
        {
            let mut streams = self.streams();
            if !streams.iter().any(|stream| stream.same(cancel)) {
                streams.push(cancel.clone());
            }
        }
        let stream = sse::open(url, &self.mac_headers(), CONNECT_TIMEOUT, cancel)?;
        // The server changed while this one was opening.
        if self.base_url() != base_url {
            cancel.sever();
        }
        Ok(stream)
    }
}

fn decode<T: DeserializeOwned>(response: ureq::Response) -> Result<T> {
    let body = read_body(response)?;
    serde_json::from_slice(&body).map_err(Error::from)
}

fn trim_base(mut url: String) -> String {
    while url.ends_with('/') {
        url.pop();
    }
    url
}

/// `?style=<id>` for the per-style writing style calls; None is the default style.
fn style_query(style: Option<&str>) -> String {
    match style {
        Some(style) if !style.is_empty() => format!("?style={}", crate::encode::component(style)),
        _ => String::new(),
    }
}
