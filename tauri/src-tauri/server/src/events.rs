//! Tells every connected app when what it shows changes, over
//! `GET /captures/events`, as `backend/services/capture_events.py` did.
//!
//! A Mac sharing its Kass (beta `network`) has apps on other Macs reading its
//! captures, corrections and learning, and they don't hear the Tauri events
//! the dictating Mac sends. So the server announces each change itself:
//! `changed` when captures change, so the apps refetch their captures and
//! Insights, and `learned` when anything else does (corrections, the
//! dictionary, styles, what Kass learned), so they refetch everything. The
//! stream also says which Macs have an app listening (`peers`).
//!
//! Routes answered here publish their own changes. The worker still saves
//! dictation, refinement and learning, and announces those on its own
//! stream, which [`relay`] passes on.

use std::collections::{BTreeSet, HashMap};
use std::convert::Infallible;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::{Body, Bytes};
use axum::http::{header, HeaderMap, HeaderValue, Method, Request};
use axum::response::{IntoResponse, Response};
use http_body_util::BodyExt;
use tokio::sync::Notify;

use crate::proxy::Proxy;

const KEEPALIVE: Duration = Duration::from_secs(15);

/// How long the relay waits before trying the worker's stream again.
const RECONNECT: Duration = Duration::from_secs(1);

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Kind {
    Changed,
    Learned,
    Peers,
}

impl Kind {
    fn name(self) -> &'static str {
        match self {
            Kind::Changed => "changed",
            Kind::Learned => "learned",
            Kind::Peers => "peers",
        }
    }
}

/// The Mac an app runs on, as it says in `X-Kass-Mac` and `X-Kass-Mac-Name`
/// or the `mac` and `mac_name` query parameters (`backend/utils/macs.py`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mac {
    pub id: String,
    pub name: Option<String>,
}

struct Listener {
    mac: Option<Mac>,
    pending: BTreeSet<Kind>,
    wake: Arc<Notify>,
}

#[derive(Default)]
struct Hub {
    next: u64,
    listeners: HashMap<u64, Listener>,
}

/// Every open stream, and what each has yet to send.
#[derive(Clone, Default)]
pub struct Events(Arc<Mutex<Hub>>);

impl Events {
    fn hub(&self) -> std::sync::MutexGuard<'_, Hub> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Tell every listener. Pending events collapse: one `changed` says it all.
    pub fn publish(&self, kind: Kind) {
        for listener in self.hub().listeners.values_mut() {
            listener.pending.insert(kind);
            listener.wake.notify_one();
        }
    }

    /// The Macs with an app listening, each once, by id.
    pub fn connected_macs(&self) -> Vec<Mac> {
        let hub = self.hub();
        let mut macs: Vec<(&u64, &Mac)> = hub
            .listeners
            .iter()
            .filter_map(|(key, listener)| Some((key, listener.mac.as_ref()?)))
            .collect();
        // The newest listener's name wins, as the worker's dict kept the last.
        macs.sort_by(|a, b| (&a.1.id, a.0).cmp(&(&b.1.id, b.0)));
        let mut out: Vec<Mac> = Vec::new();
        for (_, mac) in macs {
            let mac = mac.clone();
            match out.last_mut() {
                Some(last) if last.id == mac.id => *last = mac,
                _ => out.push(mac),
            }
        }
        out
    }

    fn subscribe(&self, mac: Option<Mac>) -> Subscription {
        let wake = Arc::new(Notify::new());
        let id = {
            let mut hub = self.hub();
            let id = hub.next;
            hub.next += 1;
            let listener = Listener {
                mac,
                pending: BTreeSet::new(),
                wake: Arc::clone(&wake),
            };
            hub.listeners.insert(id, listener);
            id
        };
        self.publish(Kind::Peers);
        Subscription {
            events: self.clone(),
            id,
            wake,
        }
    }

    fn event(&self, kind: Kind) -> String {
        let data = match kind {
            Kind::Peers => peers_json(&self.connected_macs()),
            _ => "{}".to_string(),
        };
        format!("event: {}\ndata: {data}\n\n", kind.name())
    }

    /// The `text/event-stream` response for one app: `changed` and `learned`
    /// at once, so an app that reconnects refetches what it missed, then
    /// each change as it comes and a comment every 15 seconds to keep the
    /// line open. The app stops listening when the connection closes.
    pub fn stream(&self, mac: Option<Mac>) -> Response {
        let subscription = self.subscribe(mac);
        let opening = format!("{}{}", self.event(Kind::Changed), self.event(Kind::Learned));
        let first = futures_util::stream::once(async move { Ok(Bytes::from(opening)) });
        let rest = futures_util::stream::unfold(subscription, |subscription| async move {
            let woke = tokio::time::timeout(KEEPALIVE, subscription.wake.notified()).await;
            let chunk = match woke {
                Err(_) => ": keepalive\n\n".to_string(),
                Ok(()) => subscription.take(),
            };
            Some((Ok::<_, Infallible>(Bytes::from(chunk)), subscription))
        });
        let body = Body::from_stream(futures_util::StreamExt::chain(first, rest));
        let mut response = body.into_response();
        let headers = response.headers_mut();
        headers.insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("text/event-stream; charset=utf-8"),
        );
        headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
        headers.insert(header::CONNECTION, HeaderValue::from_static("keep-alive"));
        headers.insert("x-accel-buffering", HeaderValue::from_static("no"));
        response
    }
}

/// One open stream. It stops listening when dropped, with its connection.
struct Subscription {
    events: Events,
    id: u64,
    wake: Arc<Notify>,
}

impl Subscription {
    /// The events pending, oldest kind first, as one chunk.
    fn take(&self) -> String {
        let pending = match self.events.hub().listeners.get_mut(&self.id) {
            Some(listener) => std::mem::take(&mut listener.pending),
            None => BTreeSet::new(),
        };
        pending
            .into_iter()
            .map(|kind| self.events.event(kind))
            .collect()
    }
}

impl Drop for Subscription {
    fn drop(&mut self) {
        self.events.hub().listeners.remove(&self.id);
        self.events.publish(Kind::Peers);
    }
}

// -- which Mac ----------------------------------------------------------------

/// A header as Starlette reads one: each byte a Latin-1 character. The last
/// one sent wins, as in the worker's dict of headers.
fn latin1_header(headers: &HeaderMap, name: &str) -> Option<String> {
    let value = headers.get_all(name).iter().next_back()?;
    Some(value.as_bytes().iter().map(|&b| char::from(b)).collect())
}

/// Python's `unquote`: runs of `%XX` decoded as UTF-8, anything else kept.
fn unquote(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::new();
    let mut bytes = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let hex = match (chars[i], chars.get(i + 1), chars.get(i + 2)) {
            ('%', Some(high), Some(low)) => high
                .to_digit(16)
                .zip(low.to_digit(16))
                .map(|(high, low)| (high * 16 + low) as u8),
            _ => None,
        };
        match hex {
            Some(byte) => {
                bytes.push(byte);
                i += 3;
            }
            None => {
                if !bytes.is_empty() {
                    out.push_str(&String::from_utf8_lossy(&std::mem::take(&mut bytes)));
                }
                out.push(chars[i]);
                i += 1;
            }
        }
    }
    out.push_str(&String::from_utf8_lossy(&bytes));
    out
}

/// A query string's pairs, as Python's `parse_qsl` reads one: `+` is a
/// space, and a pair without `=` has a blank value. Blank values are dropped
/// unless `keep_blank`, as `parse_qs` drops them by default.
pub(crate) fn query_pairs(query: &str, keep_blank: bool) -> Vec<(String, String)> {
    let query: String = query.bytes().map(char::from).collect();
    query
        .split('&')
        .filter(|pair| !pair.is_empty())
        .filter_map(|pair| {
            let (key, value) = match pair.split_once('=') {
                Some((key, value)) => (key, value),
                None if keep_blank => (pair, ""),
                None => return None,
            };
            let value = unquote(&value.replace('+', " "));
            (keep_blank || !value.is_empty()).then(|| (unquote(&key.replace('+', " ")), value))
        })
        .collect()
}

/// The first non-blank value of `name` in a query string, as `parse_qs`
/// reads one.
fn query_value(query: &str, name: &str) -> Option<String> {
    query_pairs(query, false)
        .into_iter()
        .find_map(|(key, value)| (key == name).then_some(value))
}

fn valid_id(id: &str) -> bool {
    (1..=64).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
}

/// The Mac a request says it came from, if any.
pub fn mac_of(headers: &HeaderMap, query: Option<&str>) -> Option<Mac> {
    let query = query.unwrap_or("");
    let id = latin1_header(headers, "x-kass-mac")
        .filter(|id| !id.is_empty())
        .or_else(|| query_value(query, "mac"))?;
    if !valid_id(&id) {
        return None;
    }
    let name = match latin1_header(headers, "x-kass-mac-name") {
        Some(raw) => unquote(&raw),
        None => query_value(query, "mac_name").unwrap_or_default(),
    };
    let name: String = name.trim().chars().take(100).collect();
    Some(Mac {
        id,
        name: (!name.is_empty()).then_some(name),
    })
}

/// `json.dumps` of the Macs listening, as the worker wrote it: ASCII only,
/// with Python's separators.
fn peers_json(macs: &[Mac]) -> String {
    let items: Vec<String> = macs
        .iter()
        .map(|mac| {
            let name = mac.name.as_deref().map_or("null".to_string(), ascii_json);
            format!("{{\"id\": {}, \"name\": {name}}}", ascii_json(&mac.id))
        })
        .collect();
    format!("[{}]", items.join(", "))
}

fn ascii_json(text: &str) -> String {
    let mut out = String::from("\"");
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            ' '..='~' => out.push(c),
            _ => {
                let mut units = [0u16; 2];
                for unit in c.encode_utf16(&mut units) {
                    out.push_str(&format!("\\u{unit:04x}"));
                }
            }
        }
    }
    out.push('"');
    out
}

// -- the worker's changes -----------------------------------------------------

/// Pass on the worker's `changed` and `learned` events, for as long as the
/// server runs: dictation, refinement and learning still save there. Its
/// stream is opened again whenever it closes, as when the worker restarts.
pub async fn relay(events: Events, proxy: Proxy) {
    loop {
        let _ = follow(&events, &proxy).await;
        tokio::time::sleep(RECONNECT).await;
    }
}

async fn follow(events: &Events, proxy: &Proxy) -> Result<(), String> {
    let request = Request::builder()
        .method(Method::GET)
        .uri(format!("http://{}/worker/captures/events", proxy.worker()))
        .body(Body::empty())
        .map_err(|e| e.to_string())?;
    let response = proxy
        .client()
        .request(request)
        .await
        .map_err(|e| e.to_string())?;
    if !response.status().is_success() {
        return Err(response.status().to_string());
    }
    let mut body = response.into_body();
    let mut buffer = String::new();
    while let Some(frame) = body.frame().await {
        let frame = frame.map_err(|e| e.to_string())?;
        let Ok(data) = frame.into_data() else {
            continue;
        };
        buffer.push_str(&String::from_utf8_lossy(&data));
        while let Some(end) = buffer.find("\n\n") {
            let event: String = buffer.drain(..end + 2).collect();
            for line in event.lines() {
                match line.strip_prefix("event: ") {
                    Some("changed") => events.publish(Kind::Changed),
                    Some("learned") => events.publish(Kind::Learned),
                    _ => {}
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(pairs: &[(&'static str, &[u8])]) -> HeaderMap {
        let mut headers = HeaderMap::new();
        for (name, value) in pairs {
            headers.append(*name, HeaderValue::from_bytes(value).unwrap());
        }
        headers
    }

    #[test]
    fn a_mac_comes_from_headers_or_the_query() {
        let named = headers(&[
            ("x-kass-mac", b"mac-1"),
            ("x-kass-mac-name", b"Morgan%E2%80%99s%20Mac"),
        ]);
        assert_eq!(
            mac_of(&named, None),
            Some(Mac {
                id: "mac-1".into(),
                name: Some("Morgan\u{2019}s Mac".into())
            })
        );
        let query = "mac=mac-2&mac_name=++Studio+%F0%9F%8E%A7++&mac_name=other";
        assert_eq!(
            mac_of(&HeaderMap::new(), Some(query)),
            Some(Mac {
                id: "mac-2".into(),
                name: Some("Studio \u{1f3a7}".into())
            })
        );
        assert_eq!(
            mac_of(&HeaderMap::new(), Some("mac=&mac=b")).unwrap().id,
            "b"
        );
        assert_eq!(
            mac_of(&headers(&[("x-kass-mac", b"no spaces")]), None),
            None
        );
        assert_eq!(mac_of(&HeaderMap::new(), Some("mac_name=x")), None);
        let blank = headers(&[("x-kass-mac", b"m"), ("x-kass-mac-name", b"  ")]);
        assert_eq!(mac_of(&blank, Some("mac_name=x")).unwrap().name, None);
        let long = format!("mac=m&mac_name={}", "a".repeat(150));
        assert_eq!(
            mac_of(&HeaderMap::new(), Some(&long))
                .unwrap()
                .name
                .unwrap()
                .len(),
            100
        );
    }

    #[test]
    fn unquote_keeps_what_isnt_an_escape() {
        assert_eq!(unquote("100%"), "100%");
        assert_eq!(unquote("%zz%41"), "%zzA");
        assert_eq!(unquote("%C3"), "\u{fffd}");
    }

    #[test]
    fn peers_are_written_as_python_wrote_them() {
        let macs = [
            Mac {
                id: "a".into(),
                name: Some("Morgan\u{2019}s \"Mac\" \u{1f3a7}".into()),
            },
            Mac {
                id: "b".into(),
                name: None,
            },
        ];
        assert_eq!(
            peers_json(&macs),
            r#"[{"id": "a", "name": "Morgan\u2019s \"Mac\" \ud83c\udfa7"}, {"id": "b", "name": null}]"#
        );
        assert_eq!(peers_json(&[]), "[]");
    }

    #[test]
    fn listeners_collapse_events_and_say_who_listens() {
        let events = Events::default();
        let first = events.subscribe(Some(Mac {
            id: "b".into(),
            name: Some("Old".into()),
        }));
        let second = events.subscribe(Some(Mac {
            id: "b".into(),
            name: Some("New".into()),
        }));
        let third = events.subscribe(Some(Mac {
            id: "a".into(),
            name: None,
        }));
        let ids: Vec<_> = events
            .connected_macs()
            .into_iter()
            .map(|m| (m.id, m.name))
            .collect();
        assert_eq!(
            ids,
            vec![("a".into(), None), ("b".into(), Some("New".into()))]
        );
        events.publish(Kind::Learned);
        events.publish(Kind::Changed);
        events.publish(Kind::Changed);
        let chunk = first.take();
        assert!(chunk.starts_with("event: changed\ndata: {}\n\nevent: learned\ndata: {}\n\nevent: peers\ndata: [{\"id\": \"a\""));
        assert_eq!(first.take(), "");
        drop(third);
        assert!(second
            .take()
            .ends_with("event: peers\ndata: [{\"id\": \"b\", \"name\": \"New\"}]\n\n"));
    }
}
