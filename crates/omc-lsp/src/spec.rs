//! Which language server reads which files, and how it is started.

use std::path::{Path, PathBuf};

/// A language server: the files it reads and the commands that may start it, the first one found on the `PATH` winning.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Spec {
    /// The name the server is known by; one server of each name runs.
    pub id: String,
    /// The file extensions it reads, each with the language id it is told the files are in.
    pub extensions: Vec<(String, String)>,
    /// The commands that start it, each a program and its arguments, tried in order.
    pub commands: Vec<Vec<String>>,
    /// The names of files whose presence at the root of a project says the project is written in its languages, so that it is worth starting before any file is asked about.
    pub markers: Vec<String>,
}

impl Spec {
    /// A server named `id` for the files with `extensions`, each paired with its language id, started by the first of `commands` found.
    pub fn new(id: &str, extensions: &[(&str, &str)], commands: &[&[&str]]) -> Self {
        Self {
            id: id.to_owned(),
            extensions: extensions
                .iter()
                .map(|(extension, language)| ((*extension).to_owned(), (*language).to_owned()))
                .collect(),
            commands: commands
                .iter()
                .map(|command| command.iter().map(|part| (*part).to_owned()).collect())
                .collect(),
            markers: Vec::new(),
        }
    }

    /// The same server, started early in a project that has one of the files `markers` names at its root.
    pub fn marked_by(mut self, markers: &[&str]) -> Self {
        self.markers = markers.iter().map(|marker| (*marker).to_owned()).collect();
        self
    }

    /// Whether the project at `root` has one of the files that say it is written in the server's languages.
    pub fn is_used_in(&self, root: &Path) -> bool {
        self.markers.iter().any(|marker| root.join(marker).exists())
    }

    /// The language id the server is told the file at `path` is in, when it reads such files.
    pub fn language_of(&self, path: &Path) -> Option<&str> {
        let extension = path.extension()?.to_str()?;
        self.extensions
            .iter()
            .find(|(known, _)| known.eq_ignore_ascii_case(extension))
            .map(|(_, language)| language.as_str())
    }

    /// Whether a program that starts the server is installed.
    pub fn is_installed(&self) -> bool {
        self.command().is_some()
    }

    /// The first of the commands whose program is found, as the program's path and the arguments.
    pub(crate) fn command(&self) -> Option<(PathBuf, Vec<String>)> {
        self.commands.iter().find_map(|command| {
            let (program, arguments) = command.split_first()?;
            Some((find_program(program)?, arguments.to_vec()))
        })
    }
}

/// The servers known without being told: the usual one for each common language.
pub fn default_specs() -> Vec<Spec> {
    vec![
        Spec::new("rust-analyzer", &[("rs", "rust")], &[&["rust-analyzer"]])
            .marked_by(&["Cargo.toml"]),
        Spec::new("gopls", &[("go", "go")], &[&["gopls"]]).marked_by(&["go.mod", "go.work"]),
        Spec::new(
            "typescript-language-server",
            &[
                ("ts", "typescript"),
                ("mts", "typescript"),
                ("cts", "typescript"),
                ("tsx", "typescriptreact"),
                ("js", "javascript"),
                ("mjs", "javascript"),
                ("cjs", "javascript"),
                ("jsx", "javascriptreact"),
            ],
            &[&["typescript-language-server", "--stdio"]],
        )
        .marked_by(&["package.json", "tsconfig.json", "jsconfig.json"]),
        Spec::new(
            "python",
            &[("py", "python"), ("pyi", "python")],
            &[
                &["pyright-langserver", "--stdio"],
                &["basedpyright-langserver", "--stdio"],
                &["pylsp"],
            ],
        )
        .marked_by(&[
            "pyproject.toml",
            "setup.py",
            "setup.cfg",
            "requirements.txt",
        ]),
        Spec::new(
            "clangd",
            &[
                ("c", "c"),
                ("h", "c"),
                ("cc", "cpp"),
                ("cpp", "cpp"),
                ("cxx", "cpp"),
                ("hh", "cpp"),
                ("hpp", "cpp"),
                ("hxx", "cpp"),
            ],
            &[&["clangd"]],
        )
        .marked_by(&[
            "compile_commands.json",
            "compile_flags.txt",
            "CMakeLists.txt",
            ".clangd",
        ]),
        Spec::new(
            "lua-language-server",
            &[("lua", "lua")],
            &[&["lua-language-server"]],
        )
        .marked_by(&[".luarc.json"]),
        Spec::new("zls", &[("zig", "zig")], &[&["zls"]]).marked_by(&["build.zig"]),
    ]
}

/// The path of the program `name`: `name` itself when it names a file, otherwise the first file of that name in a folder of the `PATH`.
fn find_program(name: &str) -> Option<PathBuf> {
    let direct = Path::new(name);
    if direct.components().count() > 1 {
        return direct.is_file().then(|| direct.to_path_buf());
    }
    let folders = std::env::var_os("PATH")?;
    std::env::split_paths(&folders)
        .map(|folder| folder.join(name))
        .find(|candidate| candidate.is_file())
}

#[cfg(test)]
/// Tests for choosing servers.
mod tests {
    use super::*;

    /// Files are matched to servers by extension, ignoring case.
    #[test]
    fn matches_files_by_extension() {
        let specs = default_specs();
        let typescript = specs
            .iter()
            .find(|spec| spec.id == "typescript-language-server")
            .unwrap();
        assert_eq!(
            typescript.language_of(Path::new("a/b.TSX")),
            Some("typescriptreact")
        );
        assert_eq!(typescript.language_of(Path::new("a/b.rs")), None);
        assert_eq!(typescript.language_of(Path::new("Makefile")), None);
    }

    /// A server none of whose programs exist has no command.
    #[test]
    fn a_missing_program_has_no_command() {
        let spec = Spec::new(
            "none",
            &[("x", "x")],
            &[&["no-such-language-server-anywhere"]],
        );
        assert_eq!(spec.command(), None);
    }
}
