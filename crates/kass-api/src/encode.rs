//! Percent-encoding the way the app does it.

/// Like JavaScript's `encodeURIComponent`, for path segments and the
/// hand-built queries in client.ts.
pub(crate) fn component(text: &str) -> String {
    encode(text, |b| {
        b.is_ascii_alphanumeric()
            || matches!(
                b,
                b'-' | b'_' | b'.' | b'!' | b'~' | b'*' | b'\'' | b'(' | b')'
            )
    })
}

/// Header values are ASCII, and Mac names often aren't ("Morgan’s MacBook").
/// The same as `percent_encode` in tauri/src-tauri/src/this_mac.rs.
pub(crate) fn header(text: &str) -> String {
    encode(text, |b| {
        b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~')
    })
}

/// A query string like JavaScript's `URLSearchParams` makes.
pub(crate) fn form<'a>(pairs: impl IntoIterator<Item = (&'a str, &'a str)>) -> String {
    url::form_urlencoded::Serializer::new(String::new())
        .extend_pairs(pairs)
        .finish()
}

fn encode(text: &str, keep: impl Fn(u8) -> bool) -> String {
    let mut out = String::with_capacity(text.len());
    for b in text.bytes() {
        if keep(b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_names_that_arent_ascii() {
        assert_eq!(header("Morgan’s MacBook"), "Morgan%E2%80%99s%20MacBook");
        assert_eq!(header("a(b)!"), "a%28b%29%21");
    }

    #[test]
    fn encodes_components_like_javascript() {
        assert_eq!(component("com.apple.Notes"), "com.apple.Notes");
        assert_eq!(component("a b/c?d&e(f)!"), "a%20b%2Fc%3Fd%26e(f)!");
    }

    #[test]
    fn builds_queries_like_url_search_params() {
        assert_eq!(
            form([("limit", "50"), ("app", "a b&c")]),
            "limit=50&app=a+b%26c"
        );
    }
}
