//! Kass's API without the app, in front of a worker started by hand:
//!
//! ```sh
//! cargo xtask db-init                     # data/kass.db, from the repo root
//! uvicorn backend.main:app --port 17494   # from the repo root
//! cargo run -p kass-server --example serve -- 127.0.0.1:17493 127.0.0.1:17494 data/kass.db
//! ```

#[tokio::main]
async fn main() -> Result<(), String> {
    let mut args = std::env::args().skip(1);
    let usage = "usage: serve <listen address> <worker address> <kass.db>";
    let listen = args
        .next()
        .ok_or(usage)?
        .parse()
        .map_err(|e| format!("{e}"))?;
    let worker = args
        .next()
        .ok_or(usage)?
        .parse()
        .map_err(|e| format!("{e}"))?;
    let database = args.next().ok_or(usage)?.into();
    let running = kass_server::start(kass_server::Config {
        listen,
        worker,
        database,
        shared_server: std::env::var("KASS_SHARED_SERVER").ok(),
        voice_model: Some(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../../backend/assets/silero_vad.onnx"),
        ),
    })
    .await?;
    println!("Kass API on {}, worker on {worker}", running.address());
    tokio::signal::ctrl_c().await.map_err(|e| e.to_string())?;
    running.stop();
    Ok(())
}
