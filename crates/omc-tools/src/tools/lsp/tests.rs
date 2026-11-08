//! Tests for the `lsp` tool, against a language service that answers as it is told.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use super::*;
use crate::testutil::scratch;
use crate::tools::snapshot::tag;
use crate::{Applied, CodeAction, Diagnostic, Location, Renamed, ServerStatus, Severity, Symbol};

/// A language service that answers navigation questions as it is told and records what it was asked.
#[derive(Default)]
struct Told {
    /// The places every lookup finds.
    locations: Vec<Location>,
    /// The files a rename changes.
    renamed: Vec<Renamed>,
    /// The symbols of any file.
    symbols: Vec<Symbol>,
    /// What is wrong with any file.
    diagnostics: Vec<Diagnostic>,
    /// The reason it gives for serving no file, when it serves none.
    no_server: Option<String>,
    /// The places it was asked about.
    asked: Mutex<Vec<Spot>>,
    /// The code actions it offers anywhere.
    actions: Vec<CodeAction>,
    /// The files applying any code action changes.
    applied: Vec<Renamed>,
    /// The files moving any file changes.
    moved: Vec<Renamed>,
    /// The whole formatted text it gives any file, when it formats.
    formatted: Option<String>,
    /// The stretches it was asked about, for code actions and formatting.
    ranges: Mutex<Vec<Location>>,
    /// The code actions it was asked to apply.
    chosen: Mutex<Vec<Chosen>>,
    /// The moves it was told were made.
    renamed_files: Mutex<Vec<(PathBuf, PathBuf)>>,
}

#[async_trait]
impl LanguageService for Told {
    /// Formats nothing.
    async fn format(&self, _path: &Path) -> Option<String> {
        None
    }

    /// The diagnostics it was told, about the files asked.
    async fn diagnostics(&self, paths: &[PathBuf], _wait: Duration) -> Vec<Diagnostic> {
        self.diagnostics
            .iter()
            .filter(|diagnostic| paths.contains(&diagnostic.path))
            .cloned()
            .collect()
    }

    /// Serves every file unless told to serve none.
    async fn serves(&self, _path: &Path) -> Result<(), LanguageError> {
        match &self.no_server {
            Some(reason) => Err(LanguageError::NoServer(reason.clone())),
            None => Ok(()),
        }
    }

    /// The places it was told, recording where it was asked.
    async fn locate(&self, _lookup: Lookup, at: &Spot) -> Result<Vec<Location>, LanguageError> {
        self.asked.lock().unwrap().push(at.clone());
        Ok(self.locations.clone())
    }

    /// The symbols it was told.
    async fn document_symbols(&self, _path: &Path) -> Result<Vec<Symbol>, LanguageError> {
        Ok(self.symbols.clone())
    }

    /// The files it was told, recording where it was asked.
    async fn rename(&self, at: &Spot, _new_name: &str) -> Result<Vec<Renamed>, LanguageError> {
        self.asked.lock().unwrap().push(at.clone());
        Ok(self.renamed.clone())
    }

    /// The diagnostics it was told.
    async fn all_diagnostics(&self, _wait: Duration) -> Result<Vec<Diagnostic>, LanguageError> {
        Ok(self.diagnostics.clone())
    }

    /// The formatted text it was told, recording the stretch asked.
    async fn reformat(
        &self,
        _path: &Path,
        range: Option<&Location>,
    ) -> Result<Option<String>, LanguageError> {
        self.ranges.lock().unwrap().extend(range.cloned());
        Ok(self.formatted.clone())
    }

    /// The actions it was told, recording the stretch asked.
    async fn code_actions(
        &self,
        range: &Location,
        _only: &[String],
    ) -> Result<Vec<CodeAction>, LanguageError> {
        self.ranges.lock().unwrap().push(range.clone());
        Ok(self.actions.clone())
    }

    /// The files it was told, under the title of the action chosen by number.
    async fn apply_code_action(
        &self,
        range: &Location,
        _only: &[String],
        chosen: &Chosen,
    ) -> Result<Applied, LanguageError> {
        self.ranges.lock().unwrap().push(range.clone());
        self.chosen.lock().unwrap().push(chosen.clone());
        let action = self
            .actions
            .get(chosen.index - 1)
            .ok_or_else(|| LanguageError::Failed("no such action".to_owned()))?;
        Ok(Applied {
            title: action.title.clone(),
            files: self.applied.clone(),
        })
    }

    /// The files it was told a move changes.
    async fn rename_file(&self, _from: &Path, _to: &Path) -> Result<Vec<Renamed>, LanguageError> {
        Ok(self.moved.clone())
    }

    /// Records the move.
    async fn file_renamed(&self, from: &Path, to: &Path) {
        self.renamed_files
            .lock()
            .unwrap()
            .push((from.to_path_buf(), to.to_path_buf()));
    }

    /// One running server.
    async fn status(&self) -> Result<Vec<ServerStatus>, LanguageError> {
        Ok(vec![ServerStatus {
            name: "rust-analyzer".to_owned(),
            languages: vec!["rust".to_owned()],
            state: "running".to_owned(),
            detail: None,
        }])
    }

    /// The server named, restarted.
    async fn reload(
        &self,
        server: Option<&str>,
        _near: Option<&Path>,
    ) -> Result<Vec<ServerStatus>, LanguageError> {
        Ok(vec![ServerStatus {
            name: server.unwrap_or("rust-analyzer").to_owned(),
            languages: vec!["rust".to_owned()],
            state: "loading".to_owned(),
            detail: Some("working on: Indexing".to_owned()),
        }])
    }
}

/// A scratch project named `name` with a library and a binary that calls into it.
fn project(name: &str) -> PathBuf {
    let dir = scratch(name);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(
        dir.join("src/lib.rs"),
        "pub fn helper() -> u32 {\n    1\n}\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("src/main.rs"),
        "fn main() {\n    let x = demo::helper();\n}\n",
    )
    .unwrap();
    dir
}

/// The tool in `dir`, asking `service`.
fn tool(dir: &Path, service: Told) -> Lsp {
    Lsp::new(
        Sandbox::new(dir).unwrap(),
        Languages::new(Some(Arc::new(service))),
    )
}

/// The place at `line` and `column` of `path`.
fn at(path: PathBuf, line: u32, column: u32) -> Location {
    Location {
        path,
        line,
        column,
        end_line: line,
        end_column: column + 6,
    }
}

/// With no language services the tool says so instead of failing obscurely.
#[tokio::test]
async fn says_when_there_are_no_language_services() {
    let dir = project("lsp-none");
    let tool = Lsp::new(Sandbox::new(&dir).unwrap(), Languages::default());
    let error = tool
        .call(json!({"action": "definition", "path": "src/main.rs", "line": 2, "symbol": "helper"}))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("No language server"), "{error}");
}

/// A service that only formats and reports diagnostics says it does not navigate, and one with no server for the file says why.
#[tokio::test]
async fn says_why_a_question_cannot_be_answered() {
    /// A service with only the required methods.
    struct Plain;

    #[async_trait]
    impl LanguageService for Plain {
        /// Formats nothing.
        async fn format(&self, _path: &Path) -> Option<String> {
            None
        }

        /// Finds nothing.
        async fn diagnostics(&self, _paths: &[PathBuf], _wait: Duration) -> Vec<Diagnostic> {
            Vec::new()
        }
    }

    let dir = project("lsp-plain");
    let plain = Lsp::new(
        Sandbox::new(&dir).unwrap(),
        Languages::new(Some(Arc::new(Plain))),
    );
    let error = plain
        .call(json!({"action": "references", "path": "src/lib.rs", "line": 1, "symbol": "helper"}))
        .await
        .unwrap_err();
    assert!(
        error.to_string().contains("do not navigate code"),
        "{error}"
    );
    let missing = tool(
        &dir,
        Told {
            no_server: Some("no language server reads .rs files".to_owned()),
            ..Told::default()
        },
    );
    let error = missing
        .call(json!({"action": "hover", "path": "src/lib.rs", "line": 1, "column": 8}))
        .await
        .unwrap_err();
    assert!(
        error
            .to_string()
            .starts_with("no language server reads .rs files. Use grep"),
        "{error}"
    );
}

/// A lookup is asked at the character the symbol starts at, and its places come back grouped by file with their lines.
#[tokio::test]
async fn references_are_found_from_the_symbol_on_the_line() {
    let dir = project("lsp-references");
    let (lib, main) = (dir.join("src/lib.rs"), dir.join("src/main.rs"));
    let service = Told {
        locations: vec![at(main.clone(), 2, 19), at(lib.clone(), 1, 8)],
        ..Told::default()
    };
    let tool = tool(&dir, service);
    let out = tool
        .call(json!({"action": "references", "path": "src/main.rs", "line": 2, "symbol": "demo::helper"}))
        .await
        .unwrap();
    assert_eq!(
        out.text,
        "2 reference(s) of `demo::helper` at src/main.rs:2:19 in 2 file(s):\nsrc/main.rs\n  2:19  let x = demo::helper();\nsrc/lib.rs\n  1:8  pub fn helper() -> u32 {"
    );
}

/// A rename writes every file it changes, reports their new tags and changes, and asks for approval; a dry run writes nothing and asks nothing.
#[tokio::test]
async fn a_rename_writes_every_file() {
    let dir = project("lsp-rename");
    let (lib, main) = (dir.join("src/lib.rs"), dir.join("src/main.rs"));
    let new_lib = "pub fn assist() -> u32 {\n    1\n}\n";
    let new_main = "fn main() {\n    let x = demo::assist();\n}\n";
    let service = Told {
        renamed: vec![
            Renamed {
                path: lib.clone(),
                text: new_lib.to_owned(),
                edits: 1,
            },
            Renamed {
                path: main.clone(),
                text: new_main.to_owned(),
                edits: 1,
            },
        ],
        ..Told::default()
    };
    let tool = tool(&dir, service);
    let args = json!({"action": "rename", "path": "src/lib.rs", "line": 1, "symbol": "helper", "new_name": "assist", "dry_run": true});
    assert_eq!(tool.approval_scope(&args), None);
    let preview = tool.call(args).await.unwrap();
    assert!(
        preview.text.contains("would make 2 edit(s) in 2 file(s)"),
        "{}",
        preview.text
    );
    assert!(
        preview
            .text
            .contains("  - pub fn helper() -> u32 {\n  + pub fn assist() -> u32 {")
    );
    assert!(std::fs::read_to_string(&lib).unwrap().contains("helper"));
    let args = json!({"action": "rename", "path": "src/lib.rs", "line": 1, "symbol": "helper", "new_name": "assist"});
    assert_eq!(tool.approval_scope(&args).as_deref(), Some("lsp rename"));
    let out = tool.call(args).await.unwrap();
    assert_eq!(std::fs::read_to_string(&lib).unwrap(), new_lib);
    assert_eq!(std::fs::read_to_string(&main).unwrap(), new_main);
    assert!(
        out.text
            .starts_with("Renamed `helper` at src/lib.rs:1:8 to `assist`: 2 edit(s) in 2 file(s)."),
        "{}",
        out.text
    );
    assert!(out.text.contains(&format!(
        "src/main.rs: 1 edit(s), tag {}",
        tag(new_main.as_bytes())
    )));
    assert_eq!(out.changes.len(), 2);
    assert_eq!(out.changes[1].path, "src/main.rs");
}

/// A rename that would reach outside the working directory changes nothing.
#[tokio::test]
async fn a_rename_outside_the_working_directory_is_refused() {
    let dir = project("lsp-rename-outside");
    let outside = scratch("lsp-rename-outside-other").join("other.rs");
    std::fs::write(&outside, "helper\n").unwrap();
    let service = Told {
        renamed: vec![
            Renamed {
                path: dir.join("src/lib.rs"),
                text: "pub fn assist() -> u32 {\n    1\n}\n".to_owned(),
                edits: 1,
            },
            Renamed {
                path: outside.clone(),
                text: "assist\n".to_owned(),
                edits: 1,
            },
        ],
        ..Told::default()
    };
    let error = tool(&dir, service)
        .call(json!({"action": "rename", "path": "src/lib.rs", "line": 1, "symbol": "helper", "new_name": "assist"}))
        .await
        .unwrap_err();
    assert!(
        error.to_string().contains("outside the working directory"),
        "{error}"
    );
    assert!(
        std::fs::read_to_string(dir.join("src/lib.rs"))
            .unwrap()
            .contains("helper")
    );
    assert_eq!(std::fs::read_to_string(&outside).unwrap(), "helper\n");
}

/// The copy that only looks is read-only, asks for nothing, and refuses a rename that would write but not a dry run.
#[tokio::test]
async fn the_looking_copy_refuses_to_write() {
    let dir = project("lsp-looking");
    let service = Told {
        renamed: vec![Renamed {
            path: dir.join("src/lib.rs"),
            text: "pub fn assist() -> u32 {\n    1\n}\n".to_owned(),
            edits: 1,
        }],
        ..Told::default()
    };
    let full = tool(&dir, service);
    assert!(!full.read_only());
    let looking = full.looking().unwrap();
    assert!(looking.read_only());
    assert!(looking.looking().is_none());
    let args = json!({"action": "rename", "path": "src/lib.rs", "line": 1, "symbol": "helper", "new_name": "assist"});
    assert_eq!(looking.approval_scope(&args), None);
    let error = looking.call(args).await.unwrap_err();
    assert!(error.to_string().contains("dry_run"), "{error}");
    let preview = looking
        .call(json!({"action": "rename", "path": "src/lib.rs", "line": 1, "symbol": "helper", "new_name": "assist", "dry_run": true}))
        .await
        .unwrap();
    assert!(preview.text.starts_with("Dry run"));
    assert!(
        std::fs::read_to_string(dir.join("src/lib.rs"))
            .unwrap()
            .contains("helper")
    );
}

/// Diagnostics of a file come from the service, and those of the project are kept to the working directory.
#[tokio::test]
async fn diagnostics_of_a_file_and_of_the_project() {
    let dir = project("lsp-diagnostics");
    let lib = dir.join("src/lib.rs");
    let diagnostic = |path: PathBuf, severity, message: &str| Diagnostic {
        path,
        line: 2,
        column: 5,
        severity,
        message: message.to_owned(),
        source: Some("rustc".to_owned()),
        code: None,
    };
    let service = Told {
        diagnostics: vec![
            diagnostic(lib.clone(), Severity::Warning, "unused"),
            diagnostic(lib.clone(), Severity::Error, "mismatched types"),
            diagnostic(
                PathBuf::from("/elsewhere/x.rs"),
                Severity::Error,
                "far away",
            ),
        ],
        ..Told::default()
    };
    let tool = tool(&dir, service);
    let file = tool
        .call(json!({"action": "diagnostics", "path": "src/lib.rs"}))
        .await
        .unwrap();
    assert_eq!(
        file.text,
        "Diagnostics in src/lib.rs: 1 error(s), 1 warning(s), 0 other\nsrc/lib.rs:2:5 error: mismatched types (rustc)\nsrc/lib.rs:2:5 warning: unused (rustc)"
    );
    let all = tool.call(json!({"action": "diagnostics"})).await.unwrap();
    assert!(
        all.text
            .starts_with("Diagnostics in the project: 1 error(s), 1 warning(s)")
    );
    assert!(!all.text.contains("far away"));
}

/// Asking for symbols with neither a file nor a query is turned away, and a file's outline is shown.
#[tokio::test]
async fn symbols_need_a_file_or_a_query() {
    let dir = project("lsp-symbols");
    let service = Told {
        symbols: vec![Symbol {
            name: "helper".to_owned(),
            kind: "function".to_owned(),
            detail: Some("fn() -> u32".to_owned()),
            container: None,
            depth: 0,
            location: at(dir.join("src/lib.rs"), 1, 8),
        }],
        ..Told::default()
    };
    let tool = tool(&dir, service);
    let error = tool.call(json!({"action": "symbols"})).await.unwrap_err();
    assert!(matches!(error, ToolError::InvalidArguments(_)));
    let out = tool
        .call(json!({"action": "symbols", "path": "src/lib.rs"}))
        .await
        .unwrap();
    assert_eq!(
        out.text,
        "1 symbol(s) in src/lib.rs:\n    1  function helper  fn() -> u32"
    );
}

/// Code actions are listed numbered for the symbol's stretch, and applying one by number writes what it changes after asking under `lsp code_actions`, while a listing or a dry run asks nothing and writes nothing.
#[tokio::test]
async fn code_actions_are_listed_and_applied() {
    let dir = project("lsp-code-actions");
    let main = dir.join("src/main.rs");
    let fixed = "fn main() {\n    let _x = demo::helper();\n}\n";
    let service = Told {
        actions: vec![
            CodeAction {
                title: "Rename to `_x`".to_owned(),
                kind: Some("quickfix".to_owned()),
                preferred: true,
                disabled: None,
            },
            CodeAction {
                title: "Inline variable".to_owned(),
                kind: Some("refactor.inline".to_owned()),
                preferred: false,
                disabled: Some("not here".to_owned()),
            },
        ],
        applied: vec![Renamed {
            path: main.clone(),
            text: fixed.to_owned(),
            edits: 1,
        }],
        ..Told::default()
    };
    let tool = tool(&dir, service);
    let list = json!({"action": "code_actions", "path": "src/main.rs", "line": 2, "symbol": "x"});
    assert_eq!(tool.approval_scope(&list), None);
    let listed = tool.call(list).await.unwrap();
    assert!(
        listed.text.starts_with(
            "2 code action(s) at `x` at src/main.rs:2:9:\n  1. Rename to `_x`  [quickfix]  (preferred)\n  2. Inline variable  [refactor.inline]  (disabled: not here)"
        ),
        "{}",
        listed.text
    );
    let apply = json!({"action": "code_actions", "path": "src/main.rs", "line": 2, "symbol": "x", "apply": 1, "title": "Rename to `_x`"});
    assert_eq!(
        tool.approval_scope(&apply).as_deref(),
        Some("lsp code_actions")
    );
    let mut dry = apply.clone();
    dry["dry_run"] = json!(true);
    assert_eq!(tool.approval_scope(&dry), None);
    assert!(tool.call(dry).await.unwrap().text.starts_with("Dry run"));
    assert!(std::fs::read_to_string(&main).unwrap().contains("let x"));
    let out = tool.call(apply).await.unwrap();
    assert_eq!(std::fs::read_to_string(&main).unwrap(), fixed);
    assert!(
        out.text.starts_with(
            "Applied the code action \"Rename to `_x`\" at `x` at src/main.rs:2:9: 1 edit(s) in 1 file(s)."
        ),
        "{}",
        out.text
    );
    assert!(out.text.contains(&format!("tag {}", tag(fixed.as_bytes()))));
    let error = tool
        .call(json!({"action": "code_actions", "path": "src/main.rs", "line": 2, "apply": 0}))
        .await
        .unwrap_err();
    assert!(matches!(error, ToolError::InvalidArguments(_)));
}

/// A stretch is the symbol's name, a whole line, or lines from line to end_line, and the services are asked about exactly that.
#[tokio::test]
async fn code_actions_are_asked_for_the_stretch_named() {
    let dir = project("lsp-code-action-ranges");
    let service = Arc::new(Told::default());
    let tool = Lsp::new(
        Sandbox::new(&dir).unwrap(),
        Languages::new(Some(service.clone())),
    );
    for args in [
        json!({"action": "code_actions", "path": "src/main.rs", "line": 2, "symbol": "demo::helper"}),
        json!({"action": "code_actions", "path": "src/main.rs", "line": 2}),
        json!({"action": "code_actions", "path": "src/main.rs", "line": 1, "end_line": 3}),
        json!({"action": "code_actions", "path": "src/main.rs", "line": 2, "column": 5, "end_line": 2, "end_column": 10}),
    ] {
        let out = tool.call(args).await.unwrap();
        assert!(
            out.text.starts_with("No code actions offered at"),
            "{}",
            out.text
        );
    }
    let asked: Vec<(u32, u32, u32, u32)> = service
        .ranges
        .lock()
        .unwrap()
        .iter()
        .map(|range| (range.line, range.column, range.end_line, range.end_column))
        .collect();
    assert_eq!(
        asked,
        [(2, 19, 2, 25), (2, 1, 2, 28), (1, 1, 3, 2), (2, 5, 2, 10)]
    );
    let error = tool
        .call(json!({"action": "code_actions", "path": "src/main.rs", "line": 3, "end_line": 1}))
        .await
        .unwrap_err();
    assert!(matches!(error, ToolError::InvalidArguments(_)), "{error}");
}

/// Formatting writes the file the service formatted after asking under `lsp format`, a range is passed on as whole lines, and a file formatted already is left alone.
#[tokio::test]
async fn format_writes_the_formatted_file() {
    let dir = project("lsp-format");
    let lib = dir.join("src/lib.rs");
    let formatted = "pub fn helper() -> u32 {\n    1\n}\n\npub fn more() {}\n";
    let service = Arc::new(Told {
        formatted: Some(formatted.to_owned()),
        ..Told::default()
    });
    let tool = Lsp::new(
        Sandbox::new(&dir).unwrap(),
        Languages::new(Some(service.clone())),
    );
    let args = json!({"action": "format", "path": "src/lib.rs"});
    assert_eq!(tool.approval_scope(&args).as_deref(), Some("lsp format"));
    let dry = tool
        .call(json!({"action": "format", "path": "src/lib.rs", "line": 1, "end_line": 2, "dry_run": true}))
        .await
        .unwrap();
    assert!(
        dry.text
            .starts_with("Dry run: formatting src/lib.rs:1:1-2:6 would change 2 line(s)"),
        "{}",
        dry.text
    );
    assert!(dry.text.contains("  + pub fn more() {}"), "{}", dry.text);
    let range = service.ranges.lock().unwrap()[0].clone();
    assert_eq!(
        (range.line, range.column, range.end_line, range.end_column),
        (1, 1, 2, 6)
    );
    let out = tool.call(args.clone()).await.unwrap();
    assert!(
        out.text
            .starts_with("Formatted src/lib.rs: 2 line(s) changed."),
        "{}",
        out.text
    );
    assert_eq!(std::fs::read_to_string(&lib).unwrap(), formatted);
    let again = tool.call(args).await.unwrap();
    assert_eq!(
        again.text,
        "src/lib.rs is formatted already; nothing was changed."
    );
}

/// Moving a file writes it at its new path with the edits the services ask for, those to the moved file among them, removes it from the old one, tells the services, and asks first under `lsp rename_file`; a taken path is refused.
#[tokio::test]
async fn rename_file_moves_the_file_and_updates_what_names_it() {
    let dir = project("lsp-rename-file");
    std::fs::write(dir.join("src/util.rs"), "pub fn one() -> u32 {\n    1\n}\n").unwrap();
    std::fs::write(dir.join("src/lib.rs"), "pub mod util;\n").unwrap();
    let service = Arc::new(Told {
        moved: vec![
            Renamed {
                path: dir.join("src/lib.rs"),
                text: "pub mod helpers;\n".to_owned(),
                edits: 1,
            },
            Renamed {
                path: dir.join("src/util.rs"),
                text: "pub fn one() -> u32 {\n    1 // moved\n}\n".to_owned(),
                edits: 1,
            },
        ],
        ..Told::default()
    });
    let tool = Lsp::new(
        Sandbox::new(&dir).unwrap(),
        Languages::new(Some(service.clone())),
    );
    let args =
        json!({"action": "rename_file", "path": "src/util.rs", "new_path": "src/helpers.rs"});
    assert_eq!(
        tool.approval_scope(&args).as_deref(),
        Some("lsp rename_file")
    );
    let taken = tool
        .call(json!({"action": "rename_file", "path": "src/util.rs", "new_path": "src/main.rs"}))
        .await
        .unwrap_err();
    assert!(taken.to_string().contains("exists already"), "{taken}");
    let out = tool.call(args).await.unwrap();
    assert!(
        out.text
            .starts_with("Moved src/util.rs to src/helpers.rs, with 2 edit(s) in 1 other file(s)."),
        "{}",
        out.text
    );
    assert!(!dir.join("src/util.rs").exists());
    assert_eq!(
        std::fs::read_to_string(dir.join("src/helpers.rs")).unwrap(),
        "pub fn one() -> u32 {\n    1 // moved\n}\n"
    );
    assert_eq!(
        std::fs::read_to_string(dir.join("src/lib.rs")).unwrap(),
        "pub mod helpers;\n"
    );
    assert_eq!(
        service.renamed_files.lock().unwrap().as_slice(),
        [(dir.join("src/util.rs"), dir.join("src/helpers.rs"))]
    );
    assert_eq!(out.changes.len(), 3);
}

/// Status and reload say how the servers stand, and need no approval.
#[tokio::test]
async fn status_and_reload_say_how_the_servers_stand() {
    let dir = project("lsp-status");
    let tool = tool(&dir, Told::default());
    let args = json!({"action": "status"});
    assert_eq!(tool.approval_scope(&args), None);
    assert_eq!(
        tool.call(args).await.unwrap().text,
        "Language servers:\nrust-analyzer: running (rust)"
    );
    let args = json!({"action": "reload", "server": "rust-analyzer"});
    assert_eq!(tool.approval_scope(&args), None);
    assert_eq!(
        tool.call(args).await.unwrap().text,
        "Restarted:\nrust-analyzer: loading (rust); working on: Indexing"
    );
}

/// The copy that only looks refuses to apply a code action, format or move a file, but lists actions and shows dry runs.
#[tokio::test]
async fn the_looking_copy_refuses_the_new_writes() {
    let dir = project("lsp-looking-writes");
    let looking = tool(
        &dir,
        Told {
            formatted: Some("x\n".to_owned()),
            ..Told::default()
        },
    )
    .looking()
    .unwrap();
    for args in [
        json!({"action": "code_actions", "path": "src/lib.rs", "line": 1, "apply": 1}),
        json!({"action": "format", "path": "src/lib.rs"}),
        json!({"action": "rename_file", "path": "src/lib.rs", "new_path": "src/other.rs"}),
    ] {
        assert_eq!(looking.approval_scope(&args), None);
        let error = looking.call(args).await.unwrap_err();
        assert!(error.to_string().contains("dry_run"), "{error}");
    }
    assert!(
        looking
            .call(json!({"action": "code_actions", "path": "src/lib.rs", "line": 1}))
            .await
            .is_ok()
    );
    assert!(
        looking
            .call(json!({"action": "format", "path": "src/lib.rs", "dry_run": true}))
            .await
            .unwrap()
            .text
            .starts_with("Dry run")
    );
    assert!(dir.join("src/lib.rs").exists());
}
