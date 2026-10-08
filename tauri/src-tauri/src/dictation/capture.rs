//! Native microphone capture with cpal.
//!
//! One thread per take owns the input stream. The audio callback only
//! downmixes into a lock-free SPSC ring (`rtrb`) and never blocks or
//! allocates; the capture thread drains the ring every few milliseconds,
//! cuts ~100 ms frames for the streaming socket, and keeps the complete
//! recording for the batch fallback.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc as std_mpsc;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SampleFormat, SizedSample, StreamConfig};
use serde::Serialize;
use tokio::sync::mpsc::UnboundedSender;
use tokio::sync::oneshot;

use super::audio::{
    self, Aligner, DigitalSilence, Framer, LevelMeter, LowCut, Pause, Resampler, TakeMetrics,
};
use super::device_kind::{self, DeviceKind};
use super::stream::AudioMsg;
use super::take::{Recorded, MIN_RECORDING};

/// How often the capture thread drains the ring.
const DRAIN_INTERVAL: Duration = Duration::from_millis(10);
/// Ring capacity; the thread drains every 10 ms, so this never fills.
const RING_SECONDS: usize = 10;
/// Show "recording" even if the device only ever delivers digital silence.
const HEARD_FALLBACK: Duration = Duration::from_millis(1500);
/// Complete-recording copy kept for the batch fallback.
const MAX_FALLBACK_SECONDS: usize = 600;

#[derive(Debug, Clone, Serialize)]
pub struct NativeInputDevice {
    pub id: String,
    pub name: String,
    pub is_default: bool,
}

pub fn list_input_devices() -> Result<Vec<NativeInputDevice>, String> {
    let host = cpal::default_host();
    let default_name = host.default_input_device().and_then(|d| d.name().ok());
    let devices = host
        .input_devices()
        .map_err(|e| format!("Failed to enumerate input devices: {e}"))?;
    let mut result: Vec<NativeInputDevice> = Vec::new();
    for device in devices {
        let Ok(name) = device.name() else { continue };
        if result.iter().any(|d| d.name == name) {
            continue;
        }
        result.push(NativeInputDevice {
            id: audio::native_device_id(&name),
            is_default: default_name.as_deref() == Some(name.as_str()),
            name,
        });
    }
    Ok(result)
}

/// Callbacks from the capture thread.
pub struct CaptureHooks {
    /// The microphone delivered sound (or the fallback delay passed).
    pub on_heard: Box<dyn FnOnce() + Send>,
    /// Input loudness in dBFS, every [`audio::LEVEL_INTERVAL`].
    pub on_level: Box<dyn FnMut(f32) + Send>,
    /// Recording stopped with a take long enough to transcribe.
    pub on_stopped: Box<dyn FnOnce(Duration) + Send>,
    /// The device could not be opened.
    pub on_error: Box<dyn FnOnce(String) + Send>,
}

pub struct CaptureHandle {
    stop: std_mpsc::Sender<()>,
    pub done: oneshot::Receiver<Option<Recorded>>,
}

impl CaptureHandle {
    pub fn stopper(&self) -> std_mpsc::Sender<()> {
        self.stop.clone()
    }
}

/// Open the device and start capturing right away.
pub fn spawn(
    device_id: Option<String>,
    keydown: Instant,
    audio_tx: UnboundedSender<AudioMsg>,
    hooks: CaptureHooks,
) -> CaptureHandle {
    let (stop_tx, stop_rx) = std_mpsc::channel();
    let (done_tx, done_rx) = oneshot::channel();
    let spawned = thread::Builder::new()
        .name("kass-dictation-capture".into())
        .spawn(move || {
            let recorded = run(device_id, keydown, audio_tx, stop_rx, hooks);
            let _ = done_tx.send(recorded);
        });
    if let Err(e) = spawned {
        eprintln!("[dictation] failed to spawn capture thread: {e}");
    }
    CaptureHandle {
        stop: stop_tx,
        done: done_rx,
    }
}

/// State shared with the realtime callback. Atomics only.
#[derive(Default)]
struct Shared {
    first_sound_nanos: AtomicU64,
    heard: AtomicBool,
    dropped: AtomicU64,
    failed: AtomicBool,
}

/// One open input: its ring and what its callback reports.
struct Input {
    consumer: rtrb::Consumer<i16>,
    shared: Arc<Shared>,
    /// From the device's rate to the take's; `None` when they match.
    resampler: Option<Resampler>,
    /// The last read at the device's rate.
    raw: Vec<i16>,
    /// The last read at the take's rate.
    out: Vec<i16>,
}

impl Input {
    fn new(consumer: rtrb::Consumer<i16>, shared: Arc<Shared>, rate: u32, take_rate: u32) -> Self {
        Self {
            consumer,
            shared,
            resampler: (rate != take_rate).then(|| Resampler::new(rate, take_rate)),
            raw: Vec::with_capacity(rate as usize),
            out: Vec::with_capacity(take_rate as usize),
        }
    }

    /// Move everything in the ring to `raw` and, at the take's rate, `out`.
    fn read(&mut self) {
        self.raw.clear();
        while let Ok(sample) = self.consumer.pop() {
            self.raw.push(sample);
        }
        self.out.clear();
        match &mut self.resampler {
            Some(resampler) => resampler.push(&self.raw, &mut self.out),
            None => self.out.extend_from_slice(&self.raw),
        }
    }
}

/// A Bluetooth headset opening on its own thread while the take records from
/// the built-in microphone. Opening its microphone switches the headset from
/// its listening profile to its headset profile, which can take a second or
/// more, and the capture thread has to keep draining meanwhile. The thread
/// owns the stream (cpal streams can't move between threads).
struct HeadsetThread {
    shared: Arc<Shared>,
    opened: std_mpsc::Receiver<Result<(rtrb::Consumer<i16>, u32), String>>,
    stop: std_mpsc::Sender<()>,
    stopped: std_mpsc::Receiver<()>,
}

/// How long the end of a take waits for the headset thread to stop its
/// stream before the final drain.
const HEADSET_STOP_WAIT: Duration = Duration::from_millis(500);
/// Live audio the headset must deliver before the take moves to it. Right
/// after opening, AirPods send a few samples and then digital silence for
/// 0.5–1.2 s while they switch profiles.
const HEADSET_SETTLE: Duration = Duration::from_millis(150);
/// No samples from the headset for this long means it has stopped (moved to
/// another device, say).
const HEADSET_STALL: Duration = Duration::from_millis(150);
/// The headset's audio arrives a tenth of a second or more behind the
/// built-in microphone's, so moving to it mid-word repeated part of the word
/// and Whisper garbled the phrase. The take moves once [`Aligner`] has lined
/// the two up on the voice, leaving out the headset audio the built-in
/// microphone already gave; or, failing that, once the built-in microphone
/// has heard this much quiet, so only quiet repeats.
const HANDOFF_PAUSE: Duration = Duration::from_millis(300);

fn open_headset(device: cpal::Device, keydown: Instant) -> Option<HeadsetThread> {
    let shared = Arc::new(Shared::default());
    let (opened_tx, opened) = std_mpsc::channel();
    let (stop, stop_rx) = std_mpsc::channel::<()>();
    let (stopped_tx, stopped) = std_mpsc::channel();
    let thread_shared = shared.clone();
    let spawned = thread::Builder::new()
        .name("kass-dictation-headset".into())
        .spawn(move || {
            let (stream, consumer, rate) = match open_device(&device, keydown, thread_shared) {
                Ok(opened) => opened,
                Err(message) => {
                    let _ = opened_tx.send(Err(message));
                    return;
                }
            };
            if let Err(e) = stream.play() {
                let _ = stream.pause();
                let _ = opened_tx.send(Err(e.to_string()));
                return;
            }
            let _ = opened_tx.send(Ok((consumer, rate)));
            // Until the take ends (or already has: the stop is queued).
            let _ = stop_rx.recv();
            let _ = stream.pause();
            drop(stream);
            let _ = stopped_tx.send(());
        });
    if let Err(e) = spawned {
        eprintln!("[dictation] failed to spawn headset thread: {e}");
        return None;
    }
    Some(HeadsetThread {
        shared,
        opened,
        stop,
        stopped,
    })
}

/// Whether the headset is delivering a live microphone: no digital silence
/// and no stall for [`HEADSET_SETTLE`].
struct HeadsetHealth {
    silence: DigitalSilence,
    live_since: Option<Instant>,
    last_data: Instant,
}

impl HeadsetHealth {
    fn new(sample_rate: u32, now: Instant) -> Self {
        Self {
            silence: DigitalSilence::new(sample_rate),
            live_since: None,
            last_data: now,
        }
    }

    /// Account for the samples just read (at the headset's own rate).
    fn update(&mut self, raw: &[i16], now: Instant) {
        if raw.is_empty() {
            if now.duration_since(self.last_data) >= HEADSET_STALL {
                self.live_since = None;
            }
            return;
        }
        self.last_data = now;
        if self.silence.push(raw) {
            self.live_since = None;
        } else if self.live_since.is_none() {
            self.live_since = Some(now);
        }
    }

    /// Delivering a microphone's audio, settled or not.
    fn receiving(&self) -> bool {
        self.live_since.is_some()
    }

    fn live(&self, now: Instant) -> bool {
        self.live_since
            .is_some_and(|since| now.duration_since(since) >= HEADSET_SETTLE)
    }
}

fn run(
    device_id: Option<String>,
    keydown: Instant,
    audio_tx: UnboundedSender<AudioMsg>,
    stop_rx: std_mpsc::Receiver<()>,
    hooks: CaptureHooks,
) -> Option<Recorded> {
    let CaptureHooks {
        on_heard,
        mut on_level,
        on_stopped,
        on_error,
    } = hooks;
    let fail = |message: String| {
        eprintln!("[dictation] microphone unavailable: {message}");
        let _ = audio_tx.send(AudioMsg::Cancel);
        on_error(format!("Microphone unavailable: {message}"));
        None
    };
    let host = cpal::default_host();
    let device = match select_device(&host, device_id.as_deref()) {
        Ok(device) => device,
        Err(message) => return fail(message),
    };
    // For a Bluetooth headset, the built-in microphone runs for the whole
    // take, and each chunk comes from the headset only while it is live.
    let (local_device, headset_device) = match built_in_bridge(&host, &device) {
        Some(bridge) => (bridge, Some(device)),
        None => (device, None),
    };
    let local_shared = Arc::new(Shared::default());
    let opened = open_device(&local_device, keydown, local_shared.clone()).and_then(
        |(stream, consumer, rate)| match stream.play() {
            Ok(()) => Ok((stream, consumer, rate)),
            Err(e) => {
                let _ = stream.pause();
                Err(e.to_string())
            }
        },
    );
    // Only now: while the headset switches profiles, CoreAudio held up the
    // built-in microphone's open by up to 0.9 s.
    let mut headset_thread = headset_device.and_then(|device| open_headset(device, keydown));
    let mut local_stream: Option<cpal::Stream> = None;
    let mut local: Option<Input> = None;
    let mut headset: Option<Input> = None;
    let mut health: Option<HeadsetHealth> = None;
    let sample_rate = match opened {
        Ok((stream, consumer, rate)) => {
            local_stream = Some(stream);
            local = Some(Input::new(consumer, local_shared, rate, rate));
            rate
        }
        // The built-in bridge failed: wait for the headset, as without one.
        Err(message) => {
            match headset_thread.as_ref().map(|t| t.opened.recv()) {
                Some(Ok(Ok((consumer, rate)))) => {
                    eprintln!("[dictation] built-in microphone unavailable ({message}); using the headset");
                    let shared = headset_thread.as_ref().expect("headset").shared.clone();
                    headset = Some(Input::new(consumer, shared, rate, rate));
                    rate
                }
                Some(Ok(Err(headset_message))) => return fail(headset_message),
                _ => return fail(message),
            }
        }
    };
    let shareds: Vec<Arc<Shared>> = [
        local.as_ref().map(|l| l.shared.clone()),
        headset_thread.as_ref().map(|t| t.shared.clone()),
    ]
    .into_iter()
    .flatten()
    .collect();
    let first_shared = shareds[0].clone();
    let _ = audio_tx.send(AudioMsg::Format(sample_rate));
    let mut metrics = TakeMetrics {
        stream_open: Some(keydown.elapsed()),
        sample_rate,
        ..Default::default()
    };
    let playing_since = Instant::now();
    let mut framer = Framer::new(sample_rate);
    let mut meter = LevelMeter::new(sample_rate);
    let mut low_cut = LowCut::new(sample_rate);
    let max_fallback = sample_rate as usize * MAX_FALLBACK_SECONDS;
    let mut recording: Vec<i16> = Vec::with_capacity(sample_rate as usize * 30);
    let mut scratch: Vec<i16> = Vec::with_capacity(sample_rate as usize);
    let mut on_heard = Some(on_heard);
    let mut leading_zeros: Option<u64> = None;
    let mut on_headset = local.is_none();
    let mut local_pause = Pause::new(sample_rate);
    let mut headset_ready_at: Option<Duration> = None;
    let mut aligner = Aligner::new(sample_rate);
    // Headset samples still to leave out: the built-in microphone gave them.
    let mut headset_skip = 0usize;

    let mut process = |chunk: &[i16],
                       framer: &mut Framer,
                       recording: &mut Vec<i16>,
                       leading_zeros: &mut Option<u64>| {
        if chunk.is_empty() {
            return;
        }
        if leading_zeros.is_none() {
            if let Some(index) = chunk.iter().position(|&s| s != 0) {
                *leading_zeros = Some(recording.len() as u64 + index as u64);
            }
        }
        scratch.clear();
        scratch.extend_from_slice(chunk);
        // Everything downstream (server, fallback upload, HUD level) gets the
        // audio without hum below the voice.
        low_cut.process(&mut scratch);
        let room = max_fallback.saturating_sub(recording.len());
        recording.extend_from_slice(&scratch[..scratch.len().min(room)]);
        for frame in framer.push(&scratch) {
            let _ = audio_tx.send(AudioMsg::Frame(frame));
        }
        // After the frames: the HUD's level events must never delay audio
        // on its way to the server.
        meter.push(&scratch, &mut on_level);
    };

    loop {
        match stop_rx.recv_timeout(DRAIN_INTERVAL) {
            Ok(()) | Err(std_mpsc::RecvTimeoutError::Disconnected) => break,
            Err(std_mpsc::RecvTimeoutError::Timeout) => {}
        }
        let now = Instant::now();
        if headset.is_none() {
            if let Some(t) = &headset_thread {
                match t.opened.try_recv() {
                    Ok(Ok((consumer, rate))) => {
                        headset = Some(Input::new(consumer, t.shared.clone(), rate, sample_rate));
                        health = Some(HeadsetHealth::new(rate, now));
                    }
                    Err(std_mpsc::TryRecvError::Empty) => {}
                    Ok(Err(message)) => {
                        eprintln!("[dictation] headset unavailable ({message}); staying on the built-in microphone");
                        headset_thread = None;
                    }
                    Err(std_mpsc::TryRecvError::Disconnected) => headset_thread = None,
                }
            }
        }
        if let Some(l) = &mut local {
            l.read();
            local_pause.push(&l.out);
            aligner.push_local(&l.out);
        }
        if let Some(h) = &mut headset {
            h.read();
            if let Some(health) = &mut health {
                health.update(&h.raw, now);
                if health.receiving() {
                    aligner.push_headset(&h.out);
                } else {
                    aligner.reset_headset();
                }
            }
        }
        let headset_ready =
            headset.is_some() && (local.is_none() || health.as_ref().is_some_and(|h| h.live(now)));
        if headset_ready && !on_headset && headset_ready_at.is_none() {
            headset_ready_at = Some(keydown.elapsed());
            eprintln!(
                "[dictation] headset microphone live at {:.0}ms; moving to it once lined up or at a pause",
                keydown.elapsed().as_secs_f64() * 1000.0
            );
        }
        let lined_up = (headset_ready && !on_headset && local.is_some())
            .then(|| aligner.lag())
            .flatten();
        // Back to the built-in microphone at once: the headset has gone.
        let headset_live = headset_ready
            && (on_headset
                || local.is_none()
                || lined_up.is_some()
                || local_pause.quiet_for() >= HANDOFF_PAUSE);
        // Moving on a lineup: the built-in microphone gives this chunk, which
        // the headset's next `lag` samples repeat.
        let mut handoff_chunk: Option<&Input> = None;
        if headset_live != on_headset {
            on_headset = headset_live;
            if on_headset {
                metrics.handoff.get_or_insert(keydown.elapsed());
                let how = match lined_up {
                    Some((lag, correlation)) => {
                        headset_skip = lag;
                        handoff_chunk = local.as_ref();
                        let trail = Duration::from_secs_f64(lag as f64 / sample_rate as f64);
                        metrics.headset_lag.get_or_insert(trail);
                        format!(
                            "lined up: it trails by {:.0}ms, match {correlation:.2}",
                            trail.as_secs_f64() * 1000.0
                        )
                    }
                    None => "at a pause".to_string(),
                };
                eprintln!(
                    "[dictation] moved to the headset microphone at {:.0}ms ({how})",
                    keydown.elapsed().as_secs_f64() * 1000.0
                );
            } else {
                metrics.headset_dropouts += 1;
                headset_ready_at = None;
                headset_skip = 0;
                eprintln!(
                    "[dictation] headset went silent at {:.0}ms; back to the built-in microphone",
                    keydown.elapsed().as_secs_f64() * 1000.0
                );
            }
        }
        let source = if on_headset { &headset } else { &local };
        if let Some(input) = handoff_chunk {
            process(&input.out, &mut framer, &mut recording, &mut leading_zeros);
        } else if let Some(input) = source {
            let mut chunk = &input.out[..];
            if on_headset {
                let skipped = headset_skip.min(chunk.len());
                headset_skip -= skipped;
                chunk = &chunk[skipped..];
            }
            process(chunk, &mut framer, &mut recording, &mut leading_zeros);
        }
        if on_heard.is_some()
            && (source
                .as_ref()
                .is_some_and(|i| i.shared.heard.load(Ordering::Relaxed))
                || playing_since.elapsed() >= HEARD_FALLBACK)
        {
            if let Some(hook) = on_heard.take() {
                hook();
            }
        }
        if headset
            .as_ref()
            .is_some_and(|h| h.shared.failed.load(Ordering::Relaxed))
        {
            eprintln!("[dictation] headset stream failed");
            headset = None;
            health = None;
            if let Some(t) = headset_thread.take() {
                let _ = t.stop.send(());
            }
        }
        if local
            .as_ref()
            .is_some_and(|l| l.shared.failed.load(Ordering::Relaxed))
        {
            eprintln!("[dictation] built-in input stream failed");
            local = None;
            if let Some(stream) = local_stream.take() {
                let _ = stream.pause();
            }
        }
        if local.is_none() && headset.is_none() && headset_thread.is_none() {
            eprintln!("[dictation] input stream failed; ending the take");
            break;
        }
    }

    metrics.wall = keydown.elapsed();
    // Stop the device explicitly: for a chosen (non-default) microphone,
    // cpal 0.15's disconnect listener keeps the stream alive, so dropping it
    // alone left the microphone running after every take. Pausing stops the
    // audio unit, and every callback has returned by then, so the final drain
    // sees all audio.
    if let Some(stream) = local_stream.take() {
        let _ = stream.pause();
    }
    if let Some(t) = headset_thread.take() {
        let _ = t.stop.send(());
        // Only an open headset stream holds audio still to drain; one still
        // opening stops itself when the open returns.
        if headset.is_some() {
            let _ = t.stopped.recv_timeout(HEADSET_STOP_WAIT);
        }
    }
    let source = if on_headset { &mut headset } else { &mut local };
    if let Some(input) = source {
        input.read();
        process(&input.out, &mut framer, &mut recording, &mut leading_zeros);
    }
    if let Some(tail) = framer.flush() {
        let _ = audio_tx.send(AudioMsg::Frame(tail));
    }

    metrics.samples = recording.len() as u64;
    metrics.leading_zero_samples = leading_zeros.unwrap_or(metrics.samples);
    metrics.dropped_samples = shareds
        .iter()
        .map(|s| s.dropped.load(Ordering::Relaxed))
        .sum();
    if first_shared.heard.load(Ordering::Relaxed) {
        metrics.first_sound = Some(Duration::from_nanos(
            first_shared.first_sound_nanos.load(Ordering::Relaxed),
        ));
    }
    eprintln!("[dictation] take captured: {}", metrics.summary());

    let recorded = Recorded {
        pcm: recording,
        sample_rate,
    };
    let duration = recorded.duration();
    if duration < MIN_RECORDING {
        let _ = audio_tx.send(AudioMsg::Cancel);
    } else {
        on_stopped(duration);
        let _ = audio_tx.send(AudioMsg::End);
    }
    Some(recorded)
}

type Opened = (cpal::Stream, rtrb::Consumer<i16>, u32);

/// The built-in microphone to record from while `device`, a Bluetooth
/// headset, opens. `None` for any other device, or when there is no
/// built-in microphone.
fn built_in_bridge(host: &cpal::Host, device: &cpal::Device) -> Option<cpal::Device> {
    let name = device.name().ok()?;
    let kinds = device_kind::device_kinds();
    if device_kind::kind_of(&kinds, &name) != DeviceKind::Bluetooth {
        return None;
    }
    host.input_devices().ok()?.find(|candidate| {
        candidate.name().is_ok_and(|candidate_name| {
            candidate_name != name
                && device_kind::kind_of(&kinds, &candidate_name) == DeviceKind::BuiltIn
        })
    })
}

fn open_device(
    device: &cpal::Device,
    keydown: Instant,
    shared: Arc<Shared>,
) -> Result<Opened, String> {
    let default_config = device
        .default_input_config()
        .map_err(|e| format!("no input configuration ({e})"))?;
    let ranges: Vec<(u32, u32)> = device
        .supported_input_configs()
        .map(|configs| {
            configs
                .filter(|c| c.sample_format() == default_config.sample_format())
                .map(|c| (c.min_sample_rate().0, c.max_sample_rate().0))
                .collect()
        })
        .unwrap_or_default();
    let sample_rate = audio::choose_sample_rate(default_config.sample_rate().0, &ranges)
        .ok_or_else(|| {
            format!(
                "unsupported sample rate {} Hz",
                default_config.sample_rate().0
            )
        })?;
    let config = StreamConfig {
        channels: default_config.channels(),
        sample_rate: cpal::SampleRate(sample_rate),
        buffer_size: cpal::BufferSize::Default,
    };
    let (producer, consumer) = rtrb::RingBuffer::<i16>::new(sample_rate as usize * RING_SECONDS);
    let stream = match default_config.sample_format() {
        SampleFormat::F32 => build::<f32>(device, &config, producer, keydown, shared),
        SampleFormat::I16 => build::<i16>(device, &config, producer, keydown, shared),
        SampleFormat::I32 => build::<i32>(device, &config, producer, keydown, shared),
        SampleFormat::U16 => build::<u16>(device, &config, producer, keydown, shared),
        other => return Err(format!("unsupported sample format {other:?}")),
    }?;
    Ok((stream, consumer, sample_rate))
}

fn select_device(host: &cpal::Host, device_id: Option<&str>) -> Result<cpal::Device, String> {
    let saved_native = device_id.is_some_and(|id| id.starts_with(audio::NATIVE_DEVICE_PREFIX));
    if saved_native {
        if let Ok(devices) = host.input_devices() {
            let devices: Vec<cpal::Device> = devices.collect();
            let names: Vec<String> = devices
                .iter()
                .map(|d| d.name().unwrap_or_default())
                .collect();
            if let Some(index) = audio::pick_device(&names, device_id) {
                return Ok(devices.into_iter().nth(index).expect("index from names"));
            }
        }
        eprintln!(
            "[dictation] saved microphone {:?} not found; using the system default",
            device_id
        );
    }
    host.default_input_device()
        .ok_or_else(|| "no input device".to_string())
}

fn build<T>(
    device: &cpal::Device,
    config: &StreamConfig,
    mut producer: rtrb::Producer<i16>,
    keydown: Instant,
    shared: Arc<Shared>,
) -> Result<cpal::Stream, String>
where
    T: SizedSample,
    f32: FromSample<T>,
{
    let channels = config.channels.max(1) as usize;
    let error_shared = shared.clone();
    device
        .build_input_stream(
            config,
            move |data: &[T], _| {
                // Realtime thread: no locks, no allocation, no I/O.
                let mut heard = shared.heard.load(Ordering::Relaxed);
                audio::downmix(data, channels, |value| {
                    if !heard && value != 0 {
                        heard = true;
                        shared
                            .first_sound_nanos
                            .store(keydown.elapsed().as_nanos() as u64, Ordering::Relaxed);
                        shared.heard.store(true, Ordering::Relaxed);
                    }
                    if producer.push(value).is_err() {
                        shared.dropped.fetch_add(1, Ordering::Relaxed);
                    }
                });
            },
            move |err| {
                eprintln!("[dictation] input stream error: {err}");
                error_shared.failed.store(true, Ordering::Relaxed);
            },
            None,
        )
        .map_err(|e| format!("could not open the microphone ({e})"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 10 ms of 24 kHz microphone noise.
    fn noise() -> Vec<i16> {
        (0..240).map(|i| [3, -2, 5, -4][i % 4]).collect()
    }

    #[test]
    fn headset_is_live_only_after_settling() {
        let start = Instant::now();
        let at = |ms: u64| start + Duration::from_millis(ms);
        let mut health = HeadsetHealth::new(24_000, start);
        // AirPods right after opening: a blip, then digital silence.
        health.update(&noise(), at(10));
        assert!(!health.live(at(10)));
        health.update(&[0; 240], at(20));
        for ms in (30..600).step_by(10) {
            health.update(&[0; 240], at(ms));
            assert!(!health.live(at(ms)));
        }
        // The microphone profile is up.
        for ms in (600..740).step_by(10) {
            health.update(&noise(), at(ms));
            assert!(!health.live(at(ms)), "{ms}");
        }
        health.update(&noise(), at(750));
        assert!(health.live(at(750)));
    }

    #[test]
    fn headset_drops_out_on_silence_or_a_stall() {
        let start = Instant::now();
        let at = |ms: u64| start + Duration::from_millis(ms);
        let mut health = HeadsetHealth::new(24_000, start);
        for ms in (0..200).step_by(10) {
            health.update(&noise(), at(ms));
        }
        assert!(health.live(at(200)));
        // Moved to the phone: zeros.
        health.update(&[0; 240], at(210));
        assert!(!health.live(at(210)));

        let mut health = HeadsetHealth::new(24_000, start);
        for ms in (0..200).step_by(10) {
            health.update(&noise(), at(ms));
        }
        // Bursty delivery is fine; nothing at all for 150 ms is not.
        health.update(&[], at(300));
        assert!(health.live(at(300)));
        health.update(&[], at(350));
        assert!(!health.live(at(350)));
    }
}
