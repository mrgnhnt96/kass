//! Short sounds for dictation: recording started, recording stopped,
//! cancelled with Escape, failed, the writing style changed by voice, and a
//! voice edit heard.
//!
//! Played with AppKit's `NSSound`, which follows the current output device
//! and takes a per-sound volume, so no audio stream of our own stays open
//! between takes. The four WAVs (`sounds/`, from
//! `scripts/generate-sound-cues.py`) are embedded in the binary and decoded
//! once at launch on a player thread that owns every `NSSound`. Callers only
//! push a message onto that thread's channel, so a cue never delays the
//! microphone, the stream to the server or the paste, whichever thread it is
//! triggered from.
//!
//! The enabled flag and volume are capture settings pushed by the main
//! window, and remembered in a file so takes before the server is up (~30 s
//! after launch) already use them.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::OnceLock;
use std::thread;

use crate::host::{AppHandle, Manager};
use objc::runtime::{Object, BOOL};
use objc::{class, msg_send, sel, sel_impl};
use serde::{Deserialize, Serialize};

const START_WAV: &[u8] = include_bytes!("../sounds/start.wav");
const STOP_WAV: &[u8] = include_bytes!("../sounds/stop.wav");
const ERROR_WAV: &[u8] = include_bytes!("../sounds/error.wav");
const STYLE_WAV: &[u8] = include_bytes!("../sounds/style.wav");

pub const DEFAULT_VOLUME: f32 = 0.5;
/// How much of a take's first audio the start cue can reach: the 220 ms
/// sound plus the time to start playing it and the output's own latency.
/// The microphone opens after the cue starts, so this is an upper bound.
pub const START_CUE_SPAN_MS: u32 = 300;
/// The same for the style cue, a 200 ms sound played while the user speaks:
/// its latency, plus the audio captured but not yet sent when it starts.
pub const STYLE_CUE_SPAN_MS: u32 = 350;
/// The style cue waits for the HUD chip's glow (`GLOW_S[0]` in
/// `StyleChip.tsx`), so it rings as the ring lights up and grows, not as the
/// chip fades in.
pub const STYLE_CUE_DELAY_MS: u32 = 920;
/// The edit cue plays as soon as the edit is heard: no chip to wait for.
pub const EDIT_CUE_DELAY_MS: u32 = 0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cue {
    /// The microphone is opening: speak now.
    Start,
    /// The microphone closed with a take to transcribe.
    Stop,
    /// The take failed, or was too short to keep.
    Error,
    /// The user pressed Escape: the stop sound, softer.
    Cancel,
    /// The user asked for a writing style by name ("make this formal").
    Style,
    /// The take opens as a voice edit of the last one ("fix that, ...").
    Edit,
}

impl Cue {
    fn index(self) -> usize {
        match self {
            Cue::Start => 0,
            Cue::Stop | Cue::Cancel => 1,
            Cue::Error => 2,
            // TODO(voice-edits): its own sound; until then the style
            // sound, quieter (EDIT_GAIN).
            Cue::Style | Cue::Edit => 3,
        }
    }

    /// Scales the user's cue volume.
    fn gain(self) -> f32 {
        match self {
            Cue::Cancel => CANCEL_GAIN,
            Cue::Style => STYLE_GAIN,
            Cue::Edit => EDIT_GAIN,
            Cue::Start | Cue::Stop | Cue::Error => 1.0,
        }
    }
}

/// The cancel cue is the stop sound at half the volume: a quiet "never mind".
const CANCEL_GAIN: f32 = 0.5;
/// The style cue plays over the user's own voice: a little under the others.
const STYLE_GAIN: f32 = 0.8;
/// The edit cue borrows the style sound, softer so the two can be told apart.
const EDIT_GAIN: f32 = 0.45;

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub enabled: bool,
    /// 0 to 1, the `NSSound` volume.
    pub volume: f32,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            enabled: true,
            volume: DEFAULT_VOLUME,
        }
    }
}

impl Settings {
    /// These settings with whatever a configure call sent. Settings that
    /// haven't loaded yet send nothing, so the remembered values stand.
    fn with(self, enabled: Option<bool>, volume: Option<f32>) -> Self {
        Self {
            enabled: enabled.unwrap_or(self.enabled),
            volume: volume.map(clamp_volume).unwrap_or(self.volume),
        }
    }

    /// The volume to play at, or `None` when cues are silent.
    fn playback_volume(self) -> Option<f32> {
        (self.enabled && self.volume > 0.0).then_some(self.volume)
    }
}

fn clamp_volume(volume: f32) -> f32 {
    if volume.is_nan() {
        DEFAULT_VOLUME
    } else {
        volume.clamp(0.0, 1.0)
    }
}

static ENABLED: AtomicBool = AtomicBool::new(true);
static VOLUME: AtomicU32 = AtomicU32::new(0x3F00_0000); // DEFAULT_VOLUME's bits
static SETTINGS_FILE: OnceLock<PathBuf> = OnceLock::new();
static PLAYER: OnceLock<Sender<(Cue, f32)>> = OnceLock::new();

fn current() -> Settings {
    Settings {
        enabled: ENABLED.load(Ordering::Relaxed),
        volume: f32::from_bits(VOLUME.load(Ordering::Relaxed)),
    }
}

fn store(settings: Settings) {
    ENABLED.store(settings.enabled, Ordering::Relaxed);
    VOLUME.store(settings.volume.to_bits(), Ordering::Relaxed);
}

/// Restore the saved settings and start the player thread. Called once from
/// Tauri's setup hook.
pub fn init(app: &AppHandle) {
    if let Ok(dir) = app.path().app_config_dir() {
        let path = dir.join("sound-cues.json");
        store(load_settings(&path));
        let _ = SETTINGS_FILE.set(path);
    }
    let (tx, rx) = mpsc::channel();
    let spawned = thread::Builder::new()
        .name("kass-sound-cues".into())
        .spawn(move || run_player(rx));
    match spawned {
        Ok(_) => {
            let _ = PLAYER.set(tx);
        }
        Err(e) => eprintln!("[sound-cues] failed to spawn player thread: {e}"),
    }
}

/// Play `cue` if cues are on. Never blocks on playback. Returns whether the
/// cue went to the player.
pub fn play(cue: Cue) -> bool {
    current()
        .playback_volume()
        .is_some_and(|volume| send(cue, volume * cue.gain()))
}

/// Play `cue` after `delay` if cues are on now and `still_wanted` holds
/// then. Returns whether it was scheduled.
pub fn play_after(
    cue: Cue,
    delay: std::time::Duration,
    still_wanted: impl FnOnce() -> bool + Send + 'static,
) -> bool {
    let Some(volume) = current().playback_volume() else {
        return false;
    };
    let volume = volume * cue.gain();
    thread::Builder::new()
        .name("kass-sound-cue-delay".into())
        .spawn(move || {
            thread::sleep(delay);
            if still_wanted() {
                send(cue, volume);
            }
        })
        .is_ok()
}

fn send(cue: Cue, volume: f32) -> bool {
    PLAYER
        .get()
        .is_some_and(|player| player.send((cue, volume)).is_ok())
}

fn load_settings(path: &Path) -> Settings {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|json| serde_json::from_str::<Settings>(&json).ok())
        .map(|s| Settings {
            volume: clamp_volume(s.volume),
            ..s
        })
        .unwrap_or_default()
}

fn save_settings(path: &Path, settings: Settings) {
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let json = serde_json::to_string(&settings).unwrap_or_default();
    if let Err(e) = std::fs::write(path, json) {
        eprintln!("[sound-cues] could not remember the settings: {e}");
    }
}

type Id = *mut Object;

fn run_player(rx: Receiver<(Cue, f32)>) {
    let sounds = unsafe {
        let _pool = AutoreleasePool::new();
        [
            load_sound(START_WAV),
            load_sound(STOP_WAV),
            load_sound(ERROR_WAV),
            load_sound(STYLE_WAV),
        ]
    };
    // Play each once, silently: AppKit sets up its audio output on the first
    // play (~130 ms), which would otherwise make the first cue late. A play
    // after that takes ~10 ms to return; opening a cpal output stream per cue
    // took ~40 ms to its first callback.
    for sound in sounds.iter().flatten() {
        unsafe {
            let _pool = AutoreleasePool::new();
            play_sound(*sound, 0.0);
        }
    }
    for (cue, volume) in rx {
        if let Some(sound) = sounds[cue.index()] {
            unsafe {
                let _pool = AutoreleasePool::new();
                play_sound(sound, volume);
            }
        }
    }
}

/// An owned `NSSound` decoded from WAV bytes, or `None` if AppKit rejects it.
unsafe fn load_sound(bytes: &[u8]) -> Option<Id> {
    let data: Id = msg_send![class!(NSData), dataWithBytes: bytes.as_ptr() length: bytes.len()];
    let sound: Id = msg_send![class!(NSSound), alloc];
    let sound: Id = msg_send![sound, initWithData: data];
    if sound.is_null() {
        eprintln!("[sound-cues] could not decode a sound");
        return None;
    }
    Some(sound)
}

/// Start `sound` from the top, cutting off a previous play still sounding.
unsafe fn play_sound(sound: Id, volume: f32) -> bool {
    // `stop` takes ~10 ms even on a finished sound, so only when needed.
    let playing: BOOL = msg_send![sound, isPlaying];
    if playing != objc::runtime::NO {
        let _: BOOL = msg_send![sound, stop];
    }
    let _: () = msg_send![sound, setVolume: volume];
    let played: BOOL = msg_send![sound, play];
    played != objc::runtime::NO
}

struct AutoreleasePool {
    pool: Id,
}

impl AutoreleasePool {
    unsafe fn new() -> Self {
        let pool: Id = msg_send![class!(NSAutoreleasePool), alloc];
        let pool: Id = msg_send![pool, init];
        Self { pool }
    }
}

impl Drop for AutoreleasePool {
    fn drop(&mut self) {
        unsafe {
            let _: () = msg_send![self.pool, drain];
        }
    }
}

// ========================================================================
// Commands
// ========================================================================

/// Push the sound cue settings to Rust. `None` leaves a setting as it is.
pub fn configure_sound_cues(enabled: Option<bool>, volume: Option<f32>) {
    let before = current();
    let next = before.with(enabled, volume);
    store(next);
    if next != before {
        if let Some(path) = SETTINGS_FILE.get() {
            save_settings(path, next);
        }
    }
}

/// Play the start cue at `volume`, so the volume slider can be heard.
pub fn preview_sound_cue(volume: f32) {
    send(Cue::Start, clamp_volume(volume));
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn a_configure_call_only_changes_what_it_sends() {
        let saved = Settings {
            enabled: false,
            volume: 0.8,
        };
        assert_eq!(saved.with(None, None), saved);
        assert_eq!(
            saved.with(Some(true), None),
            Settings {
                enabled: true,
                volume: 0.8
            }
        );
        assert_eq!(saved.with(None, Some(0.25)).volume, 0.25);
    }

    #[test]
    fn volume_is_kept_between_silent_and_full() {
        let base = Settings::default();
        assert_eq!(base.with(None, Some(3.0)).volume, 1.0);
        assert_eq!(base.with(None, Some(-1.0)).volume, 0.0);
        assert_eq!(base.with(None, Some(f32::NAN)).volume, DEFAULT_VOLUME);
    }

    #[test]
    fn cues_are_silent_when_off_or_at_zero_volume() {
        let on = Settings::default();
        assert_eq!(on.playback_volume(), Some(DEFAULT_VOLUME));
        assert_eq!(on.with(Some(false), None).playback_volume(), None);
        assert_eq!(on.with(None, Some(0.0)).playback_volume(), None);
    }

    #[test]
    fn the_cancel_cue_is_a_softer_stop() {
        assert_eq!(Cue::Cancel.index(), Cue::Stop.index());
        assert!(Cue::Cancel.gain() < Cue::Stop.gain());
    }

    #[test]
    fn the_style_cue_has_its_own_softer_sound() {
        let others = [Cue::Start, Cue::Stop, Cue::Error];
        assert!(others.iter().all(|cue| cue.index() != Cue::Style.index()));
        assert!(Cue::Style.gain() < Cue::Start.gain());
    }

    #[test]
    fn default_volume_matches_the_initial_atomic() {
        assert_eq!(f32::from_bits(0x3F00_0000), DEFAULT_VOLUME);
    }

    #[test]
    fn settings_are_remembered_across_launches() {
        let dir = temp_dir();
        let path = dir.join("sound-cues.json");
        assert_eq!(load_settings(&path), Settings::default());
        let saved = Settings {
            enabled: false,
            volume: 0.3,
        };
        save_settings(&path, saved);
        assert_eq!(load_settings(&path), saved);
        std::fs::write(&path, r#"{"volume": 7}"#).unwrap();
        assert_eq!(
            load_settings(&path),
            Settings {
                enabled: true,
                volume: 1.0
            }
        );
        std::fs::write(&path, "not json").unwrap();
        assert_eq!(load_settings(&path), Settings::default());
    }

    /// Short and quiet: the start cue plays while the microphone opens, and
    /// must end inside [`START_CUE_SPAN_MS`].
    #[test]
    fn bundled_cues_are_short_quiet_mono_wavs() {
        for bytes in [START_WAV, STOP_WAV, ERROR_WAV] {
            let mut reader = hound::WavReader::new(std::io::Cursor::new(bytes)).unwrap();
            let spec = reader.spec();
            assert_eq!(spec.channels, 1);
            let seconds = reader.duration() as f64 / spec.sample_rate as f64;
            assert!(seconds <= 0.35, "{seconds} s");
            let peak = reader
                .samples::<i16>()
                .map(|s| s.unwrap().unsigned_abs())
                .max()
                .unwrap();
            assert!(peak < i16::MAX as u16 / 2, "peak {peak}");
        }
    }

    #[test]
    fn the_start_cue_span_covers_the_start_cue() {
        let reader = hound::WavReader::new(std::io::Cursor::new(START_WAV)).unwrap();
        let ms = reader.duration() as u64 * 1000 / reader.spec().sample_rate as u64;
        assert!(START_CUE_SPAN_MS as u64 >= ms + 50, "{ms} ms cue");
    }

    /// AppKit decodes and plays the cues off the main thread. Silent; run
    /// with `cargo test -- --ignored`.
    #[test]
    #[ignore]
    fn cues_play_from_a_background_thread() {
        thread::spawn(|| unsafe {
            let _pool = AutoreleasePool::new();
            let sounds: Vec<Id> = [START_WAV, STOP_WAV, ERROR_WAV]
                .into_iter()
                .map(|bytes| load_sound(bytes).expect("decodes"))
                .collect();
            for &sound in &sounds {
                assert!(play_sound(sound, 0.0));
            }
            thread::sleep(Duration::from_millis(400));
            for &sound in &sounds {
                let started = std::time::Instant::now();
                assert!(play_sound(sound, 0.0));
                let call = started.elapsed();
                thread::sleep(Duration::from_millis(50));
                let playing: BOOL = msg_send![sound, isPlaying];
                assert!(playing != objc::runtime::NO);
                eprintln!("primed play() returned in {call:?}");
            }
        })
        .join()
        .unwrap();
    }

    fn temp_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "kass-sound-cues-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }
}
