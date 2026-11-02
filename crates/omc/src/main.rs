//! The `crab` binary: a thin entrypoint over the `omc-ui` crate.

/// Starts the TUI and returns once the user quits.
///
/// # Errors
///
/// Returns any terminal error raised by the UI loop.
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    omc_ui::run().await
}
