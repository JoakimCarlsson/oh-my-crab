//! Tool servers set up in files: crab's own, the project's and, when asked for, Claude Code's.
//!
//! Every file has the shape Claude Code's `.mcp.json` has: an object under `mcpServers` naming each server, which is either a program (`command`, `args`, `env`, `cwd`) or an address (`type` of `http` or `sse`, `url`, `headers`). Text in the commands, arguments, environment, addresses and headers may name environment variables as `${NAME}`, or `${NAME:-fallback}` for a fallback when the variable is unset or empty.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use omc_wire::{Reach, ToolServer};
use serde_json::Value;

/// The variable that names crab's own tool-server file, instead of `mcp.json` in crab's config folder.
pub const MCP_FILE_VARIABLE: &str = "OMC_MCP_FILE";

/// The variable that, set to `0`, keeps crab from reading any tool-server file, so only the servers the client passes are reached.
pub const MCP_CONFIG_VARIABLE: &str = "OMC_MCP_CONFIG";

/// The variable that, set to `1`, makes crab read the servers of Claude Code's `~/.claude.json` too.
pub const MCP_CLAUDE_VARIABLE: &str = "OMC_MCP_CLAUDE";

/// The files of a project tool servers are read from, under each of its folders from the working folder up to the repository root, the one that wins first.
pub const PROJECT_FILES: [&str; 2] = [".oh-my-crab/mcp.json", ".mcp.json"];

/// Where a tool server was set up, which decides whether it waits for the project to be trusted.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Origin {
    /// The client passed it when it opened the conversation.
    Client,
    /// The user's own files: crab's `mcp.json` or Claude Code's `~/.claude.json`.
    User,
    /// A file of the project; reached only once the project is trusted.
    Project,
}

/// How a server's address speaks.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Transport {
    /// Every message a POST, answered with JSON or an event stream.
    Streamable,
    /// The older transport: one long event stream that names where to POST, and carries the answers.
    Sse,
}

/// What a file says about signing in to a server with OAuth, for servers that do not register clients themselves.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct OAuthSetup {
    /// The client id registered with the authorization server ahead of time.
    pub client_id: Option<String>,
    /// The client secret that goes with it, for a confidential client.
    pub client_secret: Option<String>,
    /// The port the sign-in page sends the browser back to on this machine, for a client registered with a fixed address.
    pub callback_port: Option<u16>,
    /// The scopes to ask for, instead of those the server suggests.
    pub scopes: Option<String>,
}

/// One tool server to reach, with where it was set up.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Server {
    /// The name its tools are filed under.
    pub name: String,
    /// How it is reached, with the environment variables in its text replaced.
    pub reach: Reach,
    /// How an address speaks; [`Transport::Streamable`] for programs.
    pub transport: Transport,
    /// The folder a program is started in, or crab's own when there is none.
    pub folder: Option<PathBuf>,
    /// How to sign in with OAuth, when the file says.
    pub oauth: Option<OAuthSetup>,
    /// Where it was set up.
    pub origin: Origin,
    /// The file it was set up in, when it was set up in one.
    pub file: Option<PathBuf>,
    /// The server in a few words as the file wrote it, before variables were replaced: its command line or address.
    pub summary: String,
}

impl Server {
    /// The server the client passed as `given`.
    pub fn from_client(given: &ToolServer) -> Self {
        let summary = summary_of(&given.reach);
        Self {
            name: given.name.clone(),
            reach: given.reach.clone(),
            transport: Transport::Streamable,
            folder: None,
            oauth: None,
            origin: Origin::Client,
            file: None,
            summary,
        }
    }

    /// The server as project trust lists it: `tool server <name>: <command line or address>`.
    pub fn label(&self) -> String {
        format!("tool server {}: {}", self.name, self.summary)
    }
}

/// The servers found in files, and what could not be read.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Found {
    /// The servers, the one that wins first for each name.
    pub servers: Vec<Server>,
    /// One line for each file or entry that could not be read.
    pub problems: Vec<String>,
}

impl Found {
    /// Adds `other`'s servers whose names are not taken yet, and its problems.
    fn absorb(&mut self, other: Found) {
        for server in other.servers {
            if !self.servers.iter().any(|kept| kept.name == server.name) {
                self.servers.push(server);
            }
        }
        self.problems.extend(other.problems);
    }
}

/// Whether [`MCP_CONFIG_VARIABLE`] lets crab read tool-server files.
pub fn files_read() -> bool {
    !std::env::var(MCP_CONFIG_VARIABLE).is_ok_and(|value| off(&value))
}

/// Whether `value` turns a switch off: `0`, `false`, `off` or `no`, in any case.
fn off(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "0" | "false" | "off" | "no"
    )
}

/// Crab's own tool-server file: the one [`MCP_FILE_VARIABLE`] names, or `mcp.json` in crab's config folder; nothing when there is no config folder.
pub fn own_file() -> Option<PathBuf> {
    match std::env::var_os(MCP_FILE_VARIABLE) {
        Some(path) if !path.is_empty() => Some(PathBuf::from(path)),
        _ => dirs::config_dir().map(|base| base.join("oh-my-crab").join("mcp.json")),
    }
}

/// Claude Code's `.claude.json`, when [`MCP_CLAUDE_VARIABLE`] asks for it: in `claude_folder` when `CLAUDE_CONFIG_DIR` moved Claude Code's folder, else in `home`.
fn claude_file(home: Option<&Path>, claude_folder: Option<&Path>) -> Option<PathBuf> {
    if !std::env::var(MCP_CLAUDE_VARIABLE).is_ok_and(|value| value.trim() == "1") {
        return None;
    }
    match std::env::var_os("CLAUDE_CONFIG_DIR") {
        Some(folder) if !folder.is_empty() => {
            claude_folder.map(|folder| folder.join(".claude.json"))
        }
        _ => home.map(|home| home.join(".claude.json")),
    }
}

/// The user's own servers for work in `directory`, of the repository rooted at `root` when it is in one: those of crab's file `own`, then, when [`MCP_CLAUDE_VARIABLE`] asks for it, those Claude Code's `.claude.json` keeps for this project and then for every project; the first file that names a server wins.
///
/// Programs are started in `directory`. Nothing is read when [`files_read`] says not to.
pub fn user_servers(
    directory: &Path,
    root: Option<&Path>,
    own: Option<&Path>,
    home: Option<&Path>,
    claude_folder: Option<&Path>,
) -> Found {
    let mut found = Found::default();
    if !files_read() {
        return found;
    }
    if let Some(own) = own {
        found.absorb(read_file(own, Origin::User, directory));
    }
    if let Some(file) = claude_file(home, claude_folder)
        && let Some(settings) = read_json(&file, &mut found.problems)
    {
        for key in [Some(directory), root].into_iter().flatten() {
            let local = &settings["projects"][key.display().to_string()];
            found.absorb(servers_in(local, &file, Origin::User, directory));
        }
        found.absorb(servers_in(&settings, &file, Origin::User, directory));
    }
    found
}

/// The project's servers for work in `folders`, the folders from the working folder up to the repository root: under each, [`PROJECT_FILES`] in order, the first that names a server winning. A program is started in the project folder the file belongs to, the one that holds `.mcp.json` or `.oh-my-crab`.
///
/// Nothing is read when [`files_read`] says not to.
pub fn project_servers(folders: &[PathBuf]) -> Found {
    let mut found = Found::default();
    if !files_read() {
        return found;
    }
    for folder in folders {
        for file in PROJECT_FILES {
            found.absorb(read_file(&folder.join(file), Origin::Project, folder));
        }
    }
    found
}

/// The servers to reach: every one of `client`, then those of `project` and `user` whose names are not taken yet, in that order, so the client wins over the project and the project over the user.
pub fn merged(client: &[ToolServer], project: &[Server], user: &[Server]) -> Vec<Server> {
    let mut servers: Vec<Server> = Vec::new();
    let given = client.iter().map(Server::from_client);
    for server in given
        .chain(project.iter().cloned())
        .chain(user.iter().cloned())
    {
        if !servers.iter().any(|kept| kept.name == server.name) {
            servers.push(server);
        }
    }
    servers
}

/// The JSON in `file`, or nothing when it does not exist; a file that is not JSON is reported in `problems`.
fn read_json(file: &Path, problems: &mut Vec<String>) -> Option<Value> {
    let text = std::fs::read_to_string(file).ok()?;
    match serde_json::from_str(&text) {
        Ok(value) => Some(value),
        Err(error) => {
            problems.push(format!("{}: not JSON: {error}", file.display()));
            None
        }
    }
}

/// The servers of `file`, from `origin`, programs started in `folder`; nothing when the file does not exist.
fn read_file(file: &Path, origin: Origin, folder: &Path) -> Found {
    let mut problems = Vec::new();
    let Some(settings) = read_json(file, &mut problems) else {
        return Found {
            servers: Vec::new(),
            problems,
        };
    };
    servers_in(&settings, file, origin, folder)
}

/// The servers under `mcpServers` of `settings`, the JSON of `file`, from `origin`, programs started in `folder` unless they name a `cwd`.
pub fn servers_in(settings: &Value, file: &Path, origin: Origin, folder: &Path) -> Found {
    let mut found = Found::default();
    let Some(listed) = settings.get("mcpServers") else {
        return found;
    };
    let Some(listed) = listed.as_object() else {
        found
            .problems
            .push(format!("{}: `mcpServers` is not an object", file.display()));
        return found;
    };
    for (name, entry) in listed {
        match server_of(name, entry, file, origin, folder) {
            Ok(Some(server)) => found.servers.push(server),
            Ok(None) => {}
            Err(problem) => found
                .problems
                .push(format!("{}: server `{name}`: {problem}", file.display())),
        }
    }
    found
}

/// The server `name` that `entry` of `file` describes, from `origin`, a program started in `folder` unless it names a `cwd`; nothing when the entry is switched off.
///
/// # Errors
///
/// Fails with the reason when the entry has neither a `command` nor a `url`, or a field has the wrong shape.
fn server_of(
    name: &str,
    entry: &Value,
    file: &Path,
    origin: Origin,
    folder: &Path,
) -> Result<Option<Server>, String> {
    if !entry.is_object() {
        return Err("not an object".to_owned());
    }
    if entry["disabled"] == Value::Bool(true) || entry["enabled"] == Value::Bool(false) {
        return Ok(None);
    }
    let kind = entry["type"].as_str().unwrap_or_default();
    let oauth = oauth_of(&entry["oauth"]);
    if let Some(command) = entry["command"]
        .as_str()
        .filter(|_| kind.is_empty() || kind == "stdio")
    {
        let args = strings(&entry["args"], "args")?;
        let env = pairs(&entry["env"], "env")?;
        let summary = std::iter::once(command.to_owned())
            .chain(args.iter().cloned())
            .collect::<Vec<_>>()
            .join(" ");
        let cwd = entry["cwd"].as_str().map(|cwd| folder.join(expand(cwd)));
        return Ok(Some(Server {
            name: name.to_owned(),
            reach: Reach::Command {
                program: expand(command),
                args: args.iter().map(|arg| expand(arg)).collect(),
                env: env
                    .into_iter()
                    .map(|(key, value)| (key, expand(&value)))
                    .collect(),
            },
            transport: Transport::Streamable,
            folder: Some(cwd.unwrap_or_else(|| folder.to_path_buf())),
            oauth,
            origin,
            file: Some(file.to_path_buf()),
            summary,
        }));
    }
    let Some(url) = entry["url"].as_str() else {
        return Err("has neither a `command` nor a `url`".to_owned());
    };
    let transport = match kind {
        "" | "http" | "streamable-http" | "streamableHttp" => Transport::Streamable,
        "sse" => Transport::Sse,
        other => return Err(format!("unknown type `{other}`")),
    };
    let headers = pairs(&entry["headers"], "headers")?;
    Ok(Some(Server {
        name: name.to_owned(),
        reach: Reach::Http {
            url: expand(url),
            headers: headers
                .into_iter()
                .map(|(key, value)| (key, expand(&value)))
                .collect(),
        },
        transport,
        folder: None,
        oauth,
        origin,
        file: Some(file.to_path_buf()),
        summary: url.to_owned(),
    }))
}

/// The OAuth setup `value` gives, when it is an object.
fn oauth_of(value: &Value) -> Option<OAuthSetup> {
    value.as_object()?;
    let text = |key: &str| value[key].as_str().map(expand);
    Some(OAuthSetup {
        client_id: text("clientId"),
        client_secret: text("clientSecret"),
        callback_port: value["callbackPort"]
            .as_u64()
            .and_then(|port| u16::try_from(port).ok()),
        scopes: match &value["scopes"] {
            Value::Array(scopes) => Some(
                scopes
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(" "),
            ),
            other => other.as_str().map(str::to_owned),
        },
    })
}

/// The strings of the list `value`, the field `field`; none when it is missing.
///
/// # Errors
///
/// Fails when `value` is there but is not a list of strings.
fn strings(value: &Value, field: &str) -> Result<Vec<String>, String> {
    match value {
        Value::Null => Ok(Vec::new()),
        Value::Array(items) => items
            .iter()
            .map(|item| {
                item.as_str()
                    .map(str::to_owned)
                    .ok_or_else(|| format!("`{field}` holds something other than text"))
            })
            .collect(),
        _ => Err(format!("`{field}` is not a list")),
    }
}

/// The names and values of the object `value`, the field `field`, sorted by name; none when it is missing.
///
/// # Errors
///
/// Fails when `value` is there but is not an object of strings.
fn pairs(value: &Value, field: &str) -> Result<Vec<(String, String)>, String> {
    match value {
        Value::Null => Ok(Vec::new()),
        Value::Object(map) => map
            .iter()
            .map(|(key, value)| match value {
                Value::String(text) => Ok((key.clone(), text.clone())),
                Value::Number(number) => Ok((key.clone(), number.to_string())),
                Value::Bool(flag) => Ok((key.clone(), flag.to_string())),
                _ => Err(format!("`{field}.{key}` is not text")),
            })
            .collect::<Result<BTreeMap<_, _>, _>>()
            .map(|sorted| sorted.into_iter().collect()),
        _ => Err(format!("`{field}` is not an object")),
    }
}

/// `text` with every `${NAME}` replaced by the variable's value and every `${NAME:-fallback}` by the value, or the fallback when the variable is unset or empty; an unset variable without a fallback becomes empty text, and a `$` that starts no such form stays.
pub fn expand(text: &str) -> String {
    expand_with(text, &|name| std::env::var(name).ok())
}

/// `text` with its variables replaced as [`expand`] does, looking each up with `lookup`.
fn expand_with(text: &str, lookup: &dyn Fn(&str) -> Option<String>) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("${") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let Some(end) = after.find('}') else {
            out.push_str(&rest[start..]);
            return out;
        };
        let inner = &after[..end];
        let (name, fallback) = match inner.split_once(":-") {
            Some((name, fallback)) => (name, Some(fallback)),
            None => (inner, None),
        };
        let valid = !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
        if valid {
            let value = lookup(name).filter(|value| !value.is_empty());
            out.push_str(&value.or(fallback.map(str::to_owned)).unwrap_or_default());
        } else {
            out.push_str(&rest[start..start + 3 + end]);
        }
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    out
}

/// The way `reach` is described to a reader: its command line or its address.
fn summary_of(reach: &Reach) -> String {
    match reach {
        Reach::Command { program, args, .. } => std::iter::once(program.clone())
            .chain(args.iter().cloned())
            .collect::<Vec<_>>()
            .join(" "),
        Reach::Http { url, .. } => url.clone(),
    }
}

#[cfg(test)]
/// Tests for reading tool-server files.
mod tests {
    use super::*;
    use serde_json::json;

    /// Variables are replaced, with fallbacks for unset and empty ones, and text that only looks like one is kept.
    #[test]
    fn variables_expand() {
        let lookup = |name: &str| match name {
            "TOKEN" => Some("abc".to_owned()),
            "EMPTY" => Some(String::new()),
            _ => None,
        };
        let expanded = |text| expand_with(text, &lookup);
        assert_eq!(expanded("Bearer ${TOKEN}"), "Bearer abc");
        assert_eq!(expanded("${MISSING:-dflt}/${EMPTY:-e}"), "dflt/e");
        assert_eq!(expanded("x${MISSING}y"), "xy");
        assert_eq!(
            expanded("$HOME ${bad name} ${open"),
            "$HOME ${bad name} ${open"
        );
    }

    /// Programs and addresses are read in Claude Code's shape, switched-off entries are skipped and broken ones reported.
    #[test]
    fn entries_of_both_kinds() {
        let file = Path::new("/p/.mcp.json");
        let settings = json!({"mcpServers": {
            "files": {"command": "files-mcp", "args": ["--root", "."], "env": {"LEVEL": "warn"}},
            "docs": {"type": "http", "url": "https://docs.example/mcp", "headers": {"X-Key": "k"},
                     "oauth": {"clientId": "crab-client", "callbackPort": 8765, "scopes": ["read", "write"]}},
            "old": {"type": "sse", "url": "https://old.example/sse"},
            "off": {"command": "x", "disabled": true},
            "broken": {"args": []}
        }});
        let found = servers_in(&settings, file, Origin::Project, Path::new("/p"));
        let names: Vec<&str> = found.servers.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["docs", "files", "old"]);
        let files = &found.servers[1];
        assert_eq!(files.folder.as_deref(), Some(Path::new("/p")));
        assert_eq!(files.summary, "files-mcp --root .");
        assert_eq!(files.label(), "tool server files: files-mcp --root .");
        assert_eq!(
            files.reach,
            Reach::Command {
                program: "files-mcp".into(),
                args: vec!["--root".into(), ".".into()],
                env: vec![("LEVEL".into(), "warn".into())],
            }
        );
        let docs = &found.servers[0];
        let oauth = docs.oauth.clone().unwrap();
        assert_eq!(oauth.client_id.as_deref(), Some("crab-client"));
        assert_eq!(oauth.callback_port, Some(8765));
        assert_eq!(oauth.scopes.as_deref(), Some("read write"));
        assert_eq!(found.servers[2].transport, Transport::Sse);
        assert_eq!(found.problems.len(), 1);
        assert!(found.problems[0].contains("broken"));
    }

    /// The client wins over the project, and the project over the user.
    #[test]
    fn precedence() {
        let server = |name: &str, origin, summary: &str| Server {
            name: name.into(),
            reach: Reach::Http {
                url: summary.into(),
                headers: vec![],
            },
            transport: Transport::Streamable,
            folder: None,
            oauth: None,
            origin,
            file: None,
            summary: summary.into(),
        };
        let client = vec![ToolServer {
            name: "a".into(),
            reach: Reach::Http {
                url: "client".into(),
                headers: vec![],
            },
        }];
        let project = vec![
            server("a", Origin::Project, "p-a"),
            server("b", Origin::Project, "p-b"),
        ];
        let user = vec![
            server("b", Origin::User, "u-b"),
            server("c", Origin::User, "u-c"),
        ];
        let merged = merged(&client, &project, &user);
        let picked: Vec<(&str, &str)> = merged
            .iter()
            .map(|s| (s.name.as_str(), s.summary.as_str()))
            .collect();
        assert_eq!(picked, [("a", "client"), ("b", "p-b"), ("c", "u-c")]);
        assert_eq!(merged[0].origin, Origin::Client);
    }

    /// The nearer project folder wins, and crab's own project file wins over `.mcp.json` in the same folder.
    #[test]
    fn nearer_project_files_win() {
        let base = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../build/tests/mcp-config-project");
        let _ = std::fs::remove_dir_all(&base);
        let inner = base.join("inner");
        std::fs::create_dir_all(inner.join(".oh-my-crab")).unwrap();
        let write = |path: PathBuf, url: &str| {
            std::fs::write(
                path,
                json!({"mcpServers": {"s": {"url": url}, url: {"url": url}}}).to_string(),
            )
            .unwrap();
        };
        write(base.join(".mcp.json"), "outer");
        write(inner.join(".mcp.json"), "inner-claude");
        write(inner.join(".oh-my-crab/mcp.json"), "inner-own");
        let found = project_servers(&[inner.clone(), base.clone()]);
        let s = found.servers.iter().find(|s| s.name == "s").unwrap();
        assert_eq!(s.summary, "inner-own");
        assert_eq!(s.folder, None);
        assert_eq!(found.servers.len(), 4);
        let _ = std::fs::remove_dir_all(&base);
    }
}
