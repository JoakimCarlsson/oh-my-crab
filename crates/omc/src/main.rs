//! The `crab` binary: the harness, served to another program over standard input and output.

/// Usage text printed for `--help` and after a bad argument.
const USAGE: &str = "usage: crab <serve|tools|providers|account|history|rename|import|fork|memory|hooks|trust|permissions>\n\n  serve                    hold a conversation: read commands from standard input, write events to standard output\n  tools                    print the tools the agent can have, as a JSON list of name and description\n  providers                print the providers a conversation can be opened with, as a JSON list of id, name and sign-in\n  account <provider> [folder]  print who <provider> is signed in as with the logins in [folder], or in crab's own folder, or nothing\n  history <id> [folder]    print the saved conversation <id> as JSON: its title, saved settings and turns, which works while it is open elsewhere\n  rename <id> <folder> <title>  name the saved conversation <id>, which fails while it is open elsewhere\n  import <id> <folder>          save the conversation a client kept, read as JSON from standard input, as the conversation <id>\n  fork <id> <copy> <folder> [turn]  copy the branch the saved conversation <id> is on, up to before [turn] or whole, to the new conversation <copy>\n  memory [folder]          print the memories a conversation in [folder], or in the current folder, starts with, as JSON: its project's, which every worktree of the repository shares, and the user's\n  hooks [folder]           print the hooks a conversation in [folder], or in the current folder, would find, whether each runs, and its project's trust, as JSON\n  trust <folder> [on|off]  print whether the project <folder> belongs to is trusted, as JSON, or record that it is (on) or is not (off), for every worktree of its repository\n  permissions [folder]     print the permission rules a conversation in [folder], or in the current folder, would use, with where each comes from and whether it is used, as JSON\n  permissions add <allow|ask|deny> <user|project> <rule> [folder]  add a rule to crab's permissions file, for every project (user) or for the project of [folder] (project)\n  permissions remove <rule> [folder]  take a rule out of crab's permissions file, the user's and the project's of [folder]";

/// Runs the conversation the command line asks for.
///
/// `serve` refreshes the cached model catalog in the background, and ends the process as soon as the conversation is over, on [`omc_wire::Command::Close`] as on the end of standard input, without waiting for the thread that reads standard input to see its end.
///
/// # Errors
///
/// Fails when the streams cannot be read or written.
#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    match std::env::args().nth(1).as_deref() {
        Some("serve") => {
            tokio::spawn(omc_providers::refresh_catalog());
            omc_session::serve(tokio::io::stdin(), tokio::io::stdout()).await?;
            std::process::exit(0)
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
        Some("fork") => {
            let mut arguments = std::env::args().skip(2);
            let (Some(id), Some(copy), Some(folder)) =
                (arguments.next(), arguments.next(), arguments.next())
            else {
                return Err(anyhow::anyhow!(
                    "fork needs a conversation id, the id of the copy and a folder"
                ));
            };
            let before = arguments.next();
            omc_session::fork(
                &id,
                &copy,
                Some(std::path::PathBuf::from(folder)),
                before.as_deref(),
            )
            .map_err(|reason| anyhow::anyhow!(reason))
        }
        Some("memory") => {
            let folder = std::env::args().nth(2).map(std::path::PathBuf::from);
            let listed = omc_session::memory(folder).map_err(|reason| anyhow::anyhow!(reason))?;
            println!("{}", serde_json::to_string(&listed)?);
            Ok(())
        }
        Some("hooks") => {
            let folder = std::env::args().nth(2).map(std::path::PathBuf::from);
            let listed = omc_session::hooks(folder).map_err(|reason| anyhow::anyhow!(reason))?;
            println!("{}", serde_json::to_string(&listed)?);
            Ok(())
        }
        Some("trust") => {
            let mut arguments = std::env::args().skip(2);
            let folder = arguments
                .next()
                .map(std::path::PathBuf::from)
                .ok_or_else(|| anyhow::anyhow!("trust needs a folder"))?;
            let trusted = match arguments.next().as_deref() {
                None => None,
                Some("on") => Some(true),
                Some("off") => Some(false),
                Some(other) => {
                    return Err(anyhow::anyhow!("trust takes on or off, not {other}"));
                }
            };
            let standing = omc_session::trust(Some(folder), trusted)
                .map_err(|reason| anyhow::anyhow!(reason))?;
            println!("{}", serde_json::to_string(&standing)?);
            Ok(())
        }
        Some("permissions") => {
            let arguments: Vec<String> = std::env::args().skip(2).collect();
            let listed = match arguments.first().map(String::as_str) {
                Some("add") => {
                    let [_, effect, place, rule, rest @ ..] = arguments.as_slice() else {
                        return Err(anyhow::anyhow!(
                            "permissions add needs allow, ask or deny, user or project, and a rule"
                        ));
                    };
                    let project = match place.as_str() {
                        "user" => false,
                        "project" => true,
                        other => {
                            return Err(anyhow::anyhow!(
                                "a rule is kept for the user or the project, not {other}"
                            ));
                        }
                    };
                    let folder = rest.first().map(std::path::PathBuf::from);
                    omc_session::add_permission(effect, rule, project, folder)
                }
                Some("remove") => {
                    let [_, rule, rest @ ..] = arguments.as_slice() else {
                        return Err(anyhow::anyhow!("permissions remove needs a rule"));
                    };
                    omc_session::remove_permission(rule, rest.first().map(std::path::PathBuf::from))
                }
                folder => omc_session::permissions(folder.map(std::path::PathBuf::from)),
            }
            .map_err(|reason| anyhow::anyhow!(reason))?;
            println!("{}", serde_json::to_string(&listed)?);
            Ok(())
        }
        Some("-h" | "--help") => {
            println!("{USAGE}");
            Ok(())
        }
        _ => Err(anyhow::anyhow!(USAGE)),
    }
}
