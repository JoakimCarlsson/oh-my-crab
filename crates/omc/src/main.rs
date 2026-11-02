//! The `crab` binary: a thin entrypoint over the `omc-ui` crate.

use omc_ui::Options;

/// Usage text printed for `--help` and after a bad argument.
const USAGE: &str = "usage: crab [--yolo]\n\n  --yolo  run bash, edit and write without asking";

/// Parses the command line into [`Options`].
///
/// # Errors
///
/// Returns a message for an unknown argument. `--help` exits the process.
fn parse_args(args: impl Iterator<Item = String>) -> Result<Options, String> {
    let mut options = Options::default();
    for arg in args {
        match arg.as_str() {
            "--yolo" => options.yolo = true,
            "-h" | "--help" => {
                println!("{USAGE}");
                std::process::exit(0);
            }
            other => return Err(format!("unknown argument: {other}\n{USAGE}")),
        }
    }
    Ok(options)
}

/// Starts the TUI and returns once the user quits.
///
/// # Errors
///
/// Returns any terminal error raised by the UI loop.
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let options = parse_args(std::env::args().skip(1)).map_err(anyhow::Error::msg)?;
    omc_ui::run(options).await
}
