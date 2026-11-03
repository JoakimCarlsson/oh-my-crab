//! The `crab` binary: the harness, served to another program over standard input and output.

/// Usage text printed for `--help` and after a bad argument.
const USAGE: &str = "usage: crab <serve|tools>\n\n  serve  hold a conversation: read commands from standard input, write events to standard output\n  tools  print the tools the agent can have, as a JSON list of name and description";

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
        Some("tools") => {
            let tools = omc_session::tools().map_err(|reason| anyhow::anyhow!(reason))?;
            println!("{}", serde_json::to_string(&tools)?);
            Ok(())
        }
        Some("-h" | "--help") => {
            println!("{USAGE}");
            Ok(())
        }
        _ => Err(anyhow::anyhow!(USAGE)),
    }
}
