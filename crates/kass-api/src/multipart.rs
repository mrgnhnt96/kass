//! A `multipart/form-data` body, the way a browser's `FormData` sends one.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

pub(crate) struct Multipart {
    boundary: String,
    body: Vec<u8>,
}

impl Multipart {
    pub(crate) fn new() -> Self {
        static COUNT: AtomicU64 = AtomicU64::new(0);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default();
        let count = COUNT.fetch_add(1, Ordering::Relaxed);
        Self::with_boundary(format!("----KassFormBoundary{nanos:x}{count:x}"))
    }

    pub(crate) fn with_boundary(boundary: String) -> Self {
        Self {
            boundary,
            body: Vec::new(),
        }
    }

    pub(crate) fn text(&mut self, name: &str, value: &str) {
        self.start(name);
        self.body.extend_from_slice(b"\r\n\r\n");
        self.body.extend_from_slice(value.as_bytes());
        self.body.extend_from_slice(b"\r\n");
    }

    pub(crate) fn file(&mut self, name: &str, file_name: &str, content_type: &str, bytes: &[u8]) {
        self.start(name);
        self.body.extend_from_slice(
            format!(
                "; filename=\"{}\"\r\nContent-Type: {content_type}\r\n\r\n",
                escape(file_name)
            )
            .as_bytes(),
        );
        self.body.extend_from_slice(bytes);
        self.body.extend_from_slice(b"\r\n");
    }

    pub(crate) fn content_type(&self) -> String {
        format!("multipart/form-data; boundary={}", self.boundary)
    }

    pub(crate) fn finish(mut self) -> Vec<u8> {
        self.body
            .extend_from_slice(format!("--{}--\r\n", self.boundary).as_bytes());
        self.body
    }

    fn start(&mut self, name: &str) {
        self.body.extend_from_slice(
            format!(
                "--{}\r\nContent-Disposition: form-data; name=\"{}\"",
                self.boundary,
                escape(name)
            )
            .as_bytes(),
        );
    }
}

/// Quotes and line breaks in a name, escaped as browsers do.
fn escape(text: &str) -> String {
    text.replace('"', "%22")
        .replace('\r', "%0D")
        .replace('\n', "%0A")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_fields_and_a_file() {
        let mut form = Multipart::with_boundary("XYZ".into());
        form.file("file", "take \"1\".wav", "audio/wav", b"RIFF");
        form.text("source", "file");
        assert_eq!(form.content_type(), "multipart/form-data; boundary=XYZ");
        assert_eq!(
            String::from_utf8(form.finish()).unwrap(),
            "--XYZ\r\nContent-Disposition: form-data; name=\"file\"; filename=\"take %221%22.wav\"\r\n\
             Content-Type: audio/wav\r\n\r\nRIFF\r\n\
             --XYZ\r\nContent-Disposition: form-data; name=\"source\"\r\n\r\nfile\r\n\
             --XYZ--\r\n"
        );
    }

    #[test]
    fn boundaries_differ() {
        assert_ne!(
            Multipart::new().content_type(),
            Multipart::new().content_type()
        );
    }
}
