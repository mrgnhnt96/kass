//! Kass's Models page in GPUI: which speech and cleanup models are on this
//! Mac, getting and removing them, and where they're kept.

pub mod catalog;
pub mod downloads;
pub mod host;
mod page;

pub use host::{ModelsHost, NoHost};
pub use page::ModelsPage;
