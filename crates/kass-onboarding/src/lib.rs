//! Kass's onboarding window, drawn with GPUI.

pub mod downloads;
pub mod flow;
pub mod host;
pub mod keys;
pub mod motion;
pub mod page;
pub mod poster;
mod steps;

pub use host::{
    Chords, HostEvent, LoginStatus, MicPermission, NoHost, OnHostEvent, OnboardingHost,
};
pub use page::Onboarding;
