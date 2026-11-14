//! Kass's dictation text stages, ported from the Python backend.
//!
//! Every function here is pure text in, text out, and matches its Python
//! original call for call: `tests/golden.rs` replays calls recorded from the
//! Python functions and compares the results.

pub mod py;
pub mod difflib;
pub mod laughter;
pub mod lexicon;
pub mod phrase_seams;
pub mod spoken_case;
pub mod spoken_cleanup;
pub mod spoken_corrections;
pub mod spoken_punctuation;
pub mod voice_commands;
