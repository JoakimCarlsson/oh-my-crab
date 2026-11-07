//! The `lsp` tool: asks the conversation's language services where code is defined and used, what a symbol is, which symbols a file or the project has and what is wrong with them, and renames a symbol across files.

mod position;
mod render;

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};

use super::atomic::write_atomic;
use super::snapshot::tag;
use crate::language::{GRACE, Languages, Written, settle};
use crate::tool::parse_args;
use crate::{
    FileChange, LanguageError, LanguageService, Lookup, Sandbox, Spot, Tool, ToolError, ToolOutput,
};
use render::Lines;

/// What the model is told when the conversation has no language services at all.
const NO_SERVICES: &str = "No language server is available in this conversation, so `lsp` cannot answer. Use grep, ast_grep or read instead.";

/// The most changed lines a rename shows for each file.
const PREVIEW_LINES: usize = 20;

/// What the tool tells the model it does.
const DESCRIPTION: &str = "Ask the language server of a file about code: where a symbol is defined or used, what it is, which symbols a file or the project has, what is wrong with the code, or rename a symbol everywhere.\n\
Actions:\n\
- definition, type_definition, implementation, references, hover: need path and symbol (the name as written in the file), best with the line it is on (use occurrence when it appears more than once on that line); or line and column instead of symbol. Without a line, the symbol's first whole-word occurrence in the file is used. A qualified symbol such as Type::method points at its last part. A symbol a few lines off is still found, with a note.\n\
- symbols: with path, the outline of that file; with query, a search of the whole project for symbols whose names match (path then picks which language's server to ask).\n\
- diagnostics: with path, the errors and warnings of that file (or of every file under a folder); without, of the whole project as the servers know it.\n\
- rename: renames the symbol at path, line and symbol (as for references) to new_name in every file that uses it, then formats the changed files and reports their diagnostics and new snapshot tags. The user is asked to approve it. dry_run: true shows the changes without writing.\n\
Results are compact: file:line:column with the line quoted. Prefer this over grep to find the real uses of a symbol, and over editing each file by hand to rename one. When no server handles the file type, the call says so; fall back to grep, ast_grep or ast_edit.";

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
        }
    }

    /// The lookup the action asks for, when it is one of those.
    fn lookup(self) -> Option<Lookup> {
        match self {
            Action::Definition => Some(Lookup::Definition),
            Action::TypeDefinition => Some(Lookup::TypeDefinition),
            Action::Implementation => Some(Lookup::Implementation),
            Action::References => Some(Lookup::References),
            Action::Hover | Action::Symbols | Action::Diagnostics | Action::Rename => None,
        }
    }
}

/// Arguments of an `lsp` call.
#[derive(Deserialize)]
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
    /// Whether a rename only shows what it would change.
    #[serde(default)]
    dry_run: bool,
}

/// A place in a file the model pointed at, resolved.
struct Pointed {
    /// The place, as the language service is asked about it.
    spot: Spot,
    /// How the place is named to the model, such as `` `helper` at src/lib.rs:3:8 ``.
    what: String,
    /// What to tell the model about where its symbol was found, when not where it said.
    note: Option<String>,
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
        if self.looking && !args.dry_run {
            return Err(ToolError::Failed(
                "rename changes files, which is not allowed now (plan mode or a read-only agent). Set dry_run: true to see what it would change.".to_owned(),
            ));
        }
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
        let mut outside = Vec::new();
        let mut files = Vec::new();
        for file in renamed {
            match self.sandbox.resolve(&file.path.to_string_lossy()) {
                Ok(path) => files.push((path, file)),
                Err(_) => outside.push(file.path.display().to_string()),
            }
        }
        if !outside.is_empty() {
            return Err(ToolError::Failed(format!(
                "renaming {} would change files outside the working directory, so nothing was changed: {}",
                pointed.what,
                outside.join(", ")
            )));
        }
        let mut pending = Vec::with_capacity(files.len());
        for (path, file) in files {
            let old = tokio::fs::read_to_string(&path).await?;
            if old != file.text {
                pending.push((path, old, file.text, file.edits));
            }
        }
        if pending.is_empty() {
            return Err(ToolError::Failed(format!(
                "the language server found nothing to change in renaming {}",
                pointed.what
            )));
        }
        let edits: usize = pending.iter().map(|(.., edits)| edits).sum();
        if args.dry_run {
            let mut text = format!(
                "Dry run: renaming {} to `{new_name}` would make {edits} edit(s) in {} file(s); nothing was written.",
                pointed.what,
                pending.len()
            );
            for (path, old, new, edits) in &pending {
                text.push_str(&format!(
                    "\n\n{}: {edits} edit(s){}",
                    self.sandbox.display(path),
                    render::changed_lines(old, new, PREVIEW_LINES)
                ));
            }
            return Ok(ToolOutput::new(noted(pointed.note.as_deref(), text)));
        }
        write_all(&pending)?;
        let mut written: Vec<Written> = pending
            .iter()
            .map(|(path, _, new, _)| Written {
                path: path.clone(),
                shown: self.sandbox.display(path),
                text: new.clone(),
                formatted: false,
            })
            .collect();
        let diagnostics = settle(&self.languages, &mut written).await;
        let mut text = format!(
            "Renamed {} to `{new_name}`: {edits} edit(s) in {} file(s).",
            pointed.what,
            pending.len()
        );
        for ((_, _, _, edits), file) in pending.iter().zip(&written) {
            let formatted = if file.formatted { ", formatted" } else { "" };
            text.push_str(&format!(
                "\n{}: {edits} edit(s){formatted}, tag {}",
                file.shown,
                tag(file.text.as_bytes())
            ));
        }
        if !diagnostics.is_empty() {
            text.push_str("\n\n");
            text.push_str(&diagnostics);
        }
        let changes = pending
            .into_iter()
            .zip(written)
            .map(|((_, old, _, _), file)| FileChange {
                path: file.shown,
                old,
                new: file.text,
            })
            .collect();
        Ok(ToolOutput::new(noted(pointed.note.as_deref(), text)).with_changes(changes))
    }
}

/// Writes the new text of every file in `pending`; when one write fails, puts the files already written back as they were.
///
/// # Errors
///
/// Returns the error of the write that failed.
fn write_all(pending: &[(PathBuf, String, String, usize)]) -> Result<(), ToolError> {
    for (done, (path, _, new, _)) in pending.iter().enumerate() {
        if let Err(error) = write_atomic(path, new.as_bytes()) {
            for (path, old, _, _) in &pending[..done] {
                let _ = write_atomic(path, old.as_bytes());
            }
            return Err(error);
        }
    }
    Ok(())
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
                    "enum": ["definition", "type_definition", "implementation", "references", "hover", "symbols", "diagnostics", "rename"],
                    "description": "What to ask the language server."
                },
                "path": { "type": "string", "description": "The file, relative to the working directory. For diagnostics also a folder; leave out for the whole project." },
                "line": { "type": "integer", "minimum": 1, "description": "The line the symbol is on, counted from 1. Recommended; without it the first occurrence in the file is used." },
                "symbol": { "type": "string", "description": "The symbol's name exactly as written on that line, e.g. parse_config or Config::load." },
                "occurrence": { "type": "integer", "minimum": 1, "description": "Which occurrence of symbol on the line, counted from 1. Defaults to 1." },
                "column": { "type": "integer", "minimum": 1, "description": "The character the symbol starts at, counted from 1, instead of symbol." },
                "query": { "type": "string", "description": "For symbols: search the whole project for symbols whose names match this." },
                "new_name": { "type": "string", "description": "For rename: the new name." },
                "dry_run": { "type": "boolean", "description": "For rename: show the changes without writing them. Defaults to false." }
            },
            "required": ["action"]
        })
    }

    /// Whether this copy only looks: the full tool can rename, which writes files.
    fn read_only(&self) -> bool {
        self.looking
    }

    /// A copy that only looks, refusing renames except dry runs; nothing for a copy that already only looks.
    fn looking(&self) -> Option<Arc<dyn Tool>> {
        (!self.looking).then(|| {
            Arc::new(Self {
                sandbox: self.sandbox.clone(),
                languages: self.languages.clone(),
                looking: true,
            }) as Arc<dyn Tool>
        })
    }

    /// Renames write files, so the full tool may need approval.
    fn needs_approval(&self) -> bool {
        !self.looking
    }

    /// `lsp rename` for a rename that writes; nothing for the calls that only look.
    fn approval_scope(&self, args: &Value) -> Option<String> {
        (!self.looking
            && args["action"].as_str() == Some("rename")
            && args["dry_run"].as_bool() != Some(true))
        .then(|| "lsp rename".to_owned())
    }

    /// Shows the action, the place and, for a rename, the new name.
    fn describe(&self, args: &Value) -> String {
        let action = args["action"].as_str().unwrap_or("?");
        let mut line = action.to_owned();
        if let Some(symbol) = args["symbol"].as_str() {
            line.push_str(&format!(" `{symbol}`"));
        }
        if let Some(query) = args["query"].as_str() {
            line.push_str(&format!(" \"{query}\""));
        }
        if let Some(path) = args["path"].as_str() {
            line.push_str(&format!(" in {}", self.sandbox.describe(path)));
            if let Some(number) = args["line"].as_u64() {
                line.push_str(&format!(":{number}"));
            }
        }
        if let Some(name) = args["new_name"].as_str() {
            line.push_str(&format!(" to `{name}`"));
        }
        if args["dry_run"].as_bool() == Some(true) {
            line.push_str(" [dry run]");
        }
        line
    }

    /// Asks the language service what the action needs, and for a rename writes what it changes.
    async fn call(&self, args: Value) -> Result<ToolOutput, ToolError> {
        let args: Args = parse_args(args)?;
        let service = self.service()?;
        let service = service.as_ref();
        match (args.action, args.action.lookup()) {
            (action, Some(lookup)) => self
                .lookup(service, lookup, &args, action.name())
                .await
                .map(ToolOutput::new),
            (Action::Hover, None) => self.hover(service, &args).await.map(ToolOutput::new),
            (Action::Symbols, None) => self.symbols(service, &args).await.map(ToolOutput::new),
            (Action::Diagnostics, None) => {
                self.diagnostics(service, &args).await.map(ToolOutput::new)
            }
            (_, None) => self.rename(service, &args).await,
        }
    }
}

#[cfg(test)]
/// Tests for the tool.
mod tests;
