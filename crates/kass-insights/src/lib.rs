//! Kass's Insights page in GPUI: how much, how fast and where the user
//! dictates, for all apps or one, over a period against the one before.
//! Everything is counted on the server (`GET /captures/stats`).
//!
//! Run it on its own against a local server:
//! `cargo run -p kass-insights --example insights`.

mod app_list;
pub mod format;
mod heat_map;
mod host;
mod length_curve;
pub mod link;
mod page;
mod stat_cards;
mod where_you_dictate;
mod widgets;
mod words_chart;

pub use host::{InsightsHost, NoHost};
pub use link::Link;
pub use page::InsightsPage;
