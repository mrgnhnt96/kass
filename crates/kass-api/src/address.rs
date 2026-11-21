//! Server addresses (app/src/lib/serverAddress.ts).

use std::time::Duration;

use url::Url;

/// The bundled server on this Mac.
pub const LOCAL_SERVER_URL: &str = "http://127.0.0.1:17493";

pub const SERVER_PORT: u16 = 17493;

/// Turn what someone types for another Mac (`studio.local`,
/// `192.168.1.20:17493`, `http://studio.local/`) into a server URL, or None
/// when it isn't one. An http address without a port gets Kass's own.
pub fn normalize_server_address(input: &str) -> Option<String> {
    let text = input.trim();
    if text.is_empty() {
        return None;
    }
    let url = if has_scheme(text) {
        Url::parse(text)
    } else {
        Url::parse(&format!("http://{text}"))
    }
    .ok()?;
    let scheme = url.scheme();
    if scheme != "http" && scheme != "https" {
        return None;
    }
    let host = url.host_str().filter(|host| !host.is_empty())?;
    if !url.username().is_empty() || url.password().is_some() {
        return None;
    }
    if url.path() != "/" || url.query().is_some_and(|q| !q.is_empty()) {
        return None;
    }
    if url.fragment().is_some_and(|f| !f.is_empty()) {
        return None;
    }
    // URL drops a port that's the scheme's default, so look at what was typed.
    // An https address is something in front of Kass, on its own port.
    let port = match url.port() {
        Some(port) => Some(port),
        None if typed_port(text) || scheme == "https" => None,
        None => Some(SERVER_PORT),
    };
    Some(match port {
        Some(port) => format!("{scheme}://{host}:{port}"),
        None => format!("{scheme}://{host}"),
    })
}

/// Whether `url` answers as a Kass server within `timeout`.
pub fn answers_as_kass(url: &str, timeout: Duration) -> bool {
    kass_at(url, timeout).is_some()
}

/// The Kass server at `url`, if one answers within `timeout`: the id of
/// its Mac, or None from a Kass too old to say.
pub fn kass_at(url: &str, timeout: Duration) -> Option<Option<String>> {
    let agent = ureq::AgentBuilder::new().timeout(timeout).build();
    let health: serde_json::Value = agent
        .get(&format!("{url}/health"))
        .call()
        .ok()?
        .into_json()
        .ok()?;
    if health.get("status").and_then(|s| s.as_str()) != Some("healthy") {
        return None;
    }
    Some(
        health
            .get("mac_id")
            .and_then(|id| id.as_str())
            .filter(|id| !id.is_empty())
            .map(str::to_string),
    )
}

/// The first of `urls` whose Kass runs on the Mac with `mac_id`, asking
/// them all at once.
pub fn find_kass(urls: &[String], mac_id: &str, timeout: Duration) -> Option<String> {
    let found: Vec<bool> = std::thread::scope(|scope| {
        let asks: Vec<_> = urls
            .iter()
            .map(|url| {
                scope.spawn(move || kass_at(url, timeout).flatten().as_deref() == Some(mac_id))
            })
            .collect();
        asks.into_iter()
            .map(|ask| ask.join().unwrap_or(false))
            .collect()
    });
    urls.iter()
        .zip(found)
        .find_map(|(url, found)| found.then(|| url.clone()))
}

/// Where a Mac at a Bonjour address (`studio.local`) may be after its name
/// changed: when another device holds the name, macOS adds `-2`, `-3`, ...,
/// and a later restart may give the name back. None for other addresses.
pub fn renamed_addresses(url: &str) -> Vec<String> {
    const MOST: u32 = 9;
    let Ok(parsed) = Url::parse(url) else {
        return Vec::new();
    };
    let Some(host) = parsed.host_str().map(str::to_ascii_lowercase) else {
        return Vec::new();
    };
    let Some(name) = host.strip_suffix(".local").filter(|name| !name.is_empty()) else {
        return Vec::new();
    };
    // `studio-2` is `studio` renamed; `mac-mini-2024` keeps its number.
    let (base, current) = match name.rsplit_once('-') {
        Some((base, n)) if !base.is_empty() && !n.starts_with('0') => match n.parse::<u32>() {
            Ok(n) if (2..=MOST).contains(&n) => (base, n),
            _ => (name, 1),
        },
        _ => (name, 1),
    };
    let port = parsed
        .port()
        .map(|port| format!(":{port}"))
        .unwrap_or_default();
    (1..=MOST)
        .filter(|&n| n != current)
        .map(|n| match n {
            1 => format!("{}://{base}.local{port}", parsed.scheme()),
            n => format!("{}://{base}-{n}.local{port}", parsed.scheme()),
        })
        .collect()
}

/// `^[a-z][a-z\d+.-]*://`, ignoring case.
fn has_scheme(text: &str) -> bool {
    let Some((scheme, _)) = text.split_once("://") else {
        return false;
    };
    let mut bytes = scheme.bytes();
    bytes.next().is_some_and(|b| b.is_ascii_alphabetic())
        && bytes.all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'.' | b'-'))
}

/// `:\d+\/?$`
fn typed_port(text: &str) -> bool {
    let text = text.strip_suffix('/').unwrap_or(text);
    let digits = text.len() - text.trim_end_matches(|c: char| c.is_ascii_digit()).len();
    digits > 0 && text[..text.len() - digits].ends_with(':')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn normalized(input: &str) -> Option<String> {
        normalize_server_address(input)
    }

    #[test]
    fn a_renamed_mac_is_looked_for_under_the_names_macos_gives() {
        assert_eq!(
            renamed_addresses("http://studio.local:17493"),
            [
                "http://studio-2.local:17493",
                "http://studio-3.local:17493",
                "http://studio-4.local:17493",
                "http://studio-5.local:17493",
                "http://studio-6.local:17493",
                "http://studio-7.local:17493",
                "http://studio-8.local:17493",
                "http://studio-9.local:17493",
            ]
        );
        // Back from `-2` to its own name, or on to another number.
        let renamed = renamed_addresses("http://Studio-2.local:17493");
        assert_eq!(renamed[0], "http://studio.local:17493");
        assert_eq!(renamed[1], "http://studio-3.local:17493");
        assert_eq!(renamed.len(), 8);
        // A name that only ends in a number keeps it.
        assert_eq!(
            renamed_addresses("http://mac-mini-2024.local:17493")[0],
            "http://mac-mini-2024-2.local:17493"
        );
    }

    #[test]
    fn only_bonjour_names_are_renamed() {
        assert!(renamed_addresses("http://192.168.1.20:17493").is_empty());
        assert!(renamed_addresses("https://kass.example.com").is_empty());
        assert!(renamed_addresses("not a url").is_empty());
    }

    #[test]
    fn a_bare_host_gets_kass_port() {
        assert_eq!(
            normalized("studio.local").as_deref(),
            Some("http://studio.local:17493")
        );
        assert_eq!(
            normalized("  Studio.Local  ").as_deref(),
            Some("http://studio.local:17493")
        );
        assert_eq!(
            normalized("http://studio.local/").as_deref(),
            Some("http://studio.local:17493")
        );
    }

    #[test]
    fn a_typed_port_is_kept() {
        assert_eq!(
            normalized("192.168.1.20:17493").as_deref(),
            Some("http://192.168.1.20:17493")
        );
        assert_eq!(
            normalized("studio.local:8080/").as_deref(),
            Some("http://studio.local:8080")
        );
        assert_eq!(
            normalized("http://[::1]:9000").as_deref(),
            Some("http://[::1]:9000")
        );
    }

    #[test]
    fn a_typed_default_port_is_dropped() {
        assert_eq!(
            normalized("http://studio.local:80").as_deref(),
            Some("http://studio.local")
        );
        assert_eq!(
            normalized("studio.local:80").as_deref(),
            Some("http://studio.local")
        );
    }

    #[test]
    fn https_keeps_its_own_port() {
        assert_eq!(
            normalized("https://kass.example.com").as_deref(),
            Some("https://kass.example.com")
        );
        assert_eq!(
            normalized("HTTPS://kass.example.com:8443/").as_deref(),
            Some("https://kass.example.com:8443")
        );
    }

    #[test]
    fn rejects_what_isnt_a_server() {
        for input in [
            "",
            "   ",
            "ftp://studio.local",
            "http://user:pw@studio.local",
            "http://user@studio.local",
            "studio.local/kass",
            "studio.local?x=1",
            "studio.local#top",
            "studio.local:port",
            "http://",
        ] {
            assert_eq!(normalized(input), None, "{input:?}");
        }
    }

    #[test]
    fn empty_query_and_fragment_are_fine() {
        assert_eq!(
            normalized("studio.local/?").as_deref(),
            Some("http://studio.local:17493")
        );
        assert_eq!(
            normalized("studio.local/#").as_deref(),
            Some("http://studio.local:17493")
        );
    }

    #[test]
    fn nothing_listening_isnt_kass() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        drop(listener);
        assert!(!answers_as_kass(&url, Duration::from_millis(500)));
    }
}
