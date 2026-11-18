//! The models Kass can download, as `backend/backends/__init__.py` lists
//! them, and the files each loader fetches from its repository.

/// Which loader reads the model, and so which of its files it needs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Engine {
    /// mlx-audio's Whisper.
    Whisper,
    /// mlx-lm's Qwen3.
    Qwen,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Model {
    /// `whisper-turbo`, `qwen3-4b`: the name every route takes.
    pub name: &'static str,
    pub display_name: &'static str,
    pub engine: Engine,
    /// The Hugging Face repository.
    pub repo: &'static str,
    /// What `/transcribe` and `/llm/generate` call it: `turbo`, `4B`.
    pub size: &'static str,
}

pub const MODELS: [Model; 8] = [
    whisper(
        "whisper-base",
        "Whisper Base",
        "openai/whisper-base",
        "base",
    ),
    whisper(
        "whisper-small",
        "Whisper Small",
        "openai/whisper-small",
        "small",
    ),
    whisper(
        "whisper-medium",
        "Whisper Medium",
        "openai/whisper-medium",
        "medium",
    ),
    whisper(
        "whisper-large",
        "Whisper Large",
        "openai/whisper-large-v3",
        "large",
    ),
    whisper(
        "whisper-turbo",
        "Whisper Turbo",
        "openai/whisper-large-v3-turbo",
        "turbo",
    ),
    qwen(
        "qwen3-0.6b",
        "Qwen3 0.6B",
        "mlx-community/Qwen3-0.6B-4bit",
        "0.6B",
    ),
    qwen(
        "qwen3-1.7b",
        "Qwen3 1.7B",
        "mlx-community/Qwen3-1.7B-4bit",
        "1.7B",
    ),
    qwen("qwen3-4b", "Qwen3 4B", "mlx-community/Qwen3-4B-4bit", "4B"),
];

const fn whisper(
    name: &'static str,
    display_name: &'static str,
    repo: &'static str,
    size: &'static str,
) -> Model {
    Model {
        name,
        display_name,
        engine: Engine::Whisper,
        repo,
        size,
    }
}

const fn qwen(
    name: &'static str,
    display_name: &'static str,
    repo: &'static str,
    size: &'static str,
) -> Model {
    Model {
        name,
        display_name,
        engine: Engine::Qwen,
        repo,
        size,
    }
}

/// The `engine` model of a size, as the one-shot routes name them.
pub fn sized(engine: Engine, size: &str) -> Option<&'static Model> {
    MODELS
        .iter()
        .find(|model| model.engine == engine && model.size == size)
}

pub fn find(name: &str) -> Option<&'static Model> {
    MODELS.iter().find(|model| model.name == name)
}

/// mlx-audio's `DEFAULT_ALLOW_PATTERNS` (`mlx_audio/utils.py`).
const WHISPER_FILES: [&str; 11] = [
    "*.json",
    "*.safetensors",
    "*.py",
    "*.model",
    "*.tiktoken",
    "*.txt",
    "*.jsonl",
    "*.yaml",
    "*.wav",
    "*.pth",
    "*.npz",
];

/// mlx-lm's `_download` patterns (`mlx_lm/utils.py`).
const QWEN_FILES: [&str; 9] = [
    "*.json",
    "model*.safetensors",
    "*.py",
    "tokenizer.model",
    "*.tiktoken",
    "tiktoken.model",
    "*.txt",
    "*.jsonl",
    "*.jinja",
];

impl Model {
    /// Whether the loader would download `path` (a file in the repository),
    /// matched as huggingface_hub's `allow_patterns` match: fnmatch on the
    /// whole path, where `*` crosses `/`.
    pub fn wants(&self, path: &str) -> bool {
        let patterns: &[&str] = match self.engine {
            Engine::Whisper => &WHISPER_FILES,
            Engine::Qwen => &QWEN_FILES,
        };
        patterns.iter().any(|pattern| matches(pattern, path))
    }
}

/// fnmatch with `*` and literal characters, which is all the patterns use.
fn matches(pattern: &str, text: &str) -> bool {
    match pattern.split_once('*') {
        None => pattern == text,
        Some((head, rest)) => {
            let Some(text) = text.strip_prefix(head) else {
                return false;
            };
            if rest.is_empty() {
                return true;
            }
            text.char_indices()
                .map(|(i, _)| i)
                .chain(std::iter::once(text.len()))
                .any(|i| matches(rest, &text[i..]))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn whisper_fetches_its_weights_and_config_but_not_other_frameworks() {
        let turbo = find("whisper-turbo").unwrap();
        for file in [
            "model.safetensors",
            "config.json",
            "merges.txt",
            "normalizer.json",
        ] {
            assert!(turbo.wants(file), "{file}");
        }
        for file in [
            "pytorch_model.bin",
            "flax_model.msgpack",
            "tf_model.h5",
            "README.md",
            ".gitattributes",
        ] {
            assert!(!turbo.wants(file), "{file}");
        }
    }

    #[test]
    fn qwen_fetches_model_shards_and_tokenizer() {
        let qwen = find("qwen3-4b").unwrap();
        assert!(qwen.wants("model.safetensors"));
        assert!(qwen.wants("model-00001-of-00002.safetensors"));
        assert!(qwen.wants("tokenizer.json"));
        assert!(qwen.wants("chat_template.jinja"));
        assert!(!qwen.wants("consolidated.safetensors"));
        assert!(!qwen.wants("README.md"));
    }

    #[test]
    fn a_star_crosses_folders_as_fnmatch_does() {
        assert!(matches("*.json", "sub/dir/config.json"));
        assert!(!matches("model*.safetensors", "sub/model.safetensors"));
        assert!(matches("tokenizer.model", "tokenizer.model"));
    }

    #[test]
    fn names_match_the_worker() {
        assert!(find("qwen3-0.6b").is_some());
        assert!(find("whisper-tiny").is_none());
    }
}
