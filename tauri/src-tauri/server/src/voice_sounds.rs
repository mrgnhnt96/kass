//! Background sounds for voice training: other people talking, and rooms.
//!
//! A one-time download, started from Settings, of two openly licensed
//! corpora:
//!
//! - LibriSpeech test-clean (CC BY 4.0, Panayotov et al. 2015), 40 speakers
//!   reading aloud: the people talking behind the user.
//! - DEMAND (CC BY-SA 3.0, Thiemann et al. 2013) restaurant, office and
//!   living room recordings: the rooms.
//!
//! About 0.6 GB is downloaded; Kass keeps about 350 MB, decoded to 16 kHz
//! 16-bit samples, in `voice-training/sounds` next to `kass.db`: `talk.npz`
//! (one array per utterance, and `speakers`), `noise.npz` (one per room) and
//! `sounds.json`. The worker's voice training reads them with numpy
//! (`backend/services/voice_training/sounds.py`). Nothing is sent anywhere.

use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{self, BufReader, BufWriter, Cursor, Read, Seek, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use futures_util::StreamExt;
use serde_json::{json, Value};
use zip::write::SimpleFileOptions;

use crate::AppState;

const VERSION: u64 = 1;
const LIBRISPEECH: [&str; 2] = [
    "https://www.openslr.org/resources/12/test-clean.tar.gz",
    "https://us.openslr.org/resources/12/test-clean.tar.gz",
];
const ROOMS: [&str; 3] = ["PRESTO", "OOFFICE", "DLIVING"];
const UTTERANCES_PER_SPEAKER: usize = 30;
/// test-clean plus three DEMAND rooms, for progress.
const DOWNLOAD_BYTES: u64 = 630_000_000;
const CREDITS: [&str; 2] = [
    "LibriSpeech test-clean, CC BY 4.0 (Panayotov, Chen, Povey, Khudanpur 2015)",
    "DEMAND, CC BY-SA 3.0 (Thiemann, Ito, Vincent 2013)",
];
const ATTEMPTS: u32 = 5;
const RATE: u32 = 16_000;

/// The wait before the next attempt is this times 2^attempt, up to 30.
#[cfg(not(test))]
const BACKOFF: Duration = Duration::from_secs(1);
#[cfg(test)]
const BACKOFF: Duration = Duration::ZERO;

/// Where the files come from.
struct Sources {
    talk: Vec<String>,
    /// Each room's name and its zip of 16 kHz recordings.
    rooms: Vec<(String, String)>,
}

impl Sources {
    fn kass() -> Self {
        Sources {
            talk: LIBRISPEECH.iter().map(|url| url.to_string()).collect(),
            rooms: ROOMS
                .iter()
                .map(|name| {
                    let url = format!(
                        "https://zenodo.org/records/1227121/files/{name}_16k.zip?download=1"
                    );
                    (name.to_string(), url)
                })
                .collect(),
        }
    }
}

/// The download in this process. Not per server: sharing restarts the
/// server, and the old one's download goes on.
static DOWNLOAD: Mutex<Progress> = Mutex::new(Progress {
    running: false,
    fraction: 0.0,
    error: None,
});

struct Progress {
    running: bool,
    fraction: f64,
    error: Option<String>,
}

fn progress() -> std::sync::MutexGuard<'static, Progress> {
    DOWNLOAD.lock().unwrap_or_else(|e| e.into_inner())
}

fn root(state: &AppState) -> PathBuf {
    state
        .db
        .path()
        .parent()
        .map(|dir| dir.join("voice-training"))
        .unwrap_or_else(|| PathBuf::from("voice-training"))
}

fn ready(root: &Path) -> bool {
    let dir = root.join("sounds");
    let version = fs::read_to_string(dir.join("sounds.json"))
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        .and_then(|info| info["version"].as_u64());
    version == Some(VERSION) && dir.join("talk.npz").is_file() && dir.join("noise.npz").is_file()
}

fn status_of(root: &Path) -> Value {
    let (running, fraction, error) = {
        let progress = progress();
        (progress.running, progress.fraction, progress.error.clone())
    };
    if running {
        let fraction = (fraction * 1000.0).round() / 1000.0;
        return json!({ "state": "downloading", "fraction": fraction, "error": null });
    }
    if ready(root) {
        return json!({ "state": "ready", "fraction": 1.0, "error": null });
    }
    let state = if error.is_some() { "failed" } else { "missing" };
    json!({ "state": state, "fraction": 0.0, "error": error })
}

/// Downloading, ready, failed (with why) or missing, and how far along.
pub async fn status(state: &AppState) -> Value {
    let root = root(state);
    tokio::task::spawn_blocking(move || status_of(&root))
        .await
        .unwrap_or(Value::Null)
}

/// Begin the download in the background, unless it's done or running; the
/// status after.
pub async fn start(state: &AppState) -> Value {
    let root = root(state);
    let begin = {
        let root = root.clone();
        tokio::task::spawn_blocking(move || {
            let mut progress = progress();
            let begin = !progress.running && !ready(&root);
            if begin {
                *progress = Progress {
                    running: true,
                    fraction: 0.0,
                    error: None,
                };
            }
            begin
        })
        .await
        .unwrap_or(false)
    };
    if begin {
        let root = root.clone();
        tokio::spawn(async move {
            let advance = |done: u64| {
                progress().fraction = (done as f64 / DOWNLOAD_BYTES as f64).min(0.99);
            };
            let result = download(&root, &Sources::kass(), &advance).await;
            let mut progress = progress();
            progress.running = false;
            match result {
                Ok(()) => {
                    progress.fraction = 1.0;
                    progress.error = None;
                }
                Err(error) => {
                    eprintln!("[voice] downloading the voice-training sounds failed: {error}");
                    progress.error = Some(format!(
                        "The download failed: {error}. Check the connection and try again."
                    ));
                }
            }
        });
    }
    let root = root.clone();
    tokio::task::spawn_blocking(move || status_of(&root))
        .await
        .unwrap_or(Value::Null)
}

fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .user_agent("Kass")
        .connect_timeout(Duration::from_secs(15))
        .read_timeout(Duration::from_secs(60))
        .build()
        .unwrap_or_default()
}

/// Download, decode and keep the sounds in `root/sounds`; `advance` hears
/// the bytes downloaded so far. Builds in `root/sounds.partial` and swaps
/// it in when done.
async fn download(
    root: &Path,
    sources: &Sources,
    advance: &(dyn Fn(u64) + Sync),
) -> Result<(), String> {
    let staging = root.join("sounds.partial");
    let result = download_into(&staging, sources, advance).await;
    match result {
        Ok(()) => {
            let target = root.join("sounds");
            let _ = fs::remove_dir_all(&target);
            fs::rename(&staging, &target).map_err(|e| e.to_string())
        }
        Err(error) => {
            let _ = fs::remove_dir_all(&staging);
            Err(error)
        }
    }
}

async fn download_into(
    staging: &Path,
    sources: &Sources,
    advance: &(dyn Fn(u64) + Sync),
) -> Result<(), String> {
    let _ = fs::remove_dir_all(staging);
    fs::create_dir_all(staging).map_err(|e| e.to_string())?;
    let client = client();
    let done = std::sync::atomic::AtomicU64::new(0);
    let count = |bytes: u64| {
        let total = done.fetch_add(bytes, std::sync::atomic::Ordering::Relaxed) + bytes;
        advance(total);
    };

    let speech = staging.join("test-clean.tar.gz");
    fetch(&client, &sources.talk, &speech, &count).await?;
    let talk = (speech.clone(), staging.join("talk.npz"));
    let (talkers, utterances) = tokio::task::spawn_blocking(move || {
        let counts = talk_npz(&talk.0, &talk.1)?;
        fs::remove_file(&talk.0).map_err(|e| e.to_string())?;
        Ok::<_, String>(counts)
    })
    .await
    .map_err(|e| e.to_string())??;

    let mut beds = Vec::new();
    for (name, url) in &sources.rooms {
        let target = staging.join(format!("{name}.zip"));
        fetch(&client, std::slice::from_ref(url), &target, &count).await?;
        let bed = tokio::task::spawn_blocking(move || {
            let bed = room(&target)?;
            fs::remove_file(&target).map_err(|e| e.to_string())?;
            Ok::<_, String>(bed)
        })
        .await
        .map_err(|e| e.to_string())??;
        beds.push((name.clone(), bed));
    }
    let rooms: Vec<&String> = beds.iter().map(|(name, _)| name).collect();
    let info = json!({
        "version": VERSION,
        "talkers": talkers,
        "utterances": utterances,
        "rooms": rooms,
        "credits": CREDITS,
    });
    let staging = staging.to_path_buf();
    tokio::task::spawn_blocking(move || {
        let mut npz = Npz::create(&staging.join("noise.npz"))?;
        for (name, bed) in &beds {
            npz.add(name, &Array::I16(bed))?;
        }
        npz.finish()?;
        let text = serde_json::to_string_pretty(&info).map_err(|e| e.to_string())?;
        fs::write(staging.join("sounds.json"), text).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Download `urls` (mirrors, tried in turn) to `target`, resuming after a
/// dropped connection: each attempt asks for the bytes it doesn't have yet.
/// Fails when every attempt fails or the file ends short.
async fn fetch(
    client: &reqwest::Client,
    urls: &[String],
    target: &Path,
    advance: &(dyn Fn(u64) + Sync),
) -> Result<(), String> {
    let mut error = String::from("nothing to download");
    for attempt in 0..ATTEMPTS {
        let url = &urls[attempt as usize % urls.len()];
        match fetch_once(client, url, target, advance).await {
            Ok(true) => return Ok(()),
            Ok(false) => error = "the download ended early".into(),
            Err(failure) => error = failure,
        }
        eprintln!(
            "[voice] voice-training download interrupted (attempt {}): {error}",
            attempt + 1
        );
        tokio::time::sleep(BACKOFF * 2u32.pow(attempt).min(30)).await;
    }
    Err(error)
}

/// One attempt; whether `target` is whole after it.
async fn fetch_once(
    client: &reqwest::Client,
    url: &str,
    target: &Path,
    advance: &(dyn Fn(u64) + Sync),
) -> Result<bool, String> {
    let mut have = fs::metadata(target).map(|m| m.len()).unwrap_or(0);
    let mut request = client.get(url);
    if have > 0 {
        request = request.header(reqwest::header::RANGE, format!("bytes={have}-"));
    }
    let response = request.send().await.map_err(|e| e.to_string())?;
    let response = response.error_for_status().map_err(|e| e.to_string())?;
    if have > 0 && response.status() != reqwest::StatusCode::PARTIAL_CONTENT {
        have = 0; // the server ignored the range: start over
    }
    let total = have + response.content_length().unwrap_or(0);
    let file = fs::OpenOptions::new()
        .create(true)
        .write(true)
        .append(have > 0)
        .truncate(have == 0)
        .open(target)
        .map_err(|e| e.to_string())?;
    let mut out = BufWriter::new(file);
    let mut body = response.bytes_stream();
    let written = async {
        while let Some(chunk) = body.next().await {
            let chunk = chunk.map_err(|e| e.to_string())?;
            out.write_all(&chunk).map_err(|e| e.to_string())?;
            advance(chunk.len() as u64);
        }
        Ok::<(), String>(())
    }
    .await;
    // What arrived before a drop is kept for the next attempt.
    out.flush().map_err(|e| e.to_string())?;
    written?;
    let size = fs::metadata(target).map(|m| m.len()).unwrap_or(0);
    Ok(total == 0 || size >= total)
}

/// Write the first `UTTERANCES_PER_SPEAKER` utterances of each LibriSpeech
/// speaker in `archive` to `npz`, one array each (`arr_0`…, as numpy's
/// `savez(*talk)` names them), with `speakers` beside them. (talkers,
/// utterances).
fn talk_npz(archive: &Path, npz: &Path) -> Result<(usize, usize), String> {
    let file = File::open(archive).map_err(|e| e.to_string())?;
    let mut tar = tar::Archive::new(flate2::read::GzDecoder::new(BufReader::new(file)));
    let mut out = Npz::create(npz)?;
    let mut per_speaker: HashMap<String, usize> = HashMap::new();
    let mut speakers: Vec<i64> = Vec::new();
    for entry in tar.entries().map_err(|e| e.to_string())? {
        let mut entry = entry.map_err(|e| e.to_string())?;
        let name = entry
            .path()
            .map_err(|e| e.to_string())?
            .to_string_lossy()
            .into_owned();
        if !name.ends_with(".flac") {
            continue;
        }
        // LibriSpeech/test-clean/<speaker>/<chapter>/<utterance>.flac
        let parts: Vec<&str> = name.split('/').collect();
        let Some(speaker) = parts.len().checked_sub(3).map(|i| parts[i].to_string()) else {
            continue;
        };
        let taken = per_speaker.entry(speaker.clone()).or_default();
        if *taken >= UTTERANCES_PER_SPEAKER {
            continue;
        }
        *taken += 1;
        let mut bytes = Vec::new();
        entry.read_to_end(&mut bytes).map_err(|e| e.to_string())?;
        let audio = decode_flac(&bytes)?;
        out.add(&format!("arr_{}", speakers.len()), &Array::I16(&audio))?;
        speakers.push(
            speaker
                .parse()
                .map_err(|_| format!("Unexpected speaker folder {speaker:?}"))?,
        );
    }
    out.add("speakers", &Array::I64(&speakers))?;
    out.finish()?;
    Ok((per_speaker.len(), speakers.len()))
}

/// The first microphone's recording in a DEMAND room's zip.
fn room(zip_path: &Path) -> Result<Vec<i16>, String> {
    let file = File::open(zip_path).map_err(|e| e.to_string())?;
    let mut archive = zip::ZipArchive::new(BufReader::new(file)).map_err(|e| e.to_string())?;
    let name = archive
        .file_names()
        .find(|name| name.ends_with("ch01.wav"))
        .map(str::to_string)
        .ok_or("The room recording has no ch01.wav")?;
    let mut bytes = Vec::new();
    archive
        .by_name(&name)
        .map_err(|e| e.to_string())?
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    decode_wav(&bytes)
}

fn expect_rate(rate: u32) -> Result<(), String> {
    if rate == RATE {
        Ok(())
    } else {
        Err(format!("Expected 16 kHz audio, got {rate}"))
    }
}

/// An integer sample of `bits` bits as 16 bits.
fn to_i16(sample: i32, bits: u32) -> i16 {
    if bits >= 16 {
        (sample >> (bits - 16)) as i16
    } else {
        (sample << (16 - bits)) as i16
    }
}

/// Interleaved frames to one channel: their mean, truncated toward zero as
/// numpy's `mean(axis=1).astype(int16)` does.
fn mono(samples: Vec<i16>, channels: usize) -> Vec<i16> {
    if channels <= 1 {
        return samples;
    }
    samples
        .chunks(channels)
        .map(|frame| {
            let sum: f64 = frame.iter().map(|&s| s as f64).sum();
            (sum / channels as f64) as i16
        })
        .collect()
}

fn decode_flac(bytes: &[u8]) -> Result<Vec<i16>, String> {
    let mut reader = claxon::FlacReader::new(Cursor::new(bytes)).map_err(|e| e.to_string())?;
    let info = reader.streaminfo();
    expect_rate(info.sample_rate)?;
    let samples = reader
        .samples()
        .map(|s| s.map(|s| to_i16(s, info.bits_per_sample)))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    Ok(mono(samples, info.channels as usize))
}

fn decode_wav(bytes: &[u8]) -> Result<Vec<i16>, String> {
    let mut reader = hound::WavReader::new(Cursor::new(bytes)).map_err(|e| e.to_string())?;
    let spec = reader.spec();
    expect_rate(spec.sample_rate)?;
    let samples = match spec.sample_format {
        hound::SampleFormat::Int => reader
            .samples::<i32>()
            .map(|s| s.map(|s| to_i16(s, spec.bits_per_sample as u32)))
            .collect::<Result<Vec<_>, _>>(),
        hound::SampleFormat::Float => reader
            .samples::<f32>()
            .map(|s| s.map(|s| (s * 32767.0).round().clamp(-32768.0, 32767.0) as i16))
            .collect::<Result<Vec<_>, _>>(),
    }
    .map_err(|e| e.to_string())?;
    Ok(mono(samples, spec.channels as usize))
}

enum Array<'a> {
    I16(&'a [i16]),
    I64(&'a [i64]),
}

/// numpy's `.npz`: a zip of `.npy` files, stored, as `np.savez` writes it.
struct Npz<W: Write + Seek>(zip::ZipWriter<W>);

impl Npz<BufWriter<File>> {
    fn create(path: &Path) -> Result<Self, String> {
        let file = File::create(path).map_err(|e| e.to_string())?;
        Ok(Npz(zip::ZipWriter::new(BufWriter::new(file))))
    }
}

impl<W: Write + Seek> Npz<W> {
    fn add(&mut self, name: &str, array: &Array) -> Result<(), String> {
        let options = SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored)
            .large_file(true);
        self.0
            .start_file(format!("{name}.npy"), options)
            .map_err(|e| e.to_string())?;
        write_npy(&mut self.0, array).map_err(|e| e.to_string())
    }

    fn finish(self) -> Result<W, String> {
        self.0.finish().map_err(|e| e.to_string())
    }
}

/// A one-dimensional array in numpy's `.npy` format, version 1.0.
fn write_npy(out: &mut impl Write, array: &Array) -> io::Result<()> {
    let (descr, len, data) = match array {
        Array::I16(values) => (
            "<i2",
            values.len(),
            values
                .iter()
                .flat_map(|v| v.to_le_bytes())
                .collect::<Vec<u8>>(),
        ),
        Array::I64(values) => (
            "<i8",
            values.len(),
            values
                .iter()
                .flat_map(|v| v.to_le_bytes())
                .collect::<Vec<u8>>(),
        ),
    };
    let mut header = format!("{{'descr': '{descr}', 'fortran_order': False, 'shape': ({len},), }}");
    // Magic, version and length take 10 bytes; the data starts 64-aligned.
    let pad = (64 - (10 + header.len() + 1) % 64) % 64;
    header.push_str(&" ".repeat(pad));
    header.push('\n');
    out.write_all(b"\x93NUMPY\x01\x00")?;
    out.write_all(&(header.len() as u16).to_le_bytes())?;
    out.write_all(header.as_bytes())?;
    out.write_all(&data)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    use axum::http::{HeaderMap, StatusCode};
    use axum::response::IntoResponse;

    fn temp() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("kass-sounds-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn npy_headers_are_numpy_aligned() {
        let mut out = Vec::new();
        write_npy(&mut out, &Array::I16(&[1, -2, 3])).unwrap();
        assert_eq!(&out[..8], b"\x93NUMPY\x01\x00");
        let header_len = u16::from_le_bytes([out[8], out[9]]) as usize;
        assert_eq!((10 + header_len) % 64, 0);
        let header = std::str::from_utf8(&out[10..10 + header_len]).unwrap();
        assert!(header.starts_with("{'descr': '<i2', 'fortran_order': False, 'shape': (3,), }"));
        assert!(header.ends_with('\n'));
        assert_eq!(&out[10 + header_len..], &[1, 0, 0xfe, 0xff, 3, 0]);
    }

    #[test]
    fn stereo_becomes_the_mean_truncated_toward_zero() {
        assert_eq!(mono(vec![1, 2, -1, -2, 10, 20], 2), vec![1, -1, 15]);
        assert_eq!(to_i16(0x12_3456, 24), 0x1234);
        assert_eq!(to_i16(-1, 8), -256);
    }

    #[test]
    fn a_room_is_its_first_microphone_at_16_khz() {
        let dir = temp();
        let mut wav = Cursor::new(Vec::new());
        {
            let spec = hound::WavSpec {
                channels: 1,
                sample_rate: RATE,
                bits_per_sample: 16,
                sample_format: hound::SampleFormat::Int,
            };
            let mut writer = hound::WavWriter::new(&mut wav, spec).unwrap();
            for sample in [5i16, -7, 9] {
                writer.write_sample(sample).unwrap();
            }
            writer.finalize().unwrap();
        }
        let path = dir.join("PRESTO.zip");
        let mut zip = zip::ZipWriter::new(File::create(&path).unwrap());
        let options = SimpleFileOptions::default();
        zip.start_file("PRESTO/ch02.wav", options).unwrap();
        zip.write_all(b"not this one").unwrap();
        zip.start_file("PRESTO/ch01.wav", options).unwrap();
        zip.write_all(wav.get_ref()).unwrap();
        zip.finish().unwrap();
        assert_eq!(room(&path).unwrap(), vec![5, -7, 9]);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn only_a_whole_current_download_is_ready() {
        let root = temp();
        let dir = root.join("sounds");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("talk.npz"), b"").unwrap();
        fs::write(dir.join("noise.npz"), b"").unwrap();
        assert!(!ready(&root));
        fs::write(dir.join("sounds.json"), r#"{"version": 0}"#).unwrap();
        assert!(!ready(&root));
        fs::write(dir.join("sounds.json"), r#"{"version": 1}"#).unwrap();
        assert!(ready(&root));
        fs::remove_file(dir.join("noise.npz")).unwrap();
        assert!(!ready(&root));
        fs::remove_dir_all(root).unwrap();
    }

    /// Serves `payload`, dropping the first connection halfway; the ranges
    /// asked for, in order.
    async fn flaky(payload: Arc<Vec<u8>>) -> (String, Arc<Mutex<Vec<usize>>>) {
        let starts = Arc::new(Mutex::new(Vec::new()));
        let calls = Arc::new(AtomicUsize::new(0));
        let seen = starts.clone();
        let app = axum::Router::new().route(
            "/a",
            axum::routing::get(move |headers: HeaderMap| {
                let (payload, seen, calls) = (payload.clone(), seen.clone(), calls.clone());
                async move {
                    let start = headers
                        .get("range")
                        .and_then(|v| v.to_str().ok())
                        .and_then(|v| v.strip_prefix("bytes=")?.strip_suffix('-')?.parse().ok())
                        .unwrap_or(0usize);
                    seen.lock().unwrap().push(start);
                    let first = calls.fetch_add(1, Ordering::SeqCst) == 0;
                    let length = (payload.len() - start).to_string();
                    let status = if start > 0 {
                        StatusCode::PARTIAL_CONTENT
                    } else {
                        StatusCode::OK
                    };
                    // The first answer sends half, then the connection drops.
                    let body = if first {
                        let half = payload[..payload.len() / 2].to_vec();
                        let drop = async {
                            tokio::time::sleep(Duration::from_millis(50)).await;
                            Err(io::Error::other("dropped"))
                        };
                        futures_util::stream::iter([Ok(half)])
                            .chain(futures_util::stream::once(drop))
                            .boxed()
                    } else {
                        futures_util::stream::iter([Ok::<_, io::Error>(payload[start..].to_vec())])
                            .boxed()
                    };
                    (
                        status,
                        [("content-length", length)],
                        axum::body::Body::from_stream(body),
                    )
                        .into_response()
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (format!("http://{address}/a"), starts)
    }

    #[tokio::test]
    async fn a_download_resumes_after_a_dropped_connection() {
        let payload: Arc<Vec<u8>> = Arc::new((0..40).flat_map(|_| 0..=255u8).collect());
        let (url, starts) = flaky(payload.clone()).await;
        let dir = temp();
        let target = dir.join("archive.tar.gz");
        let fetched = Mutex::new(0u64);
        let advance = |bytes: u64| *fetched.lock().unwrap() += bytes;
        fetch(&client(), &[url], &target, &advance).await.unwrap();
        assert_eq!(fs::read(&target).unwrap(), *payload);
        assert_eq!(*starts.lock().unwrap(), vec![0, payload.len() / 2]);
        assert_eq!(*fetched.lock().unwrap(), payload.len() as u64);
        fs::remove_dir_all(dir).unwrap();
    }

    /// The real download, into `KASS_SOUNDS_CHECK`; run by hand to compare
    /// with the files Python made (about 0.6 GB).
    #[tokio::test]
    #[ignore]
    async fn the_real_download() {
        let root = PathBuf::from(std::env::var("KASS_SOUNDS_CHECK").unwrap());
        download(&root, &Sources::kass(), &|_| {}).await.unwrap();
        assert!(ready(&root));
    }
}
