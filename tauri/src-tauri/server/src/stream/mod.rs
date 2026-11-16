//! The `/captures/stream` dictation session, moving here from the Python
//! worker piece by piece. The route still proxies to the worker until the
//! whole session is in Rust.
//!
//! - `speech`: Silero voice detection over the incoming audio.
//! - `intake`: the start message, PCM frames, the recording, pause cuts and
//!   the phrase windows sent to recognition.
//! - `models`: the worker's Whisper and Qwen3 calls the session makes.

pub mod intake;
pub mod models;
pub mod speech;
