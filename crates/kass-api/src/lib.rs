//! A client for the Kass server's HTTP API (`backend/`), for the native
//! screens. It mirrors the React app's `app/src/lib/api/client.ts`.
//!
//! Requests block, so call them from a background executor. The server is
//! this Mac's (`LOCAL_SERVER_URL`) or, with the beta `network` feature,
//! another Mac's; `ApiClient::set_base_url` switches between them.

mod address;
mod client;
mod encode;
mod error;
mod mac;
mod multipart;
mod sse;
pub mod types;

pub use address::{
    LOCAL_SERVER_URL, SERVER_PORT, answers_as_kass, find_kass, kass_at, normalize_server_address,
    renamed_addresses,
};
pub use client::{ApiClient, hugging_face_model_info};
pub use error::{Error, Result};
pub use mac::Mac;
pub use sse::{CaptureEvent, SseCancel, SseEvent, SseReader, SseStream};
