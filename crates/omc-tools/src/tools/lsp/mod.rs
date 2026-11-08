//! The `lsp` tool: asks the conversation's language services where code is defined and used, what a symbol is, which symbols a file or the project has and what is wrong with them, renames a symbol across files, applies a server's code actions, formats files, moves a file with the imports that name it, and says which servers run and restarts them.

mod changes;
mod position;
mod render;

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::language::{GRACE, Languages};
use crate::tool::parse_args;
use crate::{
    Chosen, Concurrency, LanguageError, LanguageService, Location, Lookup, Sandbox, Spot, Tool,
    ToolError, ToolOutput,
};
use changes::{Moved, Pending, commit, pending, preview};
use render::Lines;

/// What the model is told when the conversation has no language services at all.
const NO_SERVICES: &str = "No language server is available in this conversation, so `lsp` cannot answer. Use grep, ast_grep or read instead.";

/// The actions that change files, and so ask the user first unless they are dry runs.
const WRITING: [&str; 4] = ["rename", "code_actions", "format", "rename_file"];

/// What the tool tells the model it does.
const DESCRIPTION: &str = "Ask the language server of a file about code: where a symbol is defined or used, what it is, which symbols a file or the project has, what is wrong with the code; rename a symbol everywhere, apply the server's fixes and refactorings, format a file, move a file and update its imports, or see and restart the servers.\n\
Actions:\n\
- definition, type_definition, implementation, references, hover: need path and symbol (the name as written in the file), best with the line it is on (use occurrence when it appears more than once on that line); or line and column instead of symbol. Without a line, the symbol's first whole-word occurrence in the file is used. A qualified symbol such as Type::method points at its last part. A symbol a few lines off is still found, with a note.\n\
- symbols: with path, the outline of that file; with query, a search of the whole project for symbols whose names match (path then picks which language's server to ask).\n\
- diagnostics: with path, the errors and warnings of that file (or of every file under a folder); without, of the whole project as the servers know it.\n\
- rename: renames the symbol at path, line and symbol (as for references) to new_name in every file that uses it, then formats the changed files and reports their diagnostics and new snapshot tags.\n\
- code_actions: lists the quick fixes and refactorings the server offers at path and line: the symbol on it (or line and column), the whole line, or lines line to end_line (end_column optional). kind narrows them, such as quickfix, refactor or source.organizeImports. Add apply with the number of one from the list to apply it to the files.\n\
- format: formats the file at path as its language says, or only lines line to end_line.\n\
- rename_file: moves the file at path to new_path and updates the imports and module declarations that name it.\n\
- status: which language servers run for which languages and how each stands. reload: restarts the server named server, or the one of path's language, or all of them; use it when a server is stuck or answers for stale code.\n\
rename, applying a code action, format and rename_file write files: the user is asked to approve them, and dry_run: true shows the changes without writing.\n\
Results are compact: file:line:column with the line quoted. Prefer this over grep to find the real uses of a symbol, and over editing each file by hand to rename one or fix what the server can fix. When no server handles the file type, the call says so; fall back to grep, ast_grep or ast_edit.";

/// Asks language servers about code, and renames symbols with them.
pub struct Lsp {
    /// The directory paths are confined to.
    sandbox: Sandbox,
    /// The language services asked.
    languages: Languages,
    /// Whether this copy only looks, refusing renames that would write, for plan mode and read-only agents.
    looking: bool,
}

impl Lsp {
    /// Creates the tool confined to `sandbox`, asking `languages`, which formats the files a rename changes and says what is wrong with them as well.
    pub fn new(sandbox: Sandbox, languages: Languages) -> Self {
        Self {
            sandbox,
            languages,
            looking: false,
        }
    }
}

/// The action of an `lsp` call.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
enum Action {
    /// Where the symbol is defined.
    Definition,
    /// Where the symbol's type is defined.
    TypeDefinition,
    /// Where the symbol is implemented.
    Implementation,
    /// Where the symbol is used.
    References,
    /// What the symbol is.
    Hover,
    /// The symbols of a file, or of the project matching a query.
    Symbols,
    /// What is wrong with a file, a folder or the project.
    Diagnostics,
    /// Renames the symbol everywhere.
    Rename,
    /// Lists the code actions offered for a stretch of a file, or applies one.
    CodeActions,
    /// Formats a file or some of its lines.
    Format,
    /// Moves a file and updates what names it.
    RenameFile,
    /// Says which servers run and how they stand.
    Status,
    /// Restarts servers.
    Reload,
}

impl Action {
    /// The name the action is called by.
    fn name(self) -> &'static str {
        match self {
            Action::Definition => "definition",
            Action::TypeDefinition => "type_definition",
            Action::Implementation => "implementation",
            Action::References => "references",
            Action::Hover => "hover",
            Action::Symbols => "symbols",
            Action::Diagnostics => "diagnostics",
            Action::Rename => "rename",
            Action::CodeActions => "code_actions",
            Action::Format => "format",
            Action::RenameFile => "rename_file",
            Action::Status => "status",
            Action::Reload => "reload",
        }
    }

    /// The lookup the action asks for, when it is one of those.
    fn lookup(self) -> Option<Lookup> {
        match self {
            Action::Definition => Some(Lookup::Definition),
            Action::TypeDefinition => Some(Lookup::TypeDefinition),
            Action::Implementation => Some(Lookup::Implementation),
            Action::References => Some(Lookup::References),
            _ => None,
        }
    }
}

/// Whether the call with `args` writes files: a rename, an applied code action, a format or a file move that is not a dry run.
fn writes(args: &Value) -> bool {
    let action = args["action"].as_str().unwrap_or_default();
    let applies = action != "code_actions" || !args["apply"].is_null();
    WRITING.contains(&action) && applies && args["dry_run"].as_bool() != Some(true)
}

/// Arguments of an `lsp` call.
#[derive(Clone, Deserialize)]
struct Args {
    /// What to ask.
    action: Action,
    /// The file, or for diagnostics a folder, relative to the working directory.
    path: Option<String>,
    /// The line the symbol is on, counted from one.
    line: Option<u32>,
    /// The symbol's name as written on the line.
    symbol: Option<String>,
    /// Which occurrence of the symbol on the line, counted from one.
    occurrence: Option<u32>,
    /// The character the symbol is at, counted from one, instead of naming it.
    column: Option<u32>,
    /// What to search the project's symbols for.
    query: Option<String>,
    /// The name to rename the symbol to.
    new_name: Option<String>,
    /// Whether a change only shows what it would do.
    #[serde(default)]
    dry_run: bool,
    /// The last line of a stretch, counted from one.
    end_line: Option<u32>,
    /// The character of the last line a stretch ends before, counted from one.
    end_column: Option<u32>,
    /// The kinds of code action wanted, such as `quickfix`.
    kind: Option<String>,
    /// The number, counted from one, of the code action to apply among those listed.
    apply: Option<usize>,
    /// The title of the code action to apply, which wins over its number when the list has changed.
    title: Option<String>,
    /// Where a file is moved to.
    new_path: Option<String>,
    /// The name of the server to restart.
    server: Option<String>,
}

/// A place in a file the model pointed at, resolved.
struct Pointed {
    /// The place, as the language service is asked about it.
    spot: Spot,
    /// How the place is named to the model, such as `` `helper` at src/lib.rs:3:8 ``.
    what: String,
    /// What to tell the model about where its symbol was found, when not where it said.
    note: Option<String>,
    /// The line and character the place ends before, both counted from one, when it is a stretch rather than a point.
    end: Option<(u32, u32)>,
}

impl Pointed {
    /// The stretch the place covers, as the language service is asked about it: from its spot to its end, or empty at its spot.
    fn location(&self) -> Location {
        let (end_line, end_column) = self.end.unwrap_or((self.spot.line, self.spot.column));
        Location {
            path: self.spot.path.clone(),
            line: self.spot.line,
            column: self.spot.column,
            end_line,
            end_column,
        }
    }
}

/// `text` preceded by `note` on a line of its own, when there is a note.
fn noted(note: Option<&str>, text: String) -> String {
    match note {
        Some(note) => format!("[{note}]\n{text}"),
        None => text,
    }
}

/// The model's message for `error`, a language service's failure.
fn failed(error: LanguageError) -> ToolError {
    match error {
        LanguageError::NoServer(reason) => ToolError::Failed(format!(
            "{reason}. Use grep, ast_grep or read for this file instead."
        )),
        LanguageError::Unsupported(reason) => {
            ToolError::Failed(format!("{reason}. Use grep, ast_grep or read instead."))
        }
        LanguageError::Failed(reason) => {
            ToolError::Failed(format!("the language server failed: {reason}"))
        }
    }
}

/// The value of the argument `name`, or a message saying `action` needs it.
fn needed<T>(value: Option<T>, name: &str, action: &str) -> Result<T, ToolError> {
    value.ok_or_else(|| ToolError::InvalidArguments(format!("{action} needs {name}")))
}

impl Lsp {
    /// The language service to ask, or the message saying there is none.
    fn service(&self) -> Result<Arc<dyn LanguageService>, ToolError> {
        self.languages
            .get()
            .ok_or_else(|| ToolError::Failed(NO_SERVICES.to_owned()))
    }

    /// The file `shown` resolved inside the sandbox, checked to be a file.
    ///
    /// # Errors
    ///
    /// Returns [`ToolError::OutsidePath`] for a path outside the working directory and [`ToolError::Failed`] when it is not a file.
    fn file(&self, shown: &str) -> Result<PathBuf, ToolError> {
        let path = self.sandbox.resolve(shown)?;
        if !path.is_file() {
            return Err(ToolError::Failed(format!("{shown} is not a file")));
        }
        Ok(path)
    }

    /// The place `args` points at for `action`: its file, its line, and its column, given or found from the symbol named on the line.
    ///
    /// With a symbol and no line, the symbol's first whole-word occurrence in the file is used.
    ///
    /// # Errors
    ///
    /// Returns [`ToolError::InvalidArguments`] when the path is missing, or both the symbol and a line and column, and [`ToolError::Failed`] when the file cannot be read or the symbol is not found.
    async fn pointed(&self, args: &Args, action: &str) -> Result<Pointed, ToolError> {
        let shown = needed(args.path.as_deref(), "path", action)?;
        let path = self.file(shown)?;
        let shown = self.sandbox.display(&path);
        if let (Some(column), Some(line), None) = (args.column, args.line, &args.symbol) {
            return Ok(Pointed {
                spot: Spot {
                    path,
                    line,
                    column: column.max(1),
                },
                what: format!("{shown}:{line}:{column}"),
                note: None,
                end: None,
            });
        }
        let symbol = args.symbol.as_deref().ok_or_else(|| {
            ToolError::InvalidArguments(format!(
                "{action} needs symbol (the name as written on the line), or line and column"
            ))
        })?;
        let text = tokio::fs::read_to_string(&path)
            .await
            .map_err(|error| ToolError::Failed(format!("{shown} cannot be read: {error}")))?;
        let found = match args.line {
            Some(line) => position::find(&text, line, symbol, args.occurrence),
            None => position::find_anywhere(&text, symbol),
        }
        .map_err(|reason| ToolError::Failed(format!("{shown}: {reason}")))?;
        Ok(Pointed {
            what: format!(
                "`{}` at {shown}:{}:{}",
                symbol.trim(),
                found.line,
                found.column
            ),
            spot: Spot {
                path,
                line: found.line,
                column: found.column,
            },
            note: found.note,
            end: None,
        })
    }

    /// Runs the lookup `lookup` from the place `args` points at.
    ///
    /// # Errors
    ///
    /// As [`Lsp::pointed`], and the service's failure.
    async fn lookup(
        &self,
        service: &dyn LanguageService,
        lookup: Lookup,
        args: &Args,
        action: &str,
    ) -> Result<String, ToolError> {
        let pointed = self.pointed(args, action).await?;
        service.serves(&pointed.spot.path).await.map_err(failed)?;
        let found = service
            .locate(lookup, &pointed.spot)
            .await
            .map_err(failed)?;
        let text = render::locations(
            &self.sandbox,
            lookup.noun(),
            &pointed.what,
            &found,
            &mut Lines::default(),
        );
        Ok(noted(pointed.note.as_deref(), text))
    }

    /// Says what the symbol `args` points at is.
    ///
    /// # Errors
    ///
    /// As [`Lsp::lookup`].
    async fn hover(&self, service: &dyn LanguageService, args: &Args) -> Result<String, ToolError> {
        let pointed = self.pointed(args, "hover").await?;
        service.serves(&pointed.spot.path).await.map_err(failed)?;
        let said = service.hover(&pointed.spot).await.map_err(failed)?;
        Ok(noted(
            pointed.note.as_deref(),
            render::hover(&pointed.what, said.as_deref()),
        ))
    }

    /// Lists the symbols of the file `args` names, or those of the project matching its query.
    ///
    /// # Errors
    ///
    /// Returns [`ToolError::InvalidArguments`] when there is neither a path nor a query, and the service's failure.
    async fn symbols(
        &self,
        service: &dyn LanguageService,
        args: &Args,
    ) -> Result<String, ToolError> {
        let near = args
            .path
            .as_deref()
            .map(|shown| self.file(shown))
            .transpose()?;
        if let Some(query) = args
            .query
            .as_deref()
            .filter(|query| !query.trim().is_empty())
        {
            if let Some(near) = &near {
                service.serves(near).await.map_err(failed)?;
            }
            let found = service
                .workspace_symbols(query.trim(), near.as_deref())
                .await
                .map_err(failed)?;
            return Ok(render::workspace_symbols(
                &self.sandbox,
                query.trim(),
                &found,
            ));
        }
        let Some(path) = near else {
            return Err(ToolError::InvalidArguments(
                "symbols needs path (the outline of a file) or query (a search of the project)"
                    .to_owned(),
            ));
        };
        service.serves(&path).await.map_err(failed)?;
        let found = service.document_symbols(&path).await.map_err(failed)?;
        Ok(render::document_symbols(
            &self.sandbox.display(&path),
            &found,
        ))
    }

    /// Says what is wrong with the file or folder `args` names, or with the whole project.
    ///
    /// # Errors
    ///
    /// Returns the service's failure, and [`ToolError::OutsidePath`] for a path outside the working directory.
    async fn diagnostics(
        &self,
        service: &dyn LanguageService,
        args: &Args,
    ) -> Result<String, ToolError> {
        let scope = match args.path.as_deref() {
            Some(shown) => Some(self.sandbox.resolve(shown)?),
            None => None,
        };
        if let Some(file) = scope.as_ref().filter(|scope| scope.is_file()) {
            service.serves(file).await.map_err(failed)?;
            let paths = vec![file.clone()];
            let wait = service.patience(&paths);
            let found = tokio::time::timeout(wait + GRACE, service.diagnostics(&paths, wait))
                .await
                .unwrap_or_default();
            return Ok(render::diagnostics(
                &self.sandbox,
                &self.sandbox.display(file),
                found,
            ));
        }
        let wait = crate::DIAGNOSTICS_WAIT;
        let mut found = tokio::time::timeout(
            crate::DIAGNOSTICS_WAIT * 4 + GRACE,
            service.all_diagnostics(wait),
        )
        .await
        .map_err(|_| ToolError::Failed("the language servers did not answer in time".to_owned()))?
        .map_err(failed)?;
        let shown = match &scope {
            Some(folder) => {
                found.retain(|diagnostic| diagnostic.path.starts_with(folder));
                format!("{}/", self.sandbox.display(folder))
            }
            None => {
                let root = self.sandbox.root();
                found.retain(|diagnostic| diagnostic.path.starts_with(root));
                "the project".to_owned()
            }
        };
        Ok(render::diagnostics(&self.sandbox, &shown, found))
    }

    /// Refuses a call that would write files when this copy only looks, saying how `action` can still be seen with a dry run.
    ///
    /// # Errors
    ///
    /// Returns [`ToolError::Failed`] when this copy only looks and the call is not a dry run.
    fn may_write(&self, args: &Args, action: &str) -> Result<(), ToolError> {
        if self.looking && !args.dry_run {
            return Err(ToolError::Failed(format!(
                "{action} changes files, which is not allowed now (plan mode or a read-only agent). Set dry_run: true to see what it would change."
            )));
        }
        Ok(())
    }

    /// Renames the symbol `args` points at to its new name in every file that uses it, or, for a dry run, shows what that would change.
    ///
    /// # Errors
    ///
    /// Returns [`ToolError::InvalidArguments`] for a missing or blank new name, [`ToolError::Failed`] when this copy only looks and the rename is not a dry run, when the rename reaches files outside the working directory, or changes nothing, and [`ToolError::Io`] when a file cannot be written, after putting back those already written.
    async fn rename(
        &self,
        service: &dyn LanguageService,
        args: &Args,
    ) -> Result<ToolOutput, ToolError> {
        self.may_write(args, "rename")?;
        let new_name = needed(
            args.new_name
                .as_deref()
                .map(str::trim)
                .filter(|name| !name.is_empty()),
            "new_name",
            "rename",
        )?;
        let pointed = self.pointed(args, "rename").await?;
        service.serves(&pointed.spot.path).await.map_err(failed)?;
        let renamed = service
            .rename(&pointed.spot, new_name)
            .await
            .map_err(failed)?;
        let what = format!("renaming {}", pointed.what);
        let pending = pending(&self.sandbox, renamed, &what).await?;
        if pending.is_empty() {
            return Err(ToolError::Failed(format!(
                "the language server found nothing to change in {what}"
            )));
        }
        let edits: usize = pending.iter().map(|file| file.edits).sum();
        let note = pointed.note.as_deref();
        if args.dry_run {
            let text = format!(
                "Dry run: renaming {} to `{new_name}` would make {edits} edit(s) in {} file(s); nothing was written.{}",
                pointed.what,
                pending.len(),
                preview(&self.sandbox, &pending)
            );
            return Ok(ToolOutput::new(noted(note, text)));
        }
        let headline = format!(
            "Renamed {} to `{new_name}`: {edits} edit(s) in {} file(s).",
            pointed.what,
            pending.len()
        );
        let mut out = commit(&self.sandbox, &self.languages, headline, pending, None).await?;
        out.text = noted(note, out.text);
        Ok(out)
    }

    /// The stretch of a file `args` names for `action`: the symbol it points at, a line and column, a whole line, or the lines from `line` to `end_line`, ending before `end_column` when it is given and otherwise at the end of the last line; with how it is named to the model and a note when the symbol was not where it said.
    ///
    /// # Errors
    ///
    /// Returns [`ToolError::InvalidArguments`] when the path or the line is missing or the stretch ends before it starts, and [`ToolError::Failed`] when the file cannot be read or the symbol is not found.
    async fn stretch(&self, args: &Args, action: &str) -> Result<Pointed, ToolError> {
        let shown = needed(args.path.as_deref(), "path", action)?;
        let path = self.file(shown)?;
        let shown = self.sandbox.display(&path);
        let text = tokio::fs::read_to_string(&path)
            .await
            .map_err(|error| ToolError::Failed(format!("{shown} cannot be read: {error}")))?;
        let line_length = |line: u32| -> u32 {
            let content = text
                .lines()
                .nth(usize::try_from(line.saturating_sub(1)).unwrap_or(usize::MAX))
                .unwrap_or_default();
            u32::try_from(content.chars().count()).unwrap_or(u32::MAX)
        };
        if let (Some(symbol), None) = (args.symbol.as_deref(), args.end_line) {
            let found = match args.line {
                Some(line) => position::find(&text, line, symbol, args.occurrence),
                None => position::find_anywhere(&text, symbol),
            }
            .map_err(|reason| ToolError::Failed(format!("{shown}: {reason}")))?;
            let end_column = found.column + position::named_length(symbol);
            return Ok(Pointed {
                what: format!(
                    "`{}` at {shown}:{}:{}",
                    symbol.trim(),
                    found.line,
                    found.column
                ),
                spot: Spot {
                    path: path.clone(),
                    line: found.line,
                    column: found.column,
                },
                end: Some((found.line, end_column)),
                note: found.note,
            });
        }
        let line = needed(args.line, "line", action)?.max(1);
        let column = args.column.unwrap_or(1).max(1);
        let (end_line, end_column) = match (args.end_line, args.column) {
            (Some(end_line), _) => (
                end_line,
                args.end_column.unwrap_or_else(|| line_length(end_line) + 1),
            ),
            (None, Some(column)) => (line, args.end_column.unwrap_or(column)),
            (None, None) => (line, line_length(line) + 1),
        };
        if (end_line, end_column) < (line, column) {
            return Err(ToolError::InvalidArguments(format!(
                "{action}: the stretch ends at {end_line}:{end_column}, before it starts at {line}:{column}"
            )));
        }
        let what = if (end_line, end_column) == (line, line_length(line) + 1) && column == 1 {
            format!("{shown}:{line}")
        } else if end_line == line && args.end_line.is_none() {
            format!("{shown}:{line}:{column}")
        } else {
            format!("{shown}:{line}:{column}-{end_line}:{end_column}")
        };
        Ok(Pointed {
            spot: Spot { path, line, column },
            what,
            end: Some((end_line, end_column)),
            note: None,
        })
    }

    /// Lists the code actions the server offers for the stretch `args` names, or applies the one it chooses: writes what it changes, or for a dry run shows it.
    ///
    /// # Errors
    ///
    /// As [`Lsp::stretch`]; [`ToolError::InvalidArguments`] when `apply` is nought, [`ToolError::Failed`] when this copy only looks and the action is applied without a dry run, or the action reaches outside the working directory, and the service's failure.
    async fn code_actions(
        &self,
        service: &dyn LanguageService,
        args: &Args,
    ) -> Result<ToolOutput, ToolError> {
        if args.apply.is_some() {
            self.may_write(args, "applying a code action")?;
        }
        let pointed = self.stretch(args, "code_actions").await?;
        service.serves(&pointed.spot.path).await.map_err(failed)?;
        let range = pointed.location();
        let only: Vec<String> = args
            .kind
            .iter()
            .flat_map(|kinds| kinds.split(','))
            .map(str::trim)
            .filter(|kind| !kind.is_empty())
            .map(str::to_owned)
            .collect();
        let note = pointed.note.as_deref();
        let Some(index) = args.apply else {
            let offered = service.code_actions(&range, &only).await.map_err(failed)?;
            return Ok(ToolOutput::new(noted(
                note,
                render::code_actions(&pointed.what, &offered),
            )));
        };
        if index == 0 {
            return Err(ToolError::InvalidArguments(
                "apply counts from 1, as the list of code actions does".to_owned(),
            ));
        }
        let chosen = Chosen {
            index,
            title: args.title.clone(),
        };
        let applied = service
            .apply_code_action(&range, &only, &chosen)
            .await
            .map_err(failed)?;
        let what = format!("the code action \"{}\"", applied.title);
        let pending = pending(&self.sandbox, applied.files, &what).await?;
        if pending.is_empty() {
            return Ok(ToolOutput::new(noted(
                note,
                format!("Applying {what} at {} changed no files.", pointed.what),
            )));
        }
        let edits: usize = pending.iter().map(|file| file.edits).sum();
        if args.dry_run {
            let text = format!(
                "Dry run: {what} at {} would make {edits} edit(s) in {} file(s); nothing was written.{}",
                pointed.what,
                pending.len(),
                preview(&self.sandbox, &pending)
            );
            return Ok(ToolOutput::new(noted(note, text)));
        }
        let headline = format!(
            "Applied {what} at {}: {edits} edit(s) in {} file(s).",
            pointed.what,
            pending.len()
        );
        let mut out = commit(&self.sandbox, &self.languages, headline, pending, None).await?;
        out.text = noted(note, out.text);
        Ok(out)
    }

    /// Formats the file `args` names, or only its lines from `line` to `end_line`, and writes it, or for a dry run shows what would change.
    ///
    /// # Errors
    ///
    /// Returns [`ToolError::InvalidArguments`] when the path is missing, [`ToolError::Failed`] when this copy only looks and the call is not a dry run, and the service's failure.
    async fn format(
        &self,
        service: &dyn LanguageService,
        args: &Args,
    ) -> Result<ToolOutput, ToolError> {
        self.may_write(args, "format")?;
        let shown = needed(args.path.as_deref(), "path", "format")?;
        let path = self.file(shown)?;
        let range = match args.line {
            Some(_) => {
                let lines = Args {
                    symbol: None,
                    column: None,
                    end_column: None,
                    end_line: args.end_line.or(args.line),
                    ..args.clone()
                };
                Some(self.stretch(&lines, "format").await?)
            }
            None => None,
        };
        let what = range
            .as_ref()
            .map_or_else(|| self.sandbox.display(&path), |range| range.what.clone());
        service.serves(&path).await.map_err(failed)?;
        let location = range.as_ref().map(Pointed::location);
        let formatted = tokio::time::timeout(
            crate::FORMAT_WAIT * 10 + GRACE,
            service.reformat(&path, location.as_ref()),
        )
        .await
        .map_err(|_| ToolError::Failed("the language server did not format in time".to_owned()))?
        .map_err(failed)?;
        let old = tokio::fs::read_to_string(&path).await?;
        let Some(new) = formatted.filter(|new| *new != old) else {
            return Ok(ToolOutput::new(format!(
                "{what} is formatted already; nothing was changed."
            )));
        };
        let changed = render::changed_line_count(&old, &new);
        let pending = vec![Pending {
            path,
            old: Some(old),
            new,
            edits: changed,
        }];
        if args.dry_run {
            return Ok(ToolOutput::new(format!(
                "Dry run: formatting {what} would change {changed} line(s); nothing was written.{}",
                preview(&self.sandbox, &pending)
            )));
        }
        let headline = format!("Formatted {what}: {changed} line(s) changed.");
        commit(&self.sandbox, &self.languages, headline, pending, None).await
    }

    /// Moves the file `args` names to its new path, making the edits the servers ask for first, such as to the imports that name it, and telling them it moved; or for a dry run shows what would change.
    ///
    /// # Errors
    ///
    /// Returns [`ToolError::InvalidArguments`] when the path or the new path is missing, [`ToolError::OutsidePath`] for either outside the working directory, [`ToolError::Failed`] when this copy only looks and the call is not a dry run, the new path is taken, or the edits reach outside the working directory, and the service's failure.
    async fn rename_file(
        &self,
        service: &dyn LanguageService,
        args: &Args,
    ) -> Result<ToolOutput, ToolError> {
        self.may_write(args, "rename_file")?;
        let from = self.file(needed(args.path.as_deref(), "path", "rename_file")?)?;
        let to =
            self.sandbox
                .resolve(needed(args.new_path.as_deref(), "new_path", "rename_file")?)?;
        let (shown_from, shown_to) = (self.sandbox.display(&from), self.sandbox.display(&to));
        if to.exists() {
            return Err(ToolError::Failed(format!(
                "{shown_to} exists already; choose a new path that does not"
            )));
        }
        let changed = match service.serves(&from).await {
            Ok(()) => service.rename_file(&from, &to).await.map_err(failed)?,
            Err(LanguageError::NoServer(_)) => Vec::new(),
            Err(error) => return Err(failed(error)),
        };
        let what = format!("moving {shown_from} to {shown_to}");
        let mut pending = pending(&self.sandbox, changed, &what).await?;
        let text = tokio::fs::read_to_string(&from).await?;
        let moved = match pending.iter().position(|file| file.path == from) {
            Some(index) => {
                let edited = pending.remove(index);
                Pending {
                    path: to.clone(),
                    old: None,
                    new: edited.new,
                    edits: edited.edits,
                }
            }
            None => Pending {
                path: to.clone(),
                old: None,
                new: text.clone(),
                edits: 0,
            },
        };
        let others = pending.len();
        let edits: usize = pending.iter().map(|file| file.edits).sum::<usize>() + moved.edits;
        pending.insert(0, moved);
        if args.dry_run {
            return Ok(ToolOutput::new(format!(
                "Dry run: moving {shown_from} to {shown_to} would make {edits} edit(s) in {others} other file(s); nothing was moved or written.{}",
                preview(&self.sandbox, &pending)
            )));
        }
        let headline = format!(
            "Moved {shown_from} to {shown_to}, with {edits} edit(s) in {others} other file(s)."
        );
        let out = commit(
            &self.sandbox,
            &self.languages,
            headline,
            pending,
            Some(Moved {
                from: from.clone(),
                text,
            }),
        )
        .await?;
        service.file_renamed(&from, &to).await;
        Ok(out)
    }

    /// Says which language servers the conversation has and how each stands.
    ///
    /// # Errors
    ///
    /// Returns the service's failure.
    async fn status(&self, service: &dyn LanguageService) -> Result<String, ToolError> {
        let servers = service.status().await.map_err(failed)?;
        Ok(render::servers("Language servers", &servers))
    }

    /// Restarts the server `args` names, or the one of its path's language, or every running one, and says how they stand.
    ///
    /// # Errors
    ///
    /// Returns [`ToolError::OutsidePath`] for a path outside the working directory, and the service's failure.
    async fn reload(
        &self,
        service: &dyn LanguageService,
        args: &Args,
    ) -> Result<String, ToolError> {
        let near = args
            .path
            .as_deref()
            .map(|shown| self.sandbox.resolve(shown))
            .transpose()?;
        let server = args
            .server
            .as_deref()
            .map(str::trim)
            .filter(|name| !name.is_empty());
        let servers = service
            .reload(server, near.as_deref())
            .await
            .map_err(failed)?;
        Ok(render::servers("Restarted", &servers))
    }
}

#[async_trait]
impl Tool for Lsp {
    /// Returns `lsp`.
    fn name(&self) -> &'static str {
        "lsp"
    }

    /// Describes the tool to the model.
    fn description(&self) -> &'static str {
        DESCRIPTION
    }

    /// Returns the argument schema.
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["definition", "type_definition", "implementation", "references", "hover", "symbols", "diagnostics", "rename", "code_actions", "format", "rename_file", "status", "reload"],
                    "description": "What to ask the language server."
                },
                "path": { "type": "string", "description": "The file, relative to the working directory. For diagnostics also a folder; leave out for the whole project. For rename_file the file to move; for reload a file whose language's server to restart." },
                "line": { "type": "integer", "minimum": 1, "description": "The line the symbol is on, counted from 1. Recommended; without it the first occurrence in the file is used. For code_actions and format, the first line of the stretch." },
                "symbol": { "type": "string", "description": "The symbol's name exactly as written on that line, e.g. parse_config or Config::load." },
                "occurrence": { "type": "integer", "minimum": 1, "description": "Which occurrence of symbol on the line, counted from 1. Defaults to 1." },
                "column": { "type": "integer", "minimum": 1, "description": "The character the symbol starts at, counted from 1, instead of symbol." },
                "end_line": { "type": "integer", "minimum": 1, "description": "For code_actions and format: the last line of the stretch, counted from 1." },
                "end_column": { "type": "integer", "minimum": 1, "description": "For code_actions: the character the stretch ends before on end_line, counted from 1. Defaults to the end of the line." },
                "query": { "type": "string", "description": "For symbols: search the whole project for symbols whose names match this." },
                "new_name": { "type": "string", "description": "For rename: the new name." },
                "kind": { "type": "string", "description": "For code_actions: only actions of these kinds, comma separated, e.g. quickfix, refactor.extract or source.organizeImports." },
                "apply": { "type": "integer", "minimum": 1, "description": "For code_actions: the number of the action to apply, from the list the same call without apply gives." },
                "title": { "type": "string", "description": "For code_actions with apply: the action's title as listed, so the right one is applied if the list changed." },
                "new_path": { "type": "string", "description": "For rename_file: where the file goes, relative to the working directory." },
                "server": { "type": "string", "description": "For reload: the name of the server to restart, as status lists it." },
                "dry_run": { "type": "boolean", "description": "For rename, code_actions with apply, format and rename_file: show the changes without writing them. Defaults to false." }
            },
            "required": ["action"]
        })
    }

    /// Whether this copy only looks: the full tool can rename, apply code actions, format and move files, which write files.
    fn read_only(&self) -> bool {
        self.looking
    }

    /// A copy that only looks, refusing every call that writes files except dry runs; nothing for a copy that already only looks.
    fn looking(&self) -> Option<Arc<dyn Tool>> {
        (!self.looking).then(|| {
            Arc::new(Self {
                sandbox: self.sandbox.clone(),
                languages: self.languages.clone(),
                looking: true,
            }) as Arc<dyn Tool>
        })
    }

    /// Some calls write files, so the full tool may need approval.
    fn needs_approval(&self) -> bool {
        !self.looking
    }

    /// `lsp ` and the action, such as `lsp rename` or `lsp format`, for a call that writes files; nothing for the calls that only look.
    fn approval_scope(&self, args: &Value) -> Option<String> {
        (!self.looking && writes(args))
            .then(|| format!("lsp {}", args["action"].as_str().unwrap_or_default()))
    }

    /// Exclusive for a call that writes files, and shared for the calls that only look.
    fn concurrency(&self, args: &Value) -> Concurrency {
        match self.approval_scope(args) {
            Some(_) => Concurrency::Exclusive,
            None => Concurrency::Shared,
        }
    }

    /// Shows the action, the place and, for a change, what it changes to.
    fn describe(&self, args: &Value) -> String {
        let action = args["action"].as_str().unwrap_or("?");
        let mut line = action.to_owned();
        if let Some(symbol) = args["symbol"].as_str() {
            line.push_str(&format!(" `{symbol}`"));
        }
        if let Some(query) = args["query"].as_str() {
            line.push_str(&format!(" \"{query}\""));
        }
        if let Some(server) = args["server"].as_str() {
            line.push_str(&format!(" {server}"));
        }
        if let Some(path) = args["path"].as_str() {
            line.push_str(&format!(" in {}", self.sandbox.describe(path)));
            if let Some(number) = args["line"].as_u64() {
                line.push_str(&format!(":{number}"));
            }
            if let Some(number) = args["end_line"].as_u64() {
                line.push_str(&format!("-{number}"));
            }
        }
        if let Some(name) = args["new_name"].as_str() {
            line.push_str(&format!(" to `{name}`"));
        }
        if let Some(path) = args["new_path"].as_str() {
            line.push_str(&format!(" to {}", self.sandbox.describe(path)));
        }
        if let Some(kind) = args["kind"].as_str() {
            line.push_str(&format!(" ({kind})"));
        }
        if let Some(number) = args["apply"].as_u64() {
            match args["title"].as_str() {
                Some(title) => line.push_str(&format!(", apply #{number} \"{title}\"")),
                None => line.push_str(&format!(", apply #{number}")),
            }
        }
        if args["dry_run"].as_bool() == Some(true) {
            line.push_str(" [dry run]");
        }
        line
    }

    /// Asks the language service what the action needs, and for a change writes what it changes.
    async fn call(&self, args: Value) -> Result<ToolOutput, ToolError> {
        let args: Args = parse_args(args)?;
        let service = self.service()?;
        let service = service.as_ref();
        if let Some(lookup) = args.action.lookup() {
            return self
                .lookup(service, lookup, &args, args.action.name())
                .await
                .map(ToolOutput::new);
        }
        match args.action {
            Action::Hover => self.hover(service, &args).await.map(ToolOutput::new),
            Action::Symbols => self.symbols(service, &args).await.map(ToolOutput::new),
            Action::Diagnostics => self.diagnostics(service, &args).await.map(ToolOutput::new),
            Action::CodeActions => self.code_actions(service, &args).await,
            Action::Format => self.format(service, &args).await,
            Action::RenameFile => self.rename_file(service, &args).await,
            Action::Status => self.status(service).await.map(ToolOutput::new),
            Action::Reload => self.reload(service, &args).await.map(ToolOutput::new),
            _ => self.rename(service, &args).await,
        }
    }
}

#[cfg(test)]
/// Tests for the tool.
mod tests;
