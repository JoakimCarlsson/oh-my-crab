//! The language servers the harness runs itself for one project, started the first time a file of theirs is asked about, or before when asked to warm them, and started again when one stops.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use omc_tools::{
    Applied, Chosen, CodeAction, Diagnostic, LanguageError, LanguageService, Location, Lookup,
    Renamed, ServerStatus, Spot, Symbol,
};
use serde_json::{Value, json};
use tokio::sync::OnceCell;
use tokio::time::Instant;

use crate::client::{Client, diagnostic};
use crate::navigate::{self, Texts};
use crate::spec::{Spec, default_specs};
use crate::text::{character, path_of};

/// A server once it has been tried: the running server, or nothing when it could not be started.
type Started = Arc<OnceCell<Option<Arc<Client>>>>;

/// How long to wait for the diagnostics of a file whose server is still starting or loading the project, which for a large project takes far longer than the usual wait.
pub const LOADING_WAIT: Duration = Duration::from_secs(20);

/// How long a server may take to answer a question about code.
const QUESTION_WAIT: Duration = Duration::from_secs(30);

/// A server ready to be asked about one file.
struct Ready {
    /// The server's name.
    id: String,
    /// The server.
    client: Arc<Client>,
    /// The file's address.
    uri: String,
    /// The file's text, as the server was told it.
    text: String,
}

impl Ready {
    /// The position of the protocol for the place `at` in the file.
    fn position(&self, at: &Spot) -> Value {
        let line = at.line.saturating_sub(1);
        json!({"line": line, "character": character(&self.text, line, at.column)})
    }

    /// The range of the protocol for the stretch `range` of the file.
    fn range(&self, range: &Location) -> Value {
        let (line, end_line) = (
            range.line.saturating_sub(1),
            range.end_line.saturating_sub(1),
        );
        json!({
            "start": {"line": line, "character": character(&self.text, line, range.column)},
            "end": {"line": end_line, "character": character(&self.text, end_line, range.end_column)},
        })
    }

    /// The code actions the server offers for the stretch `range` of the file, only those of the kinds `only` when it names any, as the server wrote them, with what it said is wrong there.
    ///
    /// # Errors
    ///
    /// As [`Ready::ask`].
    async fn code_actions(
        &self,
        range: &Location,
        only: &[String],
    ) -> Result<Vec<Value>, LanguageError> {
        let wanted = self.range(range);
        let (start, end) = (line_of(&wanted["start"]), line_of(&wanted["end"]));
        let wrong: Vec<Value> = self
            .client
            .published_for(&self.uri)
            .into_iter()
            .filter(|diagnostic| {
                let range = &diagnostic["range"];
                line_of(&range["start"]) <= end && start <= line_of(&range["end"])
            })
            .collect();
        let mut context = json!({"diagnostics": wrong, "triggerKind": 1});
        if !only.is_empty() {
            context["only"] = json!(only);
        }
        let answer = self
            .ask(
                "codeActionProvider",
                "code action",
                "textDocument/codeAction",
                json!({"range": wanted, "context": context}),
            )
            .await?;
        Ok(navigate::code_actions(&answer))
    }

    /// Asks the server `method` about the file with `params` added to the file's address, once it is known to answer what `capability` names, which `what` describes to the model.
    ///
    /// # Errors
    ///
    /// Returns [`LanguageError::Unsupported`] when the server does not say it can, and [`LanguageError::Failed`] when it fails.
    async fn ask(
        &self,
        capability: &str,
        what: &str,
        method: &str,
        mut params: Value,
    ) -> Result<Value, LanguageError> {
        if !self.client.can(capability) {
            return Err(LanguageError::Unsupported(format!(
                "the {} language server does not answer {what} requests",
                self.id
            )));
        }
        params["textDocument"] = json!({"uri": self.uri});
        self.client
            .ask(method, params, QUESTION_WAIT)
            .await
            .map_err(LanguageError::Failed)
    }
}

/// The line of the position `position` of the protocol, counted from nought; nought when it has none.
fn line_of(position: &Value) -> u64 {
    position["line"].as_u64().unwrap_or(0)
}

/// How long a server is given to shut down when it is restarted, before its process is ended.
const STOP_WAIT: Duration = Duration::from_secs(2);

/// The most times a server that stopped is started again, so that one that keeps stopping is given up on.
const MOST_RESTARTS: u32 = 3;

/// One server's place: its latest start, and how many times it was started again after stopping.
#[derive(Default)]
struct Slot {
    /// The latest start.
    started: Started,
    /// How many times it was started again.
    restarts: u32,
}

/// The language servers of one project, each started the first time a file it reads is formatted or checked, and stopped when this is dropped.
pub struct Servers {
    /// The folder the project is in, which the servers work on.
    root: PathBuf,
    /// The servers that may be started.
    specs: Vec<Spec>,
    /// The servers tried so far, by name.
    started: Mutex<HashMap<String, Slot>>,
}

impl Servers {
    /// The usual language servers of each common language, for the project in `root`; none is started yet.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self::with_specs(root, default_specs())
    }

    /// The language servers `specs` describes, for the project in `root`; none is started yet.
    pub fn with_specs(root: impl Into<PathBuf>, specs: Vec<Spec>) -> Self {
        Self {
            root: root.into(),
            specs,
            started: Mutex::new(HashMap::new()),
        }
    }

    /// Starts, in the background, the servers of the languages the project is written in, as the files at its root say, and whose programs are installed, so that they have loaded the project by the time the first file is written.
    pub fn warm(&self) {
        for spec in &self.specs {
            if spec.is_used_in(&self.root) && spec.is_installed() {
                drop(self.starting(spec));
            }
        }
    }

    /// Whether the server that reads the file at `path` is still starting or loading the project: it has not been started, is being started, or has not said anything about any file yet. A server that could not be started, or no server at all, is not loading.
    fn is_loading(&self, path: &Path) -> bool {
        let Some(spec) = self
            .specs
            .iter()
            .find(|spec| spec.language_of(path).is_some())
        else {
            return false;
        };
        if !spec.is_installed() {
            return false;
        }
        let started = self.started.lock().unwrap_or_else(|held| held.into_inner());
        match started.get(&spec.id).map(|slot| slot.started.get()) {
            None | Some(None) => true,
            Some(Some(None)) => false,
            Some(Some(Some(client))) => !client.has_spoken(),
        }
    }

    /// The running server that reads the file at `path`, with the language id it reads it in, starting the server the first time; nothing when no server reads such files or it could not be started.
    async fn server_for(&self, path: &Path) -> Option<(Arc<Client>, String)> {
        self.server(path)
            .await
            .ok()
            .map(|(_, client, language)| (client, language))
    }

    /// The spec, running server and language id of the file at `path`, starting the server the first time.
    ///
    /// # Errors
    ///
    /// Returns [`LanguageError::NoServer`], saying why, when no server reads such files, its program is not installed, or it could not be started.
    async fn server(&self, path: &Path) -> Result<(&Spec, Arc<Client>, String), LanguageError> {
        let Some((spec, language)) = self.specs.iter().find_map(|spec| {
            spec.language_of(path)
                .map(|language| (spec, language.to_owned()))
        }) else {
            let kind = path.extension().map_or_else(
                || format!("files like {}", path.display()),
                |extension| format!(".{} files", extension.to_string_lossy()),
            );
            return Err(LanguageError::NoServer(format!(
                "no language server handles {kind}"
            )));
        };
        if !spec.is_installed() {
            let programs: Vec<&str> = spec
                .commands
                .iter()
                .filter_map(|command| command.first().map(String::as_str))
                .collect();
            return Err(LanguageError::NoServer(format!(
                "the {} language server for {language} files is not installed (looked for {})",
                spec.id,
                programs.join(", ")
            )));
        }
        match self.starting(spec).await.ok().flatten() {
            Some(client) => Ok((spec, client, language)),
            None => Err(LanguageError::NoServer(format!(
                "the {} language server could not be started",
                spec.id
            ))),
        }
    }

    /// The running server of the file at `path`, told the file's text on disk now and every other file it was told about brought up to date, once it has loaded the project or [`LOADING_WAIT`] has passed; with the server's name, the file's address and its text.
    ///
    /// # Errors
    ///
    /// As [`Servers::server`], and [`LanguageError::Failed`] when the file cannot be read or the server told about it.
    async fn ready(&self, path: &Path) -> Result<Ready, LanguageError> {
        let (spec, client, language) = self.server(path).await?;
        client.refresh().await;
        let text = tokio::fs::read_to_string(path).await.map_err(|error| {
            LanguageError::Failed(format!("{} cannot be read: {error}", path.display()))
        })?;
        let uri = client
            .open(path, &language, &text)
            .await
            .map_err(LanguageError::Failed)?;
        client.settle(Instant::now() + LOADING_WAIT, false).await;
        Ok(Ready {
            id: spec.id.clone(),
            client,
            uri,
            text,
        })
    }

    /// The servers of the project: those already running, and those of the languages its root says it is written in whose programs are installed, started now if they were not.
    async fn project(&self) -> Vec<(String, Arc<Client>)> {
        let mut found = Vec::new();
        for spec in &self.specs {
            let tried = self
                .started
                .lock()
                .unwrap_or_else(|held| held.into_inner())
                .contains_key(&spec.id);
            if !tried && !(spec.is_used_in(&self.root) && spec.is_installed()) {
                continue;
            }
            if let Some(client) = self.starting(spec).await.ok().flatten() {
                client.refresh().await;
                found.push((spec.id.clone(), client));
            }
        }
        found
    }

    /// How the server `spec` describes stands now.
    fn status_of(&self, spec: &Spec) -> ServerStatus {
        let mut languages: Vec<String> = Vec::new();
        for (_, language) in &spec.extensions {
            if !languages.contains(language) {
                languages.push(language.clone());
            }
        }
        let (state, detail) = {
            let started = self.started.lock().unwrap_or_else(|held| held.into_inner());
            match started.get(&spec.id) {
                None if spec.is_installed() => ("not started".to_owned(), None),
                None => {
                    let programs: Vec<&str> = spec
                        .commands
                        .iter()
                        .filter_map(|command| command.first().map(String::as_str))
                        .collect();
                    (
                        "not installed".to_owned(),
                        Some(format!("looked for {}", programs.join(", "))),
                    )
                }
                Some(slot) => {
                    let restarts = (slot.restarts > 0)
                        .then(|| format!("started again {} time(s)", slot.restarts));
                    match slot.started.get() {
                        None => ("starting".to_owned(), restarts),
                        Some(None) => (
                            "failed".to_owned(),
                            Some("it could not be started".to_owned()),
                        ),
                        Some(Some(client)) if !client.is_running() => {
                            ("stopped".to_owned(), restarts)
                        }
                        Some(Some(client)) => {
                            let work = client.work();
                            if work.is_empty() {
                                ("running".to_owned(), restarts)
                            } else {
                                let doing = format!("working on: {}", work.join(", "));
                                (
                                    "loading".to_owned(),
                                    Some(restarts.map_or(doing.clone(), |restarts| {
                                        format!("{doing}; {restarts}")
                                    })),
                                )
                            }
                        }
                    }
                }
            }
        };
        ServerStatus {
            name: spec.id.clone(),
            languages,
            state,
            detail,
        }
    }

    /// Stops the server `spec` describes when it runs, and starts it again as if for the first time.
    async fn restart(&self, spec: &Spec) -> ServerStatus {
        let running = {
            let mut started = self.started.lock().unwrap_or_else(|held| held.into_inner());
            let slot = started.entry(spec.id.clone()).or_default();
            let running = slot.started.get().cloned().flatten();
            *slot = Slot::default();
            running
        };
        if let Some(client) = running {
            client.stop(STOP_WAIT).await;
        }
        let _ = self.starting(spec).await;
        self.status_of(spec)
    }

    /// The server `spec` describes, running or being started on a task of its own, so that a caller that stops waiting does not stop it starting; it is started once, started again when it has stopped, up to [`MOST_RESTARTS`] times, and one that could not be started is not tried again.
    fn starting(&self, spec: &Spec) -> tokio::task::JoinHandle<Option<Arc<Client>>> {
        let cell = {
            let mut started = self.started.lock().unwrap_or_else(|held| held.into_inner());
            let slot = started.entry(spec.id.clone()).or_default();
            let stopped = slot
                .started
                .get()
                .and_then(Option::as_ref)
                .is_some_and(|client| !client.is_running());
            if stopped && slot.restarts < MOST_RESTARTS {
                slot.restarts += 1;
                slot.started = Started::default();
            }
            slot.started.clone()
        };
        let spec = spec.clone();
        let root = self.root.clone();
        tokio::spawn(async move {
            cell.get_or_init(|| async {
                match Client::start(&spec, &root).await {
                    Ok(client) => Some(Arc::new(client)),
                    Err(reason) => {
                        eprintln!("language server unavailable: {reason}");
                        None
                    }
                }
            })
            .await
            .clone()
        })
    }
}

#[async_trait]
impl LanguageService for Servers {
    /// The file at `path` formatted by the server that reads it, when one does and it formats.
    async fn format(&self, path: &Path) -> Option<String> {
        let (client, language) = self.server_for(path).await?;
        let text = tokio::fs::read_to_string(path).await.ok()?;
        client
            .format(path, &language, &text, omc_tools::FORMAT_WAIT)
            .await
    }

    /// [`LOADING_WAIT`] when a server of one of the files at `paths` is still starting or loading the project, otherwise the usual wait.
    fn patience(&self, paths: &[PathBuf]) -> Duration {
        if paths.iter().any(|path| self.is_loading(path)) {
            LOADING_WAIT
        } else {
            omc_tools::DIAGNOSTICS_WAIT
        }
    }

    /// What the servers that read the files at `paths` say is wrong with them, waiting up to `wait` in all.
    async fn diagnostics(&self, paths: &[PathBuf], wait: Duration) -> Vec<Diagnostic> {
        let deadline = Instant::now() + wait;
        let mut found = Vec::new();
        for path in paths {
            let Ok(Some((client, language))) =
                tokio::time::timeout_at(deadline, self.server_for(path)).await
            else {
                continue;
            };
            let Ok(text) = tokio::fs::read_to_string(path).await else {
                continue;
            };
            found.extend(client.diagnostics(path, &language, &text, deadline).await);
        }
        found
    }

    /// Whether a server of the file at `path` is installed and starts.
    async fn serves(&self, path: &Path) -> Result<(), LanguageError> {
        self.server(path).await.map(|_| ())
    }

    /// Asks the server of the file at `at` for the places `lookup` finds.
    async fn locate(&self, lookup: Lookup, at: &Spot) -> Result<Vec<Location>, LanguageError> {
        let ready = self.ready(&at.path).await?;
        let position = ready.position(at);
        let (capability, method, params) = match lookup {
            Lookup::Definition => (
                "definitionProvider",
                "textDocument/definition",
                json!({"position": position}),
            ),
            Lookup::TypeDefinition => (
                "typeDefinitionProvider",
                "textDocument/typeDefinition",
                json!({"position": position}),
            ),
            Lookup::Implementation => (
                "implementationProvider",
                "textDocument/implementation",
                json!({"position": position}),
            ),
            Lookup::References => (
                "referencesProvider",
                "textDocument/references",
                json!({"position": position, "context": {"includeDeclaration": true}}),
            ),
        };
        let answer = ready.ask(capability, lookup.noun(), method, params).await?;
        Ok(navigate::locations(
            &answer,
            &mut Texts::with(&at.path, &ready.text),
        ))
    }

    /// Asks the server of the file at `at` what the symbol there is.
    async fn hover(&self, at: &Spot) -> Result<Option<String>, LanguageError> {
        let ready = self.ready(&at.path).await?;
        let params = json!({"position": ready.position(at)});
        let answer = ready
            .ask("hoverProvider", "hover", "textDocument/hover", params)
            .await?;
        Ok(navigate::hover(&answer))
    }

    /// Asks the server of the file at `path` for its symbols.
    async fn document_symbols(&self, path: &Path) -> Result<Vec<Symbol>, LanguageError> {
        let ready = self.ready(path).await?;
        let answer = ready
            .ask(
                "documentSymbolProvider",
                "document symbol",
                "textDocument/documentSymbol",
                json!({}),
            )
            .await?;
        Ok(navigate::document_symbols(
            &answer,
            path,
            &mut Texts::with(path, &ready.text),
        ))
    }

    /// Asks the server of the file at `near`, or every server of the project, for the symbols matching `query`.
    async fn workspace_symbols(
        &self,
        query: &str,
        near: Option<&Path>,
    ) -> Result<Vec<Symbol>, LanguageError> {
        let servers = match near {
            Some(path) => {
                let ready = self.ready(path).await?;
                vec![(ready.id, ready.client)]
            }
            None => self.project().await,
        };
        if servers.is_empty() {
            return Err(LanguageError::NoServer(
                "no language server runs for this project; name a file with path to start the one of its language".to_owned(),
            ));
        }
        let mut texts = Texts::default();
        let mut found = Vec::new();
        let mut failures = Vec::new();
        for (id, client) in servers {
            if !client.can("workspaceSymbolProvider") {
                failures.push(format!(
                    "the {id} language server does not search the project's symbols"
                ));
                continue;
            }
            client.settle(Instant::now() + LOADING_WAIT, false).await;
            match client
                .ask("workspace/symbol", json!({"query": query}), QUESTION_WAIT)
                .await
            {
                Ok(answer) => found.extend(navigate::workspace_symbols(&answer, &mut texts)),
                Err(reason) => failures.push(format!("{id}: {reason}")),
            }
        }
        if found.is_empty() && !failures.is_empty() {
            return Err(LanguageError::Failed(failures.join("; ")));
        }
        Ok(found)
    }

    /// Asks the server of the file at `at` how to rename the symbol there, and makes its edits to the texts on disk without writing them.
    async fn rename(&self, at: &Spot, new_name: &str) -> Result<Vec<Renamed>, LanguageError> {
        let ready = self.ready(&at.path).await?;
        let params = json!({"position": ready.position(at), "newName": new_name});
        let answer = ready
            .ask("renameProvider", "rename", "textDocument/rename", params)
            .await?;
        if answer.is_null() {
            return Err(LanguageError::Failed(
                "the server found no symbol to rename there".to_owned(),
            ));
        }
        navigate::renamed(&answer).map_err(LanguageError::Failed)
    }

    /// The whole text of the file at `path` formatted by its server, or with only the stretch `range` formatted.
    async fn reformat(
        &self,
        path: &Path,
        range: Option<&Location>,
    ) -> Result<Option<String>, LanguageError> {
        let ready = self.ready(path).await?;
        let Some(range) = range else {
            if !ready.client.formats() {
                return Err(LanguageError::Unsupported(format!(
                    "the {} language server does not format files",
                    ready.id
                )));
            }
            return Ok(self.format(path).await);
        };
        let language = self
            .specs
            .iter()
            .find_map(|spec| spec.language_of(path))
            .unwrap_or_default();
        ready
            .client
            .format_range(path, language, &ready.text, ready.range(range), QUESTION_WAIT)
            .await
            .map_err(|reason| {
                if ready.client.can("documentRangeFormattingProvider") {
                    LanguageError::Failed(reason)
                } else {
                    LanguageError::Unsupported(format!(
                        "the {} language server does not format stretches of files; format the whole file instead",
                        ready.id
                    ))
                }
            })
    }

    /// Asks the server of the file `range` names which code actions it offers there.
    async fn code_actions(
        &self,
        range: &Location,
        only: &[String],
    ) -> Result<Vec<CodeAction>, LanguageError> {
        let ready = self.ready(&range.path).await?;
        let offered = ready.code_actions(range, only).await?;
        Ok(offered.iter().filter_map(navigate::code_action).collect())
    }

    /// Asks the server of the file `range` names for the code actions offered there again, resolves the chosen one when the server needs that, has the server carry out its command when it has one the server runs, and makes the edits of both to the texts on disk without writing them.
    async fn apply_code_action(
        &self,
        range: &Location,
        only: &[String],
        chosen: &Chosen,
    ) -> Result<Applied, LanguageError> {
        let ready = self.ready(&range.path).await?;
        let offered = ready.code_actions(range, only).await?;
        let mut action = navigate::choose(&offered, chosen.index, chosen.title.as_deref())
            .map_err(LanguageError::Failed)?
            .clone();
        let title = action["title"].as_str().unwrap_or_default().to_owned();
        if let Some(reason) = action["disabled"]["reason"].as_str() {
            return Err(LanguageError::Failed(format!(
                "the code action \"{title}\" cannot be applied now: {reason}"
            )));
        }
        let bare_command = action["command"].is_string();
        let resolves = ready
            .client
            .capabilities_say("codeActionProvider", "resolveProvider");
        if !bare_command && action.get("edit").is_none() && resolves {
            action = ready
                .client
                .ask("codeAction/resolve", action, QUESTION_WAIT)
                .await
                .map_err(LanguageError::Failed)?;
        }
        let mut edits: Vec<Value> = action.get("edit").cloned().into_iter().collect();
        let command = if bare_command {
            Some(action.clone())
        } else {
            action.get("command").cloned()
        };
        if let Some(command) = command {
            let name = command["command"].as_str().unwrap_or_default();
            if ready.client.runs(name) {
                edits.extend(
                    ready
                        .client
                        .run_command(&command, QUESTION_WAIT)
                        .await
                        .map_err(LanguageError::Failed)?,
                );
            } else if edits.is_empty() {
                return Err(LanguageError::Unsupported(format!(
                    "the code action \"{title}\" only runs the editor command {name}, which the {} language server does not carry out itself",
                    ready.id
                )));
            }
        }
        let files = navigate::edited(&edits, "code action").map_err(LanguageError::Failed)?;
        Ok(Applied { title, files })
    }

    /// Asks the server of the file at `from`, and every other running server of the project, for the edits moving it to `to` makes.
    async fn rename_file(&self, from: &Path, to: &Path) -> Result<Vec<Renamed>, LanguageError> {
        let ready = self.ready(from).await?;
        let mut servers = vec![ready.client.clone()];
        for (_, client) in self.project().await {
            if !servers.iter().any(|held| Arc::ptr_eq(held, &client)) {
                servers.push(client);
            }
        }
        let mut edits = Vec::new();
        for client in servers {
            if let Some(edit) = client
                .will_rename(from, to, QUESTION_WAIT)
                .await
                .map_err(LanguageError::Failed)?
            {
                edits.push(edit);
            }
        }
        navigate::edited(&edits, "move").map_err(LanguageError::Failed)
    }

    /// Tells every running server that the file at `from` is now at `to`.
    async fn file_renamed(&self, from: &Path, to: &Path) {
        let running: Vec<Arc<Client>> = {
            let started = self.started.lock().unwrap_or_else(|held| held.into_inner());
            started
                .values()
                .filter_map(|slot| slot.started.get().cloned().flatten())
                .collect()
        };
        for client in running {
            client.did_rename(from, to).await;
        }
    }

    /// Every server the harness knows, and how each stands.
    async fn status(&self) -> Result<Vec<ServerStatus>, LanguageError> {
        Ok(self.specs.iter().map(|spec| self.status_of(spec)).collect())
    }

    /// Restarts the server named `server`, or the one that reads the file at `near`, or every server tried so far.
    async fn reload(
        &self,
        server: Option<&str>,
        near: Option<&Path>,
    ) -> Result<Vec<ServerStatus>, LanguageError> {
        let tried: Vec<String> = self
            .started
            .lock()
            .unwrap_or_else(|held| held.into_inner())
            .keys()
            .cloned()
            .collect();
        let chosen: Vec<&Spec> = self
            .specs
            .iter()
            .filter(|spec| match (server, near) {
                (Some(name), _) => spec.id.eq_ignore_ascii_case(name),
                (None, Some(path)) => spec.language_of(path).is_some(),
                (None, None) => tried.contains(&spec.id),
            })
            .collect();
        if chosen.is_empty() {
            let known: Vec<&str> = self.specs.iter().map(|spec| spec.id.as_str()).collect();
            return Err(LanguageError::NoServer(match (server, near) {
                (Some(name), _) => format!(
                    "no language server is named {name}; the known ones are {}",
                    known.join(", ")
                ),
                (None, Some(path)) => format!("no language server reads {}", path.display()),
                (None, None) => {
                    "no language server has been started in this conversation".to_owned()
                }
            }));
        }
        let mut restarted = Vec::with_capacity(chosen.len());
        for spec in chosen {
            if !spec.is_installed() {
                restarted.push(self.status_of(spec));
                continue;
            }
            restarted.push(self.restart(spec).await);
        }
        Ok(restarted)
    }

    /// Everything the servers of the project have said is wrong, once they have finished loading and checking it or `wait` has passed.
    async fn all_diagnostics(&self, wait: Duration) -> Result<Vec<Diagnostic>, LanguageError> {
        let servers = self.project().await;
        if servers.is_empty() {
            return Err(LanguageError::NoServer(
                "no language server runs for this project; name a file with path to check it with the server of its language".to_owned(),
            ));
        }
        let deadline = Instant::now() + wait;
        let mut found = Vec::new();
        for (_, client) in servers {
            client.settle(deadline, true).await;
            for (uri, said) in client.published() {
                let Some(path) = path_of(&uri) else {
                    continue;
                };
                let text = tokio::fs::read_to_string(&path).await.unwrap_or_default();
                found.extend(said.iter().filter_map(|raw| diagnostic(raw, &path, &text)));
            }
        }
        Ok(found)
    }
}

#[cfg(all(test, unix))]
/// Tests for starting servers, whose stand-in server is a POSIX shell script.
mod tests {
    use super::*;

    /// A server, run by the shell, that answers the starting request, stays up for a moment and then exits.
    const SHORT_LIVED: &str = r#"R='{"jsonrpc":"2.0","id":1,"result":{"capabilities":{}}}'; head -c 1 >/dev/null; printf 'Content-Length: %d\r\n\r\n%s' "${#R}" "$R"; sleep 1"#;

    /// Waits until `client` has stopped, for up to ten seconds.
    async fn stopped(client: &Client) {
        let until = Instant::now() + Duration::from_secs(10);
        while client.is_running() {
            assert!(Instant::now() < until, "the server did not stop");
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    /// A server that stopped is started again on the next question, until it has stopped too many times.
    #[tokio::test]
    async fn a_stopped_server_is_started_again() {
        let spec = Spec::new("short", &[("x", "x")], &[&["sh", "-c", SHORT_LIVED]]);
        let servers = Servers::with_specs(".", vec![spec]);
        let file = Path::new("a.x");
        let (mut client, _) = servers.server_for(file).await.expect("the server starts");
        for _ in 0..MOST_RESTARTS {
            stopped(&client).await;
            let (again, _) = servers.server_for(file).await.expect("it starts again");
            assert!(!Arc::ptr_eq(&again, &client));
            client = again;
        }
        stopped(&client).await;
        let (last, _) = servers
            .server_for(file)
            .await
            .expect("the last one is kept");
        assert!(Arc::ptr_eq(&last, &client));
    }

    /// A server not started yet, or started and silent, is waited for longer; files no server reads, or whose server is missing, are not.
    #[tokio::test]
    async fn a_loading_server_is_waited_for_longer() {
        let short = Spec::new("short", &[("x", "x")], &[&["sh", "-c", SHORT_LIVED]]);
        let missing = Spec::new("missing", &[("y", "y")], &[&["no-such-language-server"]]);
        let servers = Servers::with_specs(".", vec![short, missing]);
        let file = PathBuf::from("a.x");
        assert_eq!(servers.patience(std::slice::from_ref(&file)), LOADING_WAIT);
        servers.server_for(&file).await.expect("the server starts");
        assert_eq!(servers.patience(std::slice::from_ref(&file)), LOADING_WAIT);
        let others = [PathBuf::from("a.y"), PathBuf::from("a.z")];
        assert_eq!(servers.patience(&others), omc_tools::DIAGNOSTICS_WAIT);
    }
}
