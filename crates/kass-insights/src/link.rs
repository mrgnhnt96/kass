//! Links into Insights from other screens: `?app=<bundle id>`,
//! `?apps=all` or `?apps=unknown` for what to show and `?period=7d` for the
//! period, as the Captures week card sends them.

use kass_api::types::{CaptureAppFilter, UsagePeriod};

/// What a link asks Insights to show; `None` leaves that as it was.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Link {
    pub filter: Option<CaptureAppFilter>,
    pub period: Option<UsagePeriod>,
}

impl Link {
    /// Reads a query string, with or without its leading `?`.
    pub fn parse(search: &str) -> Self {
        let mut link = Self::default();
        for pair in search.trim_start_matches('?').split('&') {
            let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
            let value = decode(value);
            match key {
                "app" if !value.is_empty() => link.filter = Some(CaptureAppFilter::App(value)),
                "apps" if value == "all" => link.filter = Some(CaptureAppFilter::All),
                "apps" if value == "unknown" => link.filter = Some(CaptureAppFilter::Unknown),
                "period" => {
                    link.period = [
                        UsagePeriod::Today,
                        UsagePeriod::Days7,
                        UsagePeriod::Days30,
                        UsagePeriod::All,
                    ]
                    .into_iter()
                    .find(|period| period.as_str() == value)
                }
                _ => {}
            }
        }
        link
    }

    pub fn is_empty(&self) -> bool {
        self.filter.is_none() && self.period.is_none()
    }
}

/// Undoes URL encoding: `%XX` escapes and `+` for a space. A value the
/// router quoted as JSON (`"30d"`) loses its quotes.
fn decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let escaped = (bytes[i] == b'%')
            .then(|| bytes.get(i + 1..i + 3))
            .flatten()
            .and_then(|hex| u8::from_str_radix(std::str::from_utf8(hex).ok()?, 16).ok());
        match (escaped, bytes[i]) {
            (Some(byte), _) => {
                out.push(byte);
                i += 3;
                continue;
            }
            (None, b'+') => out.push(b' '),
            (None, byte) => out.push(byte),
        }
        i += 1;
    }
    let text = String::from_utf8_lossy(&out).into_owned();
    match text.strip_prefix('"').and_then(|t| t.strip_suffix('"')) {
        Some(inner) => inner.to_string(),
        None => text,
    }
}

#[cfg(test)]
mod tests {
    use super::{Link, decode};
    use kass_api::types::{CaptureAppFilter, UsagePeriod};

    #[test]
    fn reads_the_app_and_period() {
        assert_eq!(
            Link::parse("?app=com.tinyspeck.slackmacgap&period=7d"),
            Link {
                filter: Some(CaptureAppFilter::App("com.tinyspeck.slackmacgap".into())),
                period: Some(UsagePeriod::Days7),
            }
        );
        assert_eq!(
            Link::parse("apps=unknown").filter,
            Some(CaptureAppFilter::Unknown)
        );
        assert_eq!(Link::parse("apps=all").filter, Some(CaptureAppFilter::All));
        assert_eq!(
            Link::parse("period=%2230d%22").period,
            Some(UsagePeriod::Days30)
        );
    }

    #[test]
    fn ignores_what_it_doesnt_know() {
        assert!(Link::parse("").is_empty());
        assert!(Link::parse("?model=whisper&period=1y&app=&apps=some").is_empty());
    }

    #[test]
    fn decodes_escapes() {
        assert_eq!(decode("a%20b+c"), "a b c");
        assert_eq!(decode("100%"), "100%");
        assert_eq!(decode("%zz"), "%zz");
    }
}
