//! Sharing Kass with another Mac (beta `network`), as the Python worker did
//! until it moved here (`backend/services/sync` at b340f4f7).
//!
//! A Mac set to use another Mac's Kass still runs its own server, started
//! with the shared one's address (`Config::shared_server`). That server
//! stands in: the app dictates on it whenever the shared Kass can't be
//! reached.
//!
//! While the shared Kass is up, the stand-in copies what dictation reads
//! from it ([`mirror`]): settings, styles, the dictionary, what was learned
//! and the trained models in use. What the user does on the stand-in is
//! recorded in `sync_changes` (each write path does, given
//! `dictionary::store::Sync`) and sent to the shared Kass once it's back
//! ([`job`]), where it's applied as if it had been done there ([`import`]),
//! the later edit winning. Both sides' rows travel as [`rows`] has them.
//!
//! The stand-in loads no models until the app falls back to it (`wake`),
//! frees them once the shared Kass has been back a while, and never trains
//! or relearns: the shared Kass learns from what it's sent.

pub mod import;
pub mod job;
pub mod mirror;
pub mod rows;

pub use job::StandIn;

/// The shared Kass a server stands in for, from its configured address, or
/// None when this Mac's Kass is the one in use.
pub fn shared_server(configured: Option<&str>) -> Option<String> {
    let url = configured?.trim().trim_end_matches('/');
    (!url.is_empty()).then(|| url.to_string())
}
