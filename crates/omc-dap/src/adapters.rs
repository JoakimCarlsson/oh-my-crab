//! The debug adapters the harness knows, which languages each debugs, how each is started, and how the user points the harness at another.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{Map, Value, json};

/// The environment variable naming the file adapters are read from instead of `debuggers.json` in crab's config folder.
const FILE_ENV: &str = "OMC_DEBUGGERS_FILE";

/// The start of the environment variables that replace one adapter's command, followed by its id in capitals with `-` as `_`, as in `OMC_DAP_LLDB_DAP`.
const COMMAND_ENV: &str = "OMC_DAP_";

/// How long checking that a Python interpreter has the adapter's module may take.
const PROBE_WAIT: Duration = Duration::from_secs(10);

/// How the harness talks with an adapter.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Transport {
    /// Over the adapter's standard input and output.
    Stdio,
    /// Over a socket: the adapter is told to listen on a free port, written `{port}` in its arguments, and the harness connects to it.
    Port,
}

/// What stopping a program as it starts takes with an adapter.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Entry {
    /// A breakpoint on the function of this name.
    Function(String),
    /// This launch setting set to `true`.
    Flag(String),
}

/// A debug adapter: what it debugs, how it is started, and what it is told when a program is launched or attached to.
#[derive(Clone, Debug, PartialEq)]
pub struct Adapter {
    /// The name it is known and chosen by, such as `lldb-dap`.
    pub id: String,
    /// The languages it debugs, such as `c` or `python`; `native` stands for any compiled program.
    pub languages: Vec<String>,
    /// The commands that start it, each a program and its arguments, the first one found winning; a program with a `/` is taken relative to the project.
    pub commands: Vec<Vec<String>>,
    /// How the harness talks with it.
    pub transport: Transport,
    /// The Python module the program of its command must be able to import, for an adapter that is a Python module.
    pub module: Option<String>,
    /// The settings every launch request carries, before the program's own.
    pub launch: Map<String, Value>,
    /// The settings launching a compiled program rather than source code carries as well.
    pub binary: Map<String, Value>,
    /// The settings every request to attach to a process carries.
    pub attach: Map<String, Value>,
    /// The settings a request to attach through an adapter already listening at an address carries.
    pub remote: Map<String, Value>,
    /// How a program is stopped as it starts.
    pub entry: Entry,
    /// What to install to have it, for the message when nothing is.
    pub install: String,
}

impl Adapter {
    /// An adapter called `id`, for `languages`, started by the first of `commands` found and spoken to over its standard input and output.
    fn new(id: &str, languages: &[&str], commands: &[&[&str]], install: &str) -> Self {
        Self {
            id: id.to_owned(),
            languages: languages
                .iter()
                .map(|language| (*language).to_owned())
                .collect(),
            commands: commands
                .iter()
                .map(|command| command.iter().map(|part| (*part).to_owned()).collect())
                .collect(),
            transport: Transport::Stdio,
            module: None,
            launch: Map::new(),
            binary: Map::new(),
            attach: Map::new(),
            remote: Map::new(),
            entry: Entry::Function("main".to_owned()),
            install: install.to_owned(),
        }
    }

    /// Whether it debugs `language`.
    pub fn debugs(&self, language: &str) -> bool {
        self.languages.iter().any(|known| known == language)
    }

    /// The program and arguments that start it for the project at `root`: the first of its commands whose program is found, and, for a Python module, can import it.
    pub async fn command(&self, root: &Path) -> Option<(PathBuf, Vec<String>)> {
        for command in &self.commands {
            let Some((program, arguments)) = command.split_first() else {
                continue;
            };
            let Some(found) = find_program(program, root) else {
                continue;
            };
            if let Some(module) = &self.module
                && !imports(&found, module).await
            {
                continue;
            }
            return Some((found, arguments.to_vec()));
        }
        None
    }
}

/// The adapters known without being told, in the order they are preferred for each language.
pub fn default_adapters() -> Vec<Adapter> {
    let native = ["native", "c", "cpp", "rust", "zig", "swift", "objc"];
    let lldb = Adapter::new(
        "lldb-dap",
        &native,
        &[&["lldb-dap"]],
        "lldb-dap (it comes with LLVM's lldb package)",
    );
    let mut codelldb = Adapter::new(
        "codelldb",
        &native,
        &[&["codelldb", "--port", "{port}"]],
        "codelldb (the CodeLLDB extension's adapter)",
    );
    codelldb.transport = Transport::Port;
    codelldb.launch = object(json!({"terminal": "console"}));
    let gdb = Adapter::new(
        "gdb",
        &["native", "c", "cpp", "rust"],
        &[&["gdb", "-i", "dap"]],
        "gdb 14 or newer",
    );
    let mut dlv = Adapter::new(
        "dlv",
        &["go"],
        &[&["dlv", "dap", "--listen", "127.0.0.1:{port}"]],
        "Delve (go install github.com/go-delve/delve/cmd/dlv@latest)",
    );
    dlv.transport = Transport::Port;
    dlv.launch = object(json!({"mode": "debug"}));
    dlv.binary = object(json!({"mode": "exec"}));
    dlv.attach = object(json!({"mode": "local"}));
    dlv.remote = object(json!({"mode": "remote"}));
    dlv.entry = Entry::Function("main.main".to_owned());
    let mut debugpy = Adapter::new(
        "debugpy",
        &["python"],
        &[
            &[".venv/bin/python", "-m", "debugpy.adapter"],
            &["venv/bin/python", "-m", "debugpy.adapter"],
            &["python3", "-m", "debugpy.adapter"],
            &["python", "-m", "debugpy.adapter"],
        ],
        "debugpy (python3 -m pip install debugpy, in the project's virtual environment when it has one)",
    );
    debugpy.module = Some("debugpy".to_owned());
    debugpy.launch = object(json!({"console": "internalConsole", "justMyCode": false}));
    debugpy.attach = object(json!({"justMyCode": false}));
    debugpy.remote = object(json!({"justMyCode": false}));
    debugpy.entry = Entry::Flag("stopOnEntry".to_owned());
    vec![lldb, codelldb, gdb, dlv, debugpy]
}

/// The file adapters are read from: the one `OMC_DEBUGGERS_FILE` names, else `debuggers.json` in crab's config folder; nothing when there is no config folder.
pub fn adapters_file() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os(FILE_ENV).filter(|path| !path.is_empty()) {
        return Some(PathBuf::from(path));
    }
    dirs::config_dir().map(|base| base.join("oh-my-crab").join("debuggers.json"))
}

/// The adapters to choose from: the known ones changed and added to by the adapters file, and each one's command replaced by its environment variable when that is set.
///
/// # Errors
///
/// Returns why when the adapters file exists and cannot be read or is wrong.
pub(crate) fn configured() -> Result<Vec<Adapter>, String> {
    let mut adapters = default_adapters();
    if let Some(path) = adapters_file()
        && path.is_file()
    {
        let text = std::fs::read_to_string(&path)
            .map_err(|error| format!("{} cannot be read: {error}", path.display()))?;
        let value: Value = serde_json::from_str(&text)
            .map_err(|error| format!("{} is not JSON: {error}", path.display()))?;
        adapters = merged(adapters, &value)
            .map_err(|reason| format!("{} is wrong: {reason}", path.display()))?;
    }
    for adapter in &mut adapters {
        if let Some(command) = command_from_env(&adapter.id) {
            adapter.commands = vec![command];
            adapter.module = None;
        }
    }
    Ok(adapters)
}

/// The command in the environment variable for the adapter `id`, split on white space, when it is set.
fn command_from_env(id: &str) -> Option<Vec<String>> {
    let name = format!(
        "{COMMAND_ENV}{}",
        id.to_ascii_uppercase().replace(['-', '.'], "_")
    );
    let value = std::env::var(name).ok()?;
    let command: Vec<String> = value.split_whitespace().map(str::to_owned).collect();
    (!command.is_empty()).then_some(command)
}

/// `adapters` changed by the `adapters` list of the adapters file `file`: an entry with a known id changes the fields it gives, and one with a new id is added ahead of the known ones, so that it is preferred for its languages.
///
/// # Errors
///
/// Returns why when an entry is not an object, has no id, or a new one has no command or languages.
fn merged(mut adapters: Vec<Adapter>, file: &Value) -> Result<Vec<Adapter>, String> {
    let entries = file["adapters"]
        .as_array()
        .ok_or("it has no `adapters` list")?;
    let mut added = Vec::new();
    for entry in entries {
        let id = entry["id"].as_str().ok_or("an adapter has no `id`")?;
        let known = adapters.iter_mut().find(|adapter| adapter.id == id);
        let mut adapter = match known {
            Some(known) => known.clone(),
            None => {
                if entry.get("command").is_none() || entry.get("languages").is_none() {
                    return Err(format!(
                        "the new adapter {id} needs a `command` and `languages`"
                    ));
                }
                Adapter::new(id, &[], &[], &format!("the program of the {id} adapter"))
            }
        };
        if let Some(command) = entry.get("command") {
            adapter.commands = vec![strings(command, "command", id)?];
            adapter.module = None;
        }
        if let Some(languages) = entry.get("languages") {
            adapter.languages = strings(languages, "languages", id)?;
        }
        match entry["transport"].as_str() {
            None => {}
            Some("stdio") => adapter.transport = Transport::Stdio,
            Some("port") => adapter.transport = Transport::Port,
            Some(other) => return Err(format!("{id} has an unknown transport {other}")),
        }
        for (key, field) in [
            ("launch", &mut adapter.launch),
            ("binary", &mut adapter.binary),
            ("attach", &mut adapter.attach),
            ("remote", &mut adapter.remote),
        ] {
            if let Some(settings) = entry.get(key) {
                *field = settings
                    .as_object()
                    .cloned()
                    .ok_or_else(|| format!("{id}'s `{key}` is not an object"))?;
            }
        }
        if let Some(function) = entry["entry_function"].as_str() {
            adapter.entry = Entry::Function(function.to_owned());
        }
        if let Some(flag) = entry["entry_flag"].as_str() {
            adapter.entry = Entry::Flag(flag.to_owned());
        }
        match adapters.iter_mut().find(|known| known.id == id) {
            Some(known) => *known = adapter,
            None => added.push(adapter),
        }
    }
    added.extend(adapters);
    Ok(added)
}

/// The strings of the list `value`, the field `field` of the adapter `id`.
///
/// # Errors
///
/// Returns why when it is not a non-empty list of strings.
fn strings(value: &Value, field: &str, id: &str) -> Result<Vec<String>, String> {
    let list: Option<Vec<String>> = value
        .as_array()
        .map(|items| {
            items
                .iter()
                .map(|item| item.as_str().map(str::to_owned))
                .collect()
        })
        .and_then(|items: Option<Vec<String>>| items.filter(|items| !items.is_empty()));
    list.ok_or_else(|| format!("{id}'s `{field}` is not a list of strings"))
}

/// The language of the program at `program`, as the adapters name languages: from its extension, from the files of a folder, and `native` for anything else, taken to be a compiled program.
pub(crate) fn language_of(program: &Path) -> &'static str {
    if program.is_dir() {
        let has_go = program.join("go.mod").is_file()
            || std::fs::read_dir(program).is_ok_and(|entries| {
                entries
                    .flatten()
                    .any(|entry| entry.path().extension().is_some_and(|ext| ext == "go"))
            });
        if has_go {
            return "go";
        }
        if program.join("__main__.py").is_file() {
            return "python";
        }
        return "native";
    }
    match program
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("py" | "pyw") => "python",
        Some("go") => "go",
        Some("js" | "mjs" | "cjs" | "ts" | "mts" | "cts") => "javascript",
        _ => "native",
    }
}

/// The name a language given by the model is known by among the adapters, so that `C++`, `golang` and `py` are understood.
pub(crate) fn normalize_language(language: &str) -> String {
    let lower = language.trim().to_ascii_lowercase();
    match lower.as_str() {
        "c++" | "cxx" | "cc" => "cpp".to_owned(),
        "golang" => "go".to_owned(),
        "py" | "python3" => "python".to_owned(),
        "rs" => "rust".to_owned(),
        "js" | "node" | "nodejs" | "typescript" | "ts" => "javascript".to_owned(),
        "binary" | "compiled" | "executable" => "native".to_owned(),
        _ => lower,
    }
}

/// The path of the program `name`: taken relative to `root` when it holds a `/`, otherwise found on the `PATH` as [`omc_tools::find_program`] does, extensions and all on Windows.
fn find_program(name: &str, root: &Path) -> Option<PathBuf> {
    let direct = Path::new(name);
    if direct.components().count() > 1 {
        return omc_tools::find_program(&root.join(direct).to_string_lossy());
    }
    omc_tools::find_program(name)
}

/// Whether the Python interpreter `python` can import `module`.
async fn imports(python: &Path, module: &str) -> bool {
    let probe = tokio::process::Command::new(python)
        .arg("-c")
        .arg(format!("import {module}"))
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .status();
    matches!(
        tokio::time::timeout(PROBE_WAIT, probe).await,
        Ok(Ok(status)) if status.success()
    )
}

/// `value` as a JSON object; empty when it is not one.
fn object(value: Value) -> Map<String, Value> {
    match value {
        Value::Object(map) => map,
        _ => Map::new(),
    }
}

#[cfg(test)]
/// Tests for the adapters.
mod tests {
    use super::*;

    /// Programs are given a language from their extension, and anything else is taken for a compiled program.
    #[test]
    fn languages_come_from_the_program() {
        assert_eq!(language_of(Path::new("a/b.py")), "python");
        assert_eq!(language_of(Path::new("main.go")), "go");
        assert_eq!(language_of(Path::new("index.mjs")), "javascript");
        assert_eq!(language_of(Path::new("build/app")), "native");
        assert_eq!(normalize_language("C++"), "cpp");
        assert_eq!(normalize_language("golang"), "go");
    }

    /// Every language some adapter debugs has one adapter at least, and the compiled ones prefer `lldb-dap`.
    #[test]
    fn defaults_cover_the_usual_languages() {
        let adapters = default_adapters();
        for language in ["native", "c", "cpp", "rust", "go", "python"] {
            assert!(adapters.iter().any(|adapter| adapter.debugs(language)));
        }
        let first = adapters
            .iter()
            .find(|adapter| adapter.debugs("rust"))
            .unwrap();
        assert_eq!(first.id, "lldb-dap");
    }

    /// The adapters file changes a known adapter's command and adds a new adapter ahead of the known ones.
    #[test]
    fn the_file_changes_and_adds_adapters() {
        let file = json!({"adapters": [
            {"id": "gdb", "command": ["/opt/gdb/bin/gdb", "-i", "dap"]},
            {"id": "mine", "command": ["mine-dap"], "languages": ["zig"], "transport": "port", "launch": {"x": 1}},
        ]});
        let adapters = merged(default_adapters(), &file).unwrap();
        assert_eq!(adapters[0].id, "mine");
        assert_eq!(adapters[0].transport, Transport::Port);
        assert_eq!(adapters[0].launch["x"], 1);
        let gdb = adapters.iter().find(|adapter| adapter.id == "gdb").unwrap();
        assert_eq!(gdb.commands, vec![vec!["/opt/gdb/bin/gdb", "-i", "dap"]]);
        assert!(gdb.debugs("c"));
    }

    /// A new adapter without a command, or a wrong field, is refused with the reason.
    #[test]
    fn a_wrong_file_is_refused() {
        let missing = json!({"adapters": [{"id": "new", "languages": ["x"]}]});
        assert!(
            merged(default_adapters(), &missing)
                .unwrap_err()
                .contains("command")
        );
        let wrong = json!({"adapters": [{"id": "gdb", "languages": "c"}]});
        assert!(
            merged(default_adapters(), &wrong)
                .unwrap_err()
                .contains("languages")
        );
        assert!(merged(default_adapters(), &json!({})).is_err());
    }

    /// A program with a `/` is looked for in the project, and one that is not there is not found.
    #[test]
    fn programs_with_a_slash_are_in_the_project() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        assert_eq!(
            find_program("src/lib.rs", root),
            Some(root.join("src/lib.rs"))
        );
        assert_eq!(find_program(".venv/bin/python", root), None);
        assert_eq!(find_program("no-such-debugger-anywhere", root), None);
    }
}
