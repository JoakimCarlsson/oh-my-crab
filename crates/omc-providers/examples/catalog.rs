//! Regenerates the model catalog crab compiles in, from a models.dev catalog read on standard input.
//!
//! `make catalog` runs it as
//! `curl -fsSL https://models.dev/api.json | cargo run -p omc-providers --example catalog -- crates/omc-providers/catalog/models.json`.
//! The file is replaced only once the whole snapshot is made, so a failed
//! download leaves the old one in place.

use std::io::Read;
use std::path::PathBuf;

/// Reads the models.dev catalog from standard input and writes the snapshot to the path given as the first argument, or to standard output without one.
///
/// # Errors
///
/// Fails when standard input cannot be read or is not a models.dev catalog, or when the snapshot cannot be written.
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut raw = String::new();
    std::io::stdin().read_to_string(&mut raw)?;
    let snapshot = omc_providers::catalog_snapshot(&raw)?;
    match std::env::args().nth(1).map(PathBuf::from) {
        Some(path) => {
            let staging = path.with_extension("json.part");
            std::fs::write(&staging, snapshot)?;
            std::fs::rename(staging, path)?;
        }
        None => print!("{snapshot}"),
    }
    Ok(())
}
