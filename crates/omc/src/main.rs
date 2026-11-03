//! The `crab` binary: the harness, served to another program over standard input and output.

/// Usage text printed for `--help` and after a bad argument.
const USAGE: &str = "usage: crab serve\n\n  serve  hold a conversation: read commands from standard input, write events to standard output";

/// Runs the conversation the command line asks for.
///
/// # Errors
///
/// Fails when the streams cannot be read or written.
#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    match std::env::args().nth(1).as_deref() {
        Some("serve") => {
            omc_session::serve(tokio::io::stdin(), tokio::io::stdout()).await?;
            Ok(())
        }
        Some("-h" | "--help") => {
            println!("{USAGE}");
            Ok(())
        }
        _ => Err(anyhow::anyhow!(USAGE)),
    }
}
