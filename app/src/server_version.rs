//! Whether a server already listening on the port belongs to this version of
//! the app. After an update, a server from the old version can still be
//! running (the app crashed or was force-quit, or was replaced while open);
//! reusing it would run the old backend under the new app.

/// The version the server on `port` reports from `/health`, or `None` when it
/// doesn't answer or doesn't say.
pub fn running(port: u16) -> Option<String> {
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(3))
        .build()
        .ok()?;
    let body: serde_json::Value = client
        .get(format!("http://127.0.0.1:{port}/health"))
        .send()
        .ok()?
        .json()
        .ok()?;
    body.get("version")?.as_str().map(str::to_owned)
}

/// A server can be reused only when it reports exactly the app's version. One
/// that doesn't answer, or doesn't say, is replaced too.
pub fn is_current(running: Option<&str>, app: &str) -> bool {
    running == Some(app)
}

#[cfg(test)]
mod tests {
    use super::is_current;

    #[test]
    fn reuses_a_server_of_the_same_version() {
        assert!(is_current(Some("0.6.0"), "0.6.0"));
    }

    #[test]
    fn replaces_a_server_from_another_version() {
        assert!(!is_current(Some("0.5.0"), "0.6.0"));
        assert!(!is_current(Some("0.7.0"), "0.6.0"));
    }

    #[test]
    fn replaces_a_server_that_doesnt_say() {
        assert!(!is_current(None, "0.6.0"));
    }
}
