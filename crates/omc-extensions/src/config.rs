//! Where extensions are declared and how their files are read.
//!
//! Every file is a JSON object whose `extensions` object names each extension: a program to start (`command`, `args`, `env`, `cwd`), how long crab waits for its answer to an event (`timeout`, in seconds) and for a tool it gives the model (`tool_timeout`), and whether it is on (`disabled`). Text in the command, arguments and environment may name environment variables as `${NAME}`, or `${NAME:-fallback}`, as tool-server files do.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::Value;

/// The variable that names the user's own extensions file, instead of `extensions.json` in crab's config folder.
pub const EXTENSIONS_FILE_VARIABLE: &str = "OMC_EXTENSIONS_FILE";

/// The variable that, set to `0`, keeps crab from starting any extension.
pub const EXTENSIONS_VARIABLE: &str = "OMC_EXTENSIONS";

/// The file of a project extensions are read from, under each of its folders from the working folder up to the repository root.
pub const PROJECT_FILE: &str = ".oh-my-crab/extensions.json";

/// How long crab waits for an extension's answer to an event when its declaration gives no `timeout`.
const EVENT_TIMEOUT: Duration = Duration::from_secs(10);

/// How long crab waits for a tool an extension gives when its declaration gives no `tool_timeout`.
const TOOL_TIMEOUT: Duration = Duration::from_secs(600);

/// Where an extension is declared, which decides whether it waits for the project to be trusted.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Origin {
    /// The user's own file; always used.
    User,
    /// A file of the project; started only once the project is trusted.
    Project,
}

/// One extension to start.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Extension {
    /// The name it is known by.
    pub name: String,
    /// The program, with the environment variables in it replaced; a relative path with a folder in it is taken from the folder of the file that declares it.
    pub program: String,
    /// Its arguments, with the environment variables in them replaced.
    pub args: Vec<String>,
    /// The environment it is started with, over crab's own.
    pub env: Vec<(String, String)>,
    /// The folder it is started in, or the conversation's when there is none.
    pub folder: Option<PathBuf>,
    /// How long crab waits for its answer to an event.
    pub timeout: Duration,
    /// How long crab waits for a call of a tool it gives.
    pub tool_timeout: Duration,
    /// Where it is declared.
    pub origin: Origin,
    /// The file it is declared in.
    pub file: PathBuf,
    /// Its command line as the file wrote it, before variables were replaced.
    pub summary: String,
}

impl Extension {
    /// The extension as project trust lists it: `extension <name>: <command line>`.
    pub fn label(&self) -> String {
        format!("extension {}: {}", self.name, self.summary)
    }

    /// How trust records the extension: `Extension`, its name and its command line, apart by tabs.
    pub fn entry(&self) -> String {
        format!("Extension\t{}\t{}", self.name, self.summary)
    }
}

/// The extensions found in files, and what could not be read.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Found {
    /// The extensions, the first declared for each name.
    pub extensions: Vec<Extension>,
    /// One line for each file or entry that could not be read.
    pub problems: Vec<String>,
}

impl Found {
    /// Adds `other`'s extensions whose names are not taken yet, and its problems.
    fn absorb(&mut self, other: Found) {
        for extension in other.extensions {
            if !self
                .extensions
                .iter()
                .any(|kept| kept.name == extension.name)
            {
                self.extensions.push(extension);
            }
        }
        self.problems.extend(other.problems);
    }
}

/// Whether [`EXTENSIONS_VARIABLE`] lets crab start extensions.
pub fn enabled() -> bool {
    !std::env::var(EXTENSIONS_VARIABLE).is_ok_and(|value| {
        matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "0" | "false" | "off" | "no"
        )
    })
}

/// The user's own extensions file: the one [`EXTENSIONS_FILE_VARIABLE`] names, or `extensions.json` in crab's config folder; nothing when there is no config folder.
pub fn own_file() -> Option<PathBuf> {
    match std::env::var_os(EXTENSIONS_FILE_VARIABLE) {
        Some(path) if !path.is_empty() => Some(PathBuf::from(path)),
        _ => dirs::config_dir().map(|base| base.join("oh-my-crab").join("extensions.json")),
    }
}

/// The user's own extensions, from `own`; nothing when [`enabled`] says not to start any.
pub fn user_extensions(own: Option<&Path>) -> Found {
    match own {
        Some(own) if enabled() => read_file(own, Origin::User),
        _ => Found::default(),
    }
}

/// The project's extensions for work in `folders`, the folders from the working folder up to the repository root, each read from its [`PROJECT_FILE`], the nearest declaration of a name winning; nothing when [`enabled`] says not to start any. A project extension is started in the folder that holds `.oh-my-crab` unless it names a `cwd`.
pub fn project_extensions(folders: &[PathBuf]) -> Found {
    let mut found = Found::default();
    if !enabled() {
        return found;
    }
    for folder in folders {
        found.absorb(read_file(&folder.join(PROJECT_FILE), Origin::Project));
    }
    found
}

/// The extensions `file` declares, as from `origin`; a file that does not exist declares none, and one that cannot be read is reported.
fn read_file(file: &Path, origin: Origin) -> Found {
    let text = match std::fs::read_to_string(file) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Found::default(),
        Err(error) => {
            return Found {
                problems: vec![format!("{}: {error}", file.display())],
                ..Found::default()
            };
        }
    };
    match serde_json::from_str::<Value>(&text) {
        Ok(settings) => parse(&settings, file, origin),
        Err(error) => Found {
            problems: vec![format!("{}: {error}", file.display())],
            ..Found::default()
        },
    }
}

/// The extensions under `extensions` in `settings`, read from `file` as from `origin`; entries that cannot be read, and the file when it has no such object, are reported, and entries with `disabled` set are left out.
pub fn parse(settings: &Value, file: &Path, origin: Origin) -> Found {
    let mut found = Found::default();
    let Some(declared) = settings.get("extensions").and_then(Value::as_object) else {
        found
            .problems
            .push(format!("{}: no `extensions` object", file.display()));
        return found;
    };
    let base = file
        .parent()
        .map(|folder| match origin {
            Origin::Project => folder.parent().unwrap_or(folder),
            Origin::User => folder,
        })
        .unwrap_or(Path::new("."));
    for (name, entry) in declared {
        if entry.get("disabled") == Some(&Value::Bool(true)) {
            continue;
        }
        match extension(name, entry, file, origin, base) {
            Ok(extension) => found.extensions.push(extension),
            Err(reason) => found
                .problems
                .push(format!("{}: extension {name}: {reason}", file.display())),
        }
    }
    found
}

/// The extension `name` that `entry` of `file` declares, as from `origin`, with relative paths taken from `base`.
///
/// # Errors
///
/// Fails with the reason when the name is not one word, there is no command, or a field has the wrong type.
fn extension(
    name: &str,
    entry: &Value,
    file: &Path,
    origin: Origin,
    base: &Path,
) -> Result<Extension, String> {
    if name.is_empty()
        || !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err("the name may hold only letters, digits, `-` and `_`".to_owned());
    }
    let command = entry
        .get("command")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|command| !command.is_empty())
        .ok_or("no `command`")?;
    let args: Vec<String> = match entry.get("args") {
        None => Vec::new(),
        Some(Value::Array(args)) => args
            .iter()
            .map(|arg| arg.as_str().map(str::to_owned))
            .collect::<Option<_>>()
            .ok_or("`args` must be strings")?,
        Some(_) => return Err("`args` must be a list".to_owned()),
    };
    let env: Vec<(String, String)> = match entry.get("env") {
        None => Vec::new(),
        Some(Value::Object(env)) => env
            .iter()
            .map(|(key, value)| {
                value
                    .as_str()
                    .map(|value| (key.clone(), omc_mcp::config::expand(value)))
            })
            .collect::<Option<_>>()
            .ok_or("`env` values must be text")?,
        Some(_) => return Err("`env` must be an object".to_owned()),
    };
    let seconds = |key: &str, default: Duration| -> Result<Duration, String> {
        match entry.get(key) {
            None => Ok(default),
            Some(value) => value
                .as_f64()
                .filter(|seconds| seconds.is_finite() && *seconds > 0.0)
                .map(|seconds| Duration::from_secs_f64(seconds.min(86_400.0)))
                .ok_or_else(|| format!("`{key}` must be a positive number of seconds")),
        }
    };
    let program = located(&omc_mcp::config::expand(command), base);
    let folder = match entry.get("cwd").and_then(Value::as_str) {
        Some(cwd) => Some(base.join(omc_mcp::config::expand(cwd))),
        None if origin == Origin::Project => Some(base.to_path_buf()),
        None => None,
    };
    let mut summary = command.to_owned();
    for arg in &args {
        summary.push(' ');
        summary.push_str(arg);
    }
    Ok(Extension {
        name: name.to_owned(),
        program,
        args: args
            .iter()
            .map(|arg| omc_mcp::config::expand(arg))
            .collect(),
        env,
        folder,
        timeout: seconds("timeout", EVENT_TIMEOUT)?,
        tool_timeout: seconds("tool_timeout", TOOL_TIMEOUT)?,
        origin,
        file: file.to_path_buf(),
        summary,
    })
}

/// `program` with a relative path that names a folder, such as `./ext.py`, taken from `base`; a bare name is left for the search path.
fn located(program: &str, base: &Path) -> String {
    let path = Path::new(program);
    if path.is_relative() && path.components().count() > 1 {
        base.join(path).display().to_string()
    } else {
        program.to_owned()
    }
}

#[cfg(test)]
/// Tests for reading extension files.
mod tests {
    use serde_json::json;

    use super::*;

    /// An entry is read with its defaults, a relative program is taken from the file's folder, a disabled entry is left out, and a bad one is reported.
    #[test]
    fn reads_a_file() {
        let settings = json!({"extensions": {
            "guard": {"command": "./guard.py", "args": ["--strict"], "env": {"LEVEL": "2"}, "timeout": 2.5},
            "off": {"command": "x", "disabled": true},
            "bad name": {"command": "x"},
            "empty": {"args": []}
        }});
        let found = parse(
            &settings,
            Path::new("/p/.oh-my-crab/extensions.json"),
            Origin::Project,
        );
        assert_eq!(found.extensions.len(), 1);
        let guard = &found.extensions[0];
        assert_eq!(guard.program, "/p/./guard.py");
        assert_eq!(guard.args, vec!["--strict"]);
        assert_eq!(guard.env, vec![("LEVEL".to_owned(), "2".to_owned())]);
        assert_eq!(guard.folder.as_deref(), Some(Path::new("/p")));
        assert_eq!(guard.timeout, Duration::from_millis(2500));
        assert_eq!(guard.tool_timeout, TOOL_TIMEOUT);
        assert_eq!(guard.label(), "extension guard: ./guard.py --strict");
        assert_eq!(guard.entry(), "Extension\tguard\t./guard.py --strict");
        assert_eq!(found.problems.len(), 2);
        let user = parse(
            &json!({"extensions": {"g": {"command": "python3"}}}),
            Path::new("/home/u/.config/oh-my-crab/extensions.json"),
            Origin::User,
        );
        assert_eq!(user.extensions[0].program, "python3");
        assert_eq!(user.extensions[0].folder, None);
    }
}
