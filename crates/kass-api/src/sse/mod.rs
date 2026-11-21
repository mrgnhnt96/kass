//! Server-Sent Event streams: `/captures/events`, `/models/progress/{name}`
//! and `/models/migrate/progress`. Reading blocks; stop a stream from
//! another thread with its `SseCancel`.

mod connect;
mod reader;

use std::io::BufRead;
use std::net::{Shutdown, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

pub(crate) use connect::open;
pub use reader::{SseEvent, SseReader};

use crate::error::{Error, Result};
use crate::mac::Mac;

/// Events from the server until it closes the stream or it's cancelled.
/// An `Err` is a broken connection; the stream ends after it. Unlike
/// `EventSource`, it doesn't reconnect: open a new one.
pub struct SseStream {
    reader: SseReader<Box<dyn BufRead + Send>>,
    cancel: SseCancel,
    ended: bool,
}

impl SseStream {
    pub(crate) fn new(reader: SseReader<Box<dyn BufRead + Send>>, cancel: SseCancel) -> Self {
        Self {
            reader,
            cancel,
            ended: false,
        }
    }

    pub fn cancel_handle(&self) -> SseCancel {
        self.cancel.clone()
    }
}

impl Iterator for SseStream {
    type Item = Result<SseEvent>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.ended || self.cancel.is_cancelled() {
            return None;
        }
        match self.reader.next() {
            Some(Ok(event)) => Some(Ok(event)),
            Some(Err(_)) if self.cancel.is_cancelled() => None,
            Some(Err(error)) => {
                self.ended = true;
                Some(Err(Error::from(error)))
            }
            None => None,
        }
    }
}

/// Stops a stream from any thread: a blocked read returns and the stream
/// ends. Cancel one before it's opened and it opens already ended.
#[derive(Clone, Default)]
pub struct SseCancel {
    inner: Arc<CancelState>,
}

#[derive(Default)]
struct CancelState {
    cancelled: AtomicBool,
    socket: Mutex<Option<TcpStream>>,
}

impl SseCancel {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        self.inner.cancelled.store(true, Ordering::SeqCst);
        if let Some(socket) = self.lock_socket().as_ref() {
            let _ = socket.shutdown(Shutdown::Both);
        }
    }

    pub fn is_cancelled(&self) -> bool {
        self.inner.cancelled.load(Ordering::SeqCst)
    }

    /// Ends the open stream without cancelling: its reader may open a new
    /// one. Over https it can't reach the socket and does nothing.
    pub(crate) fn sever(&self) {
        if let Some(socket) = self.lock_socket().take() {
            let _ = socket.shutdown(Shutdown::Both);
        }
    }

    pub(crate) fn same(&self, other: &SseCancel) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
    }

    /// No stream or reader holds this handle any more, only the client
    /// that tracks it.
    pub(crate) fn is_orphaned(&self) -> bool {
        Arc::strong_count(&self.inner) == 1
    }

    pub(crate) fn set_socket(&self, socket: TcpStream) {
        let mut slot = self.lock_socket();
        if self.is_cancelled() {
            let _ = socket.shutdown(Shutdown::Both);
        }
        *slot = Some(socket);
    }

    fn lock_socket(&self) -> std::sync::MutexGuard<'_, Option<TcpStream>> {
        self.inner
            .socket
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// An event of `/captures/events` (beta `network`). `Changed`: captures
/// changed. `Learned`: anything else the app shows did (corrections, the
/// dictionary, styles, what Kass learned), so refetch everything. `Peers`:
/// the Macs with an app listening, this one included. The server greets
/// each connection with `Changed` and `Learned`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CaptureEvent {
    Changed,
    Learned,
    Peers(Vec<Mac>),
    /// Anything else, including a `peers` list that didn't parse.
    Other(SseEvent),
}

impl From<SseEvent> for CaptureEvent {
    fn from(event: SseEvent) -> Self {
        match event.event.as_str() {
            "changed" => CaptureEvent::Changed,
            "learned" => CaptureEvent::Learned,
            "peers" => match event.json() {
                Ok(macs) => CaptureEvent::Peers(macs),
                Err(_) => CaptureEvent::Other(event),
            },
            _ => CaptureEvent::Other(event),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(name: &str, data: &str) -> SseEvent {
        SseEvent {
            event: name.into(),
            data: data.into(),
        }
    }

    #[test]
    fn reads_capture_events() {
        assert_eq!(
            CaptureEvent::from(event("changed", "{}")),
            CaptureEvent::Changed
        );
        assert_eq!(
            CaptureEvent::from(event("learned", "{}")),
            CaptureEvent::Learned
        );
        assert_eq!(
            CaptureEvent::from(event(
                "peers",
                r#"[{"id":"a1","name":"Studio"},{"id":"b2","name":null}]"#
            )),
            CaptureEvent::Peers(vec![
                Mac::new("a1", Some("Studio".into())),
                Mac::new("b2", None)
            ])
        );
        assert!(matches!(
            CaptureEvent::from(event("peers", "not json")),
            CaptureEvent::Other(_)
        ));
    }
}
