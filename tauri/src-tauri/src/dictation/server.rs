//! Where a take's requests go. Usually the one server the app is using; with
//! another Mac's Kass in use (beta `network`), this Mac's own server stands
//! by, and a take that can't reach the other Mac moves to it and stays there
//! (backend/services/sync sends what it made to the other Mac later).

use std::sync::atomic::{AtomicBool, Ordering};

pub struct TakeServer {
    primary: String,
    fallback: Option<String>,
    fell_back: AtomicBool,
}

impl TakeServer {
    pub fn new(primary: String, fallback: Option<String>) -> Self {
        let fallback = fallback.filter(|url| !url.is_empty() && *url != primary);
        Self {
            primary,
            fallback,
            fell_back: AtomicBool::new(false),
        }
    }

    /// A take starting while the other Mac is known to be away begins on the
    /// server standing by, without first waiting on the other.
    pub fn starting(primary: String, fallback: Option<String>, away: bool) -> Self {
        let server = Self::new(primary, fallback);
        if away {
            server.fall_back();
        }
        server
    }

    /// The server the take uses now.
    pub fn url(&self) -> String {
        match &self.fallback {
            Some(fallback) if self.fell_back.load(Ordering::Relaxed) => fallback.clone(),
            _ => self.primary.clone(),
        }
    }

    /// Whether there's a server to fall back to that the take isn't on yet.
    pub fn can_fall_back(&self) -> bool {
        self.fallback.is_some() && !self.fell_back.load(Ordering::Relaxed)
    }

    /// Move the take to the server standing by. Returns its URL the first
    /// time; `None` when there's none or the take is already on it.
    pub fn fall_back(&self) -> Option<String> {
        let fallback = self.fallback.clone()?;
        (!self.fell_back.swap(true, Ordering::Relaxed)).then_some(fallback)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn falls_back_once_and_stays() {
        let server = TakeServer::new(
            "http://studio.local:17493".into(),
            Some("http://127.0.0.1:17493".into()),
        );
        assert_eq!(server.url(), "http://studio.local:17493");
        assert!(server.can_fall_back());
        assert_eq!(
            server.fall_back().as_deref(),
            Some("http://127.0.0.1:17493")
        );
        assert_eq!(server.url(), "http://127.0.0.1:17493");
        assert!(!server.can_fall_back());
        assert_eq!(server.fall_back(), None);
        assert_eq!(server.url(), "http://127.0.0.1:17493");
    }

    #[test]
    fn starts_on_this_mac_while_the_other_is_away() {
        let away = TakeServer::starting(
            "http://studio.local:17493".into(),
            Some("http://127.0.0.1:17493".into()),
            true,
        );
        assert_eq!(away.url(), "http://127.0.0.1:17493");
        assert!(!away.can_fall_back());
        let here = TakeServer::starting("http://127.0.0.1:17493".into(), None, true);
        assert_eq!(here.url(), "http://127.0.0.1:17493");
    }

    #[test]
    fn nothing_to_fall_back_to_on_this_macs_own_server() {
        let local = "http://127.0.0.1:17493".to_string();
        let server = TakeServer::new(local.clone(), Some(local.clone()));
        assert!(!server.can_fall_back());
        assert_eq!(server.fall_back(), None);
        assert_eq!(server.url(), local);
        assert_eq!(TakeServer::new(local.clone(), None).fall_back(), None);
    }
}
