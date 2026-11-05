//! The `crab` binary: the harness, served to another program over standard input and output.

/// Usage text printed for `--help` and after a bad argument.
const USAGE: &str = "usage: crab <serve|tools|providers|account|history|rename|import>\n\n  serve                    hold a conversation: read commands from standard input, write events to standard output\n  tools                    print the tools the agent can have, as a JSON list of name and description\n  providers                print the providers a conversation can be opened with, as a JSON list of id, name and sign-in\n  account <provider> [folder]  print who <provider> is signed in as with the logins in [folder], or in crab's own folder, or nothing\n  history <id> [folder]    print the saved conversation <id> as JSON: its title, saved settings and turns, which works while it is open elsewhere\n  rename <id> <folder> <title>  name the saved conversation <id>, which fails while it is open elsewhere\n  import <id> <folder>          save the conversation a client kept, read as JSON from standard input, as the conversation <id>";

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
        Some("providers") => {
            println!("{}", serde_json::to_string(&omc_session::providers())?);
            Ok(())
        }
        Some("account") => {
            let mut arguments = std::env::args().skip(2);
            let provider = arguments
                .next()
                .ok_or_else(|| anyhow::anyhow!("account needs a provider id"))?;
            let folder = arguments.next().map(std::path::PathBuf::from);
            if let Some(account) = omc_session::account(&provider, folder) {
                println!("{account}");
            }
            Ok(())
        }
        Some("history") => {
            let mut arguments = std::env::args().skip(2);
            let id = arguments
                .next()
                .ok_or_else(|| anyhow::anyhow!("history needs a conversation id"))?;
            let folder = arguments.next().map(std::path::PathBuf::from);
            let saved =
                omc_session::history(&id, folder).map_err(|reason| anyhow::anyhow!(reason))?;
            println!("{}", serde_json::to_string(&saved)?);
            Ok(())
        }
        Some("rename") => {
            let mut arguments = std::env::args().skip(2);
            let (Some(id), Some(folder), Some(title)) =
                (arguments.next(), arguments.next(), arguments.next())
            else {
                return Err(anyhow::anyhow!(
                    "rename needs a conversation id, a folder and a title"
                ));
            };
            omc_session::rename(&id, Some(std::path::PathBuf::from(folder)), &title)
                .map_err(|reason| anyhow::anyhow!(reason))
        }
        Some("import") => {
            let mut arguments = std::env::args().skip(2);
            let (Some(id), Some(folder)) = (arguments.next(), arguments.next()) else {
                return Err(anyhow::anyhow!(
                    "import needs a conversation id and a folder"
                ));
            };
            let imported: omc_wire::Imported = serde_json::from_reader(std::io::stdin().lock())?;
            omc_session::import(&id, Some(std::path::PathBuf::from(folder)), &imported)
                .map_err(|reason| anyhow::anyhow!(reason))
        }
        Some("-h" | "--help") => {
            println!("{USAGE}");
            Ok(())
        }
        _ => Err(anyhow::anyhow!(USAGE)),
    }
}
