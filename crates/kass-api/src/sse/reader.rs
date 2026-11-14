//! The Server-Sent Events format
//! (https://html.spec.whatwg.org/multipage/server-sent-events.html).

use std::io::{self, BufRead};

use serde::de::DeserializeOwned;

/// One event: its name (`message` when it has none) and its data, the
/// `data:` lines joined with `\n`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SseEvent {
    pub event: String,
    pub data: String,
}

impl SseEvent {
    /// The data as JSON.
    pub fn json<T: DeserializeOwned>(&self) -> serde_json::Result<T> {
        serde_json::from_str(&self.data)
    }
}

/// Events read from any byte stream. A blank line dispatches an event,
/// `:` lines are comments, and an event cut off by the end of the stream is
/// dropped.
pub struct SseReader<R> {
    reader: R,
    started: bool,
    /// The last line ended with `\r`, so a `\n` next belongs to it.
    skip_lf: bool,
    event: String,
    data: String,
}

impl<R: BufRead> SseReader<R> {
    pub fn new(reader: R) -> Self {
        Self {
            reader,
            started: false,
            skip_lf: false,
            event: String::new(),
            data: String::new(),
        }
    }

    /// The next line, ended by `\n`, `\r\n` or `\r`; None at the end.
    fn read_line(&mut self) -> io::Result<Option<String>> {
        let mut line = Vec::new();
        loop {
            let available = match self.reader.fill_buf() {
                Ok(available) => available,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(error),
            };
            if available.is_empty() {
                return Ok(None);
            }
            let start = usize::from(std::mem::take(&mut self.skip_lf) && available[0] == b'\n');
            match available[start..]
                .iter()
                .position(|&b| b == b'\n' || b == b'\r')
            {
                Some(at) => {
                    let end = start + at;
                    line.extend_from_slice(&available[start..end]);
                    self.skip_lf = available[end] == b'\r';
                    self.reader.consume(end + 1);
                    let mut text = String::from_utf8_lossy(&line).into_owned();
                    if !std::mem::replace(&mut self.started, true) && text.starts_with('\u{feff}') {
                        text.remove(0);
                    }
                    return Ok(Some(text));
                }
                None => {
                    let len = available.len();
                    line.extend_from_slice(&available[start..]);
                    self.reader.consume(len);
                }
            }
        }
    }

    fn dispatch(&mut self) -> Option<SseEvent> {
        let event = std::mem::take(&mut self.event);
        if self.data.is_empty() {
            return None;
        }
        let mut data = std::mem::take(&mut self.data);
        data.pop();
        Some(SseEvent {
            event: if event.is_empty() {
                "message".into()
            } else {
                event
            },
            data,
        })
    }
}

impl<R: BufRead> Iterator for SseReader<R> {
    type Item = io::Result<SseEvent>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            let line = match self.read_line() {
                Ok(Some(line)) => line,
                Ok(None) => return None,
                Err(error) => return Some(Err(error)),
            };
            if line.is_empty() {
                if let Some(event) = self.dispatch() {
                    return Some(Ok(event));
                }
                continue;
            }
            if line.starts_with(':') {
                continue;
            }
            let (field, value) = match line.split_once(':') {
                Some((field, value)) => (field, value.strip_prefix(' ').unwrap_or(value)),
                None => (line.as_str(), ""),
            };
            match field {
                "event" => self.event = value.to_string(),
                "data" => {
                    self.data.push_str(value);
                    self.data.push('\n');
                }
                _ => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn events(text: &str) -> Vec<(String, String)> {
        SseReader::new(text.as_bytes())
            .map(|event| event.unwrap())
            .map(|event| (event.event, event.data))
            .collect()
    }

    fn pair(event: &str, data: &str) -> (String, String) {
        (event.into(), data.into())
    }

    #[test]
    fn joins_data_lines() {
        assert_eq!(
            events("data: one\ndata:two\ndata:  three\n\n"),
            vec![pair("message", "one\ntwo\n three")]
        );
    }

    #[test]
    fn names_events() {
        assert_eq!(
            events("event: changed\ndata: {}\n\nevent: peers\ndata: []\n\ndata: plain\n\n"),
            vec![
                pair("changed", "{}"),
                pair("peers", "[]"),
                pair("message", "plain")
            ]
        );
    }

    #[test]
    fn ignores_comments_and_unknown_fields() {
        assert_eq!(
            events(": keepalive\n\nid: 4\nretry: 100\n: note\ndata: x\n\n"),
            vec![pair("message", "x")]
        );
    }

    #[test]
    fn dispatches_only_on_a_blank_line() {
        assert_eq!(
            events("data: first\n\ndata: cut off"),
            vec![pair("message", "first")]
        );
        assert_eq!(
            events("event: lonely\n\ndata: y\n\n"),
            vec![pair("message", "y")]
        );
    }

    #[test]
    fn an_empty_data_line_is_an_event() {
        assert_eq!(events("data\n\n"), vec![pair("message", "")]);
        assert_eq!(events("data:\ndata:\n\n"), vec![pair("message", "\n")]);
    }

    #[test]
    fn accepts_every_line_ending_and_a_bom() {
        assert_eq!(
            events("\u{feff}event: a\r\ndata: 1\r\n\r\nevent: b\rdata: 2\r\r"),
            vec![pair("a", "1"), pair("b", "2")]
        );
    }

    #[test]
    fn reads_lines_split_across_reads() {
        let reader = std::io::BufReader::with_capacity(
            3,
            "event: changed\r\ndata: {\"a\":1}\r\n\r\n".as_bytes(),
        );
        let events: Vec<_> = SseReader::new(reader).map(Result::unwrap).collect();
        assert_eq!(
            events,
            vec![SseEvent {
                event: "changed".into(),
                data: "{\"a\":1}".into()
            }]
        );
    }
}
