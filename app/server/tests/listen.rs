//! A server listening on `[::]` (how sharing starts it) answers both IPv4
//! and IPv6 clients.

use std::path::PathBuf;

fn temp(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("kass-listen-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[tokio::test]
async fn listening_on_the_ipv6_wildcard_also_answers_ipv4_and_ipv6_loopback() {
    let data = temp("dual");
    let server = kass_server::start(kass_server::Config {
        listen: "[::]:0".parse().unwrap(),
        // Nothing runs here: a 502 from /health still proves the request arrived.
        worker: "127.0.0.1:9".parse().unwrap(),
        database: data.join("kass.db"),
        shared_server: None,
        voice_model: None,
        models_dir: Some(temp("dual-cache")),
        app_version: "1.2.3".into(),
    })
    .await
    .unwrap();
    let port = server.address().port();
    for host in ["127.0.0.1", "[::1]"] {
        let response = reqwest::get(format!("http://{host}:{port}/health"))
            .await
            .unwrap_or_else(|e| panic!("{host} did not connect: {e}"));
        assert_eq!(response.status(), 502, "{host}");
    }
}
