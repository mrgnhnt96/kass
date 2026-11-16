//! The server's request and response bodies (app/src/lib/api/types.ts,
//! backend/models.py). Enums that come back from the server have an
//! `Unknown` variant, so a newer server's values still parse. Update types
//! send only the fields that are set.

mod captures;
mod dictionary;
mod learning;
mod models;
mod server;
mod settings;
mod stats;
mod teach;
mod writing_style;

pub use captures::*;
pub use dictionary::*;
pub use learning::*;
pub use models::*;
pub use server::*;
pub use settings::*;
pub use stats::*;
pub use teach::*;
pub use writing_style::*;
