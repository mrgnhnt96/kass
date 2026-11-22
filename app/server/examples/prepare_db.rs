//! Create kass.db, or bring an older one up to date, as the app does before
//! it starts the worker (`cargo xtask db-init` runs this on data/kass.db):
//!
//! ```sh
//! cargo run -p kass-server --example prepare_db -- /path/to/kass.db
//! ```

fn main() -> Result<(), String> {
    let mut args = std::env::args().skip(1);
    let path: std::path::PathBuf = args.next().ok_or("usage: prepare_db <kass.db>")?.into();
    kass_server::db::prepare(&path)?;
    kass_server::db::seed(&path)?;
    println!("{} is ready", path.display());
    Ok(())
}
