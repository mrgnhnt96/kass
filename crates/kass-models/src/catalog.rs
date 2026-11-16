//! What the page knows about the models beyond the server's list: their
//! families and roles, which capture setting picks each one, and how sizes
//! and Hugging Face facts read.

use kass_api::types::{
    CaptureSettings, CaptureSettingsUpdate, ModelStatus, Qwen3ModelSize, WhisperModelSize,
};
use std::collections::HashMap;

/// A model's description, by model name.
pub fn description(model_name: &str) -> Option<&'static str> {
    Some(match model_name {
        "whisper-base" => {
            "Smallest Whisper model (74M parameters). Fast transcription with moderate accuracy."
        }
        "whisper-small" => {
            "Whisper Small (244M parameters). Good balance of speed and accuracy for transcription."
        }
        "whisper-medium" => {
            "Whisper Medium (769M parameters). Higher accuracy transcription at moderate speed."
        }
        "whisper-large" => {
            "Whisper Large (1.5B parameters). Best accuracy for speech-to-text across multiple languages."
        }
        "whisper-turbo" => {
            "Whisper Large v3 Turbo. Pruned for significantly faster inference while maintaining near-large accuracy."
        }
        "qwen3-0.6b" => {
            "Qwen3 0.6B — smallest of the Qwen3 instruct family. Very fast on CPU, runs at ~400 MB quantized on Apple Silicon. Good for dictation refinement and short completions."
        }
        "qwen3-1.7b" => {
            "Qwen3 1.7B — balanced size and quality. Handles subtle self-corrections and technical vocabulary better than the 0.6B. Runs at ~1.1 GB quantized on Apple Silicon."
        }
        "qwen3-4b" => {
            "Qwen3 4B — highest quality local refinement and longer-form reasoning. Runs at ~2.5 GB quantized on Apple Silicon."
        }
        _ => return None,
    })
}

/// What dictation uses a model for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    Transcription,
    Refinement,
}

impl Role {
    pub fn label(self) -> String {
        kass_i18n::t(match self {
            Role::Transcription => "models-roles-transcription",
            Role::Refinement => "models-roles-refinement",
        })
    }
}

/// The models of one family, in the server's order.
#[derive(Clone, Debug, PartialEq)]
pub struct Group {
    /// The model name's prefix: `whisper`, `qwen3`.
    pub family: String,
    pub models: Vec<ModelStatus>,
}

pub fn family(model_name: &str) -> &str {
    model_name.split('-').next().unwrap_or(model_name)
}

/// The models grouped by family, keeping the server's order.
pub fn groups(models: &[ModelStatus]) -> Vec<Group> {
    let mut groups: Vec<Group> = Vec::new();
    for model in models {
        let family = family(&model.model_name);
        match groups.iter_mut().find(|group| group.family == family) {
            Some(group) => group.models.push(model.clone()),
            None => groups.push(Group {
                family: family.to_string(),
                models: vec![model.clone()],
            }),
        }
    }
    groups
}

/// What dictation would use a model for, if anything.
pub fn role_of(model_name: &str) -> Option<Role> {
    match family(model_name) {
        "whisper" => Some(Role::Transcription),
        "qwen3" => Some(Role::Refinement),
        _ => None,
    }
}

/// The size part of a model name: `turbo` in `whisper-turbo`.
fn size_of(model_name: &str) -> &str {
    model_name.split_once('-').map_or("", |(_, size)| size)
}

/// The capture setting that makes dictation use a model.
pub fn setting_for(model_name: &str) -> Option<CaptureSettingsUpdate> {
    let size = size_of(model_name);
    match role_of(model_name)? {
        Role::Transcription => {
            let stt = match size {
                "base" => WhisperModelSize::Base,
                "small" => WhisperModelSize::Small,
                "medium" => WhisperModelSize::Medium,
                "large" => WhisperModelSize::Large,
                "turbo" => WhisperModelSize::Turbo,
                _ => return None,
            };
            Some(CaptureSettingsUpdate {
                stt_model: Some(stt),
                ..Default::default()
            })
        }
        Role::Refinement => {
            let llm = match size {
                "0.6b" => Qwen3ModelSize::Size0_6B,
                "1.7b" => Qwen3ModelSize::Size1_7B,
                "4b" => Qwen3ModelSize::Size4B,
                _ => return None,
            };
            Some(CaptureSettingsUpdate {
                llm_model: Some(llm),
                ..Default::default()
            })
        }
    }
}

fn qwen_name(size: Qwen3ModelSize) -> Option<&'static str> {
    match size {
        Qwen3ModelSize::Size0_6B => Some("qwen3-0.6b"),
        Qwen3ModelSize::Size1_7B => Some("qwen3-1.7b"),
        Qwen3ModelSize::Size4B => Some("qwen3-4b"),
        Qwen3ModelSize::Unknown => None,
    }
}

/// The models dictation is set to use, by model name.
pub fn in_use(settings: Option<&CaptureSettings>) -> HashMap<String, Role> {
    let mut in_use = HashMap::new();
    let Some(settings) = settings else {
        return in_use;
    };
    if settings.stt_model != WhisperModelSize::Unknown {
        in_use.insert(
            format!("whisper-{}", settings.stt_model.as_str()),
            Role::Transcription,
        );
    }
    if let Some(name) = qwen_name(settings.llm_model) {
        in_use.insert(name.to_string(), Role::Refinement);
    }
    in_use
}

/// A model's folder in the Hugging Face hub cache.
pub fn hf_cache_folder(cache_dir: &str, repo_id: &str) -> String {
    format!(
        "{}/models--{}",
        cache_dir.trim_end_matches('/'),
        repo_id.replace('/', "--")
    )
}

pub fn format_bytes(bytes: f64) -> String {
    if bytes <= 0. {
        return "0 B".into();
    }
    const SIZES: [&str; 4] = ["B", "KB", "MB", "GB"];
    let i = ((bytes.ln() / 1024f64.ln()).floor() as usize).min(SIZES.len() - 1);
    format!("{:.1} {}", bytes / 1024f64.powi(i as i32), SIZES[i])
}

pub fn format_size_mb(size_mb: f64) -> String {
    if size_mb < 1024. {
        format!("{size_mb:.1} MB")
    } else {
        format!("{:.2} GB", size_mb / 1024.)
    }
}

pub fn format_count(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.)
    } else if n >= 1_000 {
        format!("{:.1}k", n as f64 / 1_000.)
    } else {
        n.to_string()
    }
}

pub fn format_license(license: &str) -> String {
    match license {
        "apache-2.0" => "Apache 2.0",
        "mit" => "MIT",
        "cc-by-4.0" => "CC BY 4.0",
        "cc-by-sa-4.0" => "CC BY-SA 4.0",
        "cc-by-nc-4.0" => "CC BY-NC 4.0",
        "openrail++" => "OpenRAIL++",
        "openrail" => "OpenRAIL",
        other => other,
    }
    .to_string()
}

/// `automatic-speech-recognition` → `Automatic Speech Recognition`.
pub fn format_pipeline_tag(tag: &str) -> String {
    tag.split('-')
        .map(|word| {
            let mut chars = word.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().chain(chars).collect(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// When a download started, as a local time of day. The server writes UTC,
/// with or without an offset; anything else is shown as it came.
pub fn local_time(started_at: &str) -> String {
    use chrono::{DateTime, Local, NaiveDateTime, Utc};
    let utc = DateTime::parse_from_rfc3339(started_at)
        .map(|time| time.with_timezone(&Utc))
        .or_else(|_| {
            NaiveDateTime::parse_from_str(started_at, "%Y-%m-%dT%H:%M:%S%.f")
                .map(|time| time.and_utc())
        });
    match utc {
        Ok(time) => time
            .with_timezone(&Local)
            .format("%-I:%M:%S %p")
            .to_string(),
        Err(_) => started_at.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model(name: &str) -> ModelStatus {
        ModelStatus {
            model_name: name.into(),
            display_name: name.into(),
            hf_repo_id: None,
            downloaded: false,
            downloading: false,
            size_mb: None,
            loaded: false,
        }
    }

    #[test]
    fn groups_by_family_in_order() {
        let models = [
            model("whisper-base"),
            model("qwen3-0.6b"),
            model("whisper-turbo"),
        ];
        let groups = groups(&models);
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].family, "whisper");
        assert_eq!(groups[0].models.len(), 2);
        assert_eq!(groups[1].family, "qwen3");
    }

    #[test]
    fn settings_pick_models() {
        let stt = setting_for("whisper-turbo").unwrap();
        assert_eq!(stt.stt_model, Some(WhisperModelSize::Turbo));
        assert_eq!(stt.llm_model, None);
        let llm = setting_for("qwen3-1.7b").unwrap();
        assert_eq!(llm.llm_model, Some(Qwen3ModelSize::Size1_7B));
        assert!(setting_for("whisper-huge").is_none());
        assert!(setting_for("other-model").is_none());
    }

    #[test]
    fn roles() {
        assert_eq!(role_of("whisper-small"), Some(Role::Transcription));
        assert_eq!(role_of("qwen3-4b"), Some(Role::Refinement));
        assert_eq!(role_of("kokoro"), None);
    }

    #[test]
    fn formats() {
        assert_eq!(format_bytes(0.), "0 B");
        assert_eq!(format_bytes(1536.), "1.5 KB");
        assert_eq!(format_bytes(3. * 1024. * 1024. * 1024.), "3.0 GB");
        assert_eq!(format_size_mb(512.), "512.0 MB");
        assert_eq!(format_size_mb(1536.), "1.50 GB");
        assert_eq!(format_count(999), "999");
        assert_eq!(format_count(1_500), "1.5k");
        assert_eq!(format_count(2_300_000), "2.3M");
        assert_eq!(format_license("apache-2.0"), "Apache 2.0");
        assert_eq!(format_license("other"), "other");
        assert_eq!(
            format_pipeline_tag("automatic-speech-recognition"),
            "Automatic Speech Recognition"
        );
    }

    #[test]
    fn times() {
        assert_eq!(local_time("soon"), "soon");
        assert!(local_time("2026-11-16T10:00:00").ends_with('M'));
        assert!(local_time("2026-11-16T10:00:00.123+00:00").ends_with('M'));
    }

    #[test]
    fn cache_folder() {
        assert_eq!(
            hf_cache_folder("/cache/", "openai/whisper-base"),
            "/cache/models--openai--whisper-base"
        );
    }
}
