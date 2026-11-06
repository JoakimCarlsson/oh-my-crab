//! One conversation: the commands the client sends, and the turns they start.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use omc_agent::Journal;
use omc_auth::{AuthError, LoginPrompt};
use omc_context::{Discovered, Locations, discover, expand_skill};
use omc_providers::Message;
use omc_providers::Provider;
use omc_tools::{LanguageService, Registry, Skill};
use omc_wire::{Command, Event, Opening, Slash};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use tokio::task::AbortHandle;

use crate::prompt::compose;
use crate::settings::{CHECKPOINTS, EFFORT, MODE, MODEL, Mode, OFF, ON};
use crate::shared::Shared;
use crate::turn::{Ended, Job, Turn};

/// A conversation that has been opened.
struct Open {
    /// The provider that serves it.
    provider: Provider,
    /// The built-in tools, tool-server tools and the skill tool, confined to the folder it works in.
    base: Arc<Registry>,
    /// The names of the tools the agent is not given.
    denied: Arc<Vec<String>>,
    /// What was found on disk.
    discovered: Arc<Discovered>,
    /// Where the work is.
    locations: Arc<Locations>,
    /// The instruction files, skills and agents the reader turned off.
    disabled: Vec<String>,
}

/// The state of one conversation.
struct Session {
    /// What commands, turns and tools share.
    shared: Arc<Shared>,
    /// What was said so far.
    history: Vec<Message>,
    /// The file the history is saved in, when the conversation is saved.
    journal: Option<Arc<Journal>>,
    /// The conversation once it is opened.
    open: Option<Open>,
    /// The turn that is running and its number, when one is.
    running: Option<(u64, AbortHandle)>,
    /// The number the next turn gets.
    turns: u64,
    /// How many fresh context windows had been started when the running turn began.
    resets_at_start: u64,
    /// Where finished turns report.
    ended: UnboundedSender<(u64, Ended)>,
}

/// What a conversation is run with besides its commands.
#[derive(Clone, Default)]
pub struct Options {
    /// The language services `edit` and `write` ask, whatever the client offers; when there is none, they ask the client's when it offers them, and otherwise the harness's own servers when it was built with them.
    pub languages: Option<Arc<dyn LanguageService>>,
}

/// Runs a conversation: reads `commands` and reports what happens as events on `out`, until the client closes it or hangs up.
pub async fn run(commands: UnboundedReceiver<Command>, out: UnboundedSender<Event>) {
    run_with(commands, out, Options::default()).await;
}

/// Runs a conversation like [`run`], with what `options` gives it.
pub async fn run_with(
    mut commands: UnboundedReceiver<Command>,
    out: UnboundedSender<Event>,
    options: Options,
) {
    let (ended, mut finished) = unbounded_channel();
    let mut session = Session {
        shared: Shared::with_languages(out, options.languages),
        history: Vec::new(),
        journal: None,
        open: None,
        running: None,
        turns: 0,
        resets_at_start: 0,
        ended,
    };
    loop {
        tokio::select! {
            command = commands.recv() => match command {
                None | Some(Command::Close) => break,
                Some(command) => session.handle(command).await,
            },
            Some((number, how)) = finished.recv() => session.finish(number, how),
        }
    }
    session.stop();
}

impl Session {
    /// Carries out one command.
    async fn handle(&mut self, command: Command) {
        match command {
            Command::Open(opening) => self.open(opening).await,
            Command::Prompt {
                text,
                files,
                turn,
                context,
            } => {
                if let Some(focus) = compact_request(&text) {
                    if !files.is_empty() {
                        return self.say(Event::Failed(
                            "/compact takes no files; send them with a prompt instead".to_owned(),
                        ));
                    }
                    let mark = turn.map(|id| omc_agent::Mark {
                        id,
                        text: text.clone(),
                        files: Vec::new(),
                    });
                    return self.start(Job::Compact(focus), mark);
                }
                let names = files
                    .iter()
                    .filter_map(|file| file.file_name())
                    .map(|name| name.to_string_lossy().into_owned())
                    .collect();
                let mark = turn.map(|id| omc_agent::Mark {
                    id,
                    text: text.clone(),
                    files: names,
                });
                let told = format!("{}{}", context.unwrap_or_default(), compose(&text, &files));
                self.start(Job::Prompt(told), mark);
            }
            Command::Choose { ticket, option } => {
                self.shared.questions.answer(ticket, option.as_deref());
            }
            Command::Answer { ticket, replies } => self.shared.questions.reply(ticket, replies),
            Command::Rename { title } => {
                let title = title.split_whitespace().collect::<Vec<_>>().join(" ");
                if !title.is_empty() {
                    self.shared.name(&title, false);
                }
            }
            Command::Cancel => self.cancel(),
            Command::Set { setting, value } => self.set(&setting, &value),
            Command::Login { provider } => self.login(&provider),
            Command::Language { ticket, answer } => {
                self.shared.client_languages.answer(ticket, answer);
            }
            Command::Close => {}
        }
    }

    /// Sends `event` to the client.
    fn say(&self, event: Event) {
        self.shared.say(event);
    }

    /// Opens the conversation: finds the provider, the tools, the skills and the agents, and says what can be set.
    async fn open(&mut self, opening: Opening) {
        let Opening {
            directory,
            home,
            provider,
            model,
            yolo,
            tools: servers,
            denied,
            disabled,
            conversation,
            history,
            keep,
            before,
            defaults,
            languages,
        } = opening;
        let provider = provider.as_str();
        self.shared.set_home(home);
        let Some(provider) = Provider::from_id(provider) else {
            return self.say(Event::Failed(format!("unknown provider: {provider}")));
        };
        self.journal = None;
        self.shared.jobs.stop_all();
        let saved = match conversation {
            Some(id) => match load_history(&id, history, keep, before.as_deref()) {
                Ok(saved) => Some(saved),
                Err(reason) => return self.say(Event::Failed(reason)),
            },
            None => None,
        };
        self.shared.languages.set(None);
        let mut registry = match Registry::builtin_in(
            &directory,
            Some(self.shared.jobs.clone()),
            self.shared.languages.clone(),
        ) {
            Ok(registry) => registry,
            Err(error) => return self.say(Event::Failed(error.to_string())),
        };
        let (tools, unreachable) = omc_mcp::tools(&servers).await;
        for tool in tools {
            registry.register_shared(tool);
        }
        for reason in unreachable {
            eprintln!("tool server unavailable: {reason}");
        }
        let locations = Locations::detect(&directory);
        let discovered = discover(&locations, &disabled);
        if !discovered.skills.is_empty() {
            registry.register(Skill::new(discovered.skills.clone()));
        }
        let credentials = match self
            .shared
            .with_home(omc_providers::credentials_for(provider))
            .await
        {
            Ok(credentials) => credentials,
            Err(AuthError::NotLoggedIn) => {
                return self.say(Event::SignedOut {
                    reason: "not logged in".to_owned(),
                    account: None,
                });
            }
            Err(error) => return self.say(Event::Failed(error.to_string())),
        };
        let models = omc_providers::list_models(provider, &credentials)
            .await
            .unwrap_or_default();
        let has_model = model.is_some();
        let model = match model.or_else(|| models.first().cloned()) {
            Some(model) => model,
            None => match omc_providers::default_model(provider, &credentials).await {
                Ok(model) => model,
                Err(error) => return self.say(Event::Failed(error.to_string())),
            },
        };
        let commands = commands_of(&discovered);
        let commands_value = serde_json::to_value(&commands).unwrap_or_default();
        let (journal, messages) = saved.unzip();
        self.shared
            .notebook
            .load(journal.as_ref().map_or("", |journal| journal.notes()));
        self.shared.checkpoint.clear();
        self.history = messages.unwrap_or_default();
        self.journal = journal;
        self.shared.attach(self.journal.clone());
        self.open = Some(Open {
            provider,
            base: Arc::new(registry),
            denied: Arc::new(denied),
            discovered: Arc::new(discovered),
            locations: Arc::new(locations),
            disabled,
        });
        let mode = if yolo { Mode::Yolo } else { Mode::Asking };
        self.shared.open(provider, models, model, mode);
        let chosen = self
            .journal
            .as_ref()
            .map(|journal| journal.settings())
            .unwrap_or_default();
        let wanted = restored_settings(&defaults, &chosen);
        for (id, value) in &wanted {
            if id != MODEL || !has_model {
                self.apply(id, value);
            }
        }
        let trusted = self.shared.mode() == Mode::Yolo;
        self.shared
            .languages
            .set(self.languages_for(&directory, languages, trusted));
        self.shared.announce();
        self.shared.save_fact("commands", &commands_value);
        self.say(Event::Commands(commands));
        if self.journal.is_some() {
            if let Some(title) = self.shared.title() {
                self.say(Event::Titled(title));
            }
            self.say_history();
        }
    }

    /// The language services for a conversation in `directory`: the ones the program running it gave it, the client's when `client_offers` them, or the harness's own servers, started early when the agent is `trusted` with everything.
    fn languages_for(
        &self,
        directory: &Path,
        client_offers: bool,
        trusted: bool,
    ) -> Option<Arc<dyn LanguageService>> {
        if let Some(given) = &self.shared.given_languages {
            return Some(given.clone());
        }
        if client_offers {
            let client: Arc<dyn LanguageService> = self.shared.client_languages.clone();
            return Some(client);
        }
        crate::languages::own_servers(directory, trusted)
    }

    /// The value saved under `key`, or what `T` is by default when nothing readable is saved there.
    fn fact<T: serde::de::DeserializeOwned + Default>(&self, key: &str) -> T {
        self.journal
            .as_ref()
            .and_then(|journal| journal.fact(key))
            .and_then(|value| serde_json::from_value(value).ok())
            .unwrap_or_default()
    }

    /// Tells the client the conversation as it was saved, as the turns it holds.
    fn say_history(&self) {
        let (Some(open), Some((provider, model))) = (&self.open, self.shared.model()) else {
            return;
        };
        let window = omc_providers::context_window(provider, &model);
        let marks = self
            .journal
            .as_ref()
            .map(|journal| journal.marks())
            .unwrap_or_default();
        let lost = self
            .journal
            .as_ref()
            .map(|journal| journal.lost())
            .unwrap_or_default();
        self.say(Event::Resumed(omc_wire::Conversation {
            title: self.shared.title(),
            settings: self
                .journal
                .as_ref()
                .map(|journal| journal.settings())
                .unwrap_or_default(),
            turns: crate::translate::replay(
                &self.history,
                &marks,
                &lost,
                &self
                    .journal
                    .as_ref()
                    .map(|journal| journal.changes())
                    .unwrap_or_default(),
                &open.base,
                Some(window),
            ),
            offered: self.fact("settings"),
            commands: self.fact("commands"),
            window: Some(window),
        }));
    }

    /// Starts a turn doing `job`, whose prompt the client described as `mark` when it did, unless one is running or nothing is open.
    fn start(&mut self, job: Job, mark: Option<omc_agent::Mark>) {
        let (true, Some((_, model))) = (self.open.is_some(), self.shared.model()) else {
            return self.say(Event::Failed("the conversation is not open".to_owned()));
        };
        if self.running.is_some() {
            return;
        }
        if let (Some(journal), Some(mark)) = (&self.journal, &mark)
            && let Err(error) = journal.mark(self.history.len(), mark)
        {
            return self.say(Event::Failed(format!(
                "cannot save the conversation: {error}"
            )));
        }
        self.rescan();
        let Some(open) = &self.open else {
            return;
        };
        self.turns += 1;
        let number = self.turns;
        self.resets_at_start = self.shared.notebook.resets();
        let job = match job {
            Job::Prompt(prompt) => Job::Prompt(expand_skill(&prompt, &open.discovered.skills)),
            compact => compact,
        };
        let turn = Turn {
            provider: open.provider,
            model,
            base: open.base.clone(),
            denied: open.denied.clone(),
            discovered: open.discovered.clone(),
            locations: open.locations.clone(),
            shared: self.shared.clone(),
            history: self.history.clone(),
            scribe: self.journal.as_ref().map(Journal::scribe),
        };
        let ended = self.ended.clone();
        let task = tokio::spawn(async move {
            let how = turn.run(job).await;
            let _ = ended.send((number, how));
        });
        self.running = Some((number, task.abort_handle()));
    }

    /// Reads what is on disk again, so that a skill added or removed since the last turn is known to this one; the client is told the commands again when the skills changed.
    ///
    /// The skill tool is rebuilt from the new list only when the list differs, which keeps the common turn to one scan of a few folders.
    fn rescan(&mut self) {
        let Some(open) = &mut self.open else {
            return;
        };
        let found = discover(&open.locations, &open.disabled);
        let changed = found.skills != open.discovered.skills;
        if changed {
            let mut registry = open.base.filtered(|tool| tool.name() != SKILL_TOOL);
            if !found.skills.is_empty() {
                registry.register(Skill::new(found.skills.clone()));
            }
            open.base = Arc::new(registry);
        }
        open.discovered = Arc::new(found);
        if changed {
            let commands = commands_of(&open.discovered);
            self.say(Event::Commands(commands));
        }
    }

    /// Takes in the end of turn `number`; a turn that was cancelled meanwhile is ignored.
    fn finish(&mut self, number: u64, how: Ended) {
        if self.running.as_ref().map(|(running, _)| *running) != Some(number) {
            return;
        }
        self.running = None;
        match how {
            Ended::Done(history) => {
                self.history = history;
                self.say(Event::Finished(None));
            }
            Ended::SignedOut(reason) => self.say(Event::SignedOut {
                reason,
                account: None,
            }),
            Ended::Failed(reason) => {
                self.shared.checkpoint.clear();
                self.keep_fresh_window();
                self.say(Event::Finished(Some(reason)));
            }
        }
    }

    /// Stops the turn that is running, subagents included, and declines what it waited on.
    ///
    /// The turn's task owns everything it started, so aborting it ends them all; its history is never taken in.
    fn cancel(&mut self) {
        self.shared.questions.decline_all();
        if let Some((_, task)) = self.running.take() {
            task.abort();
            self.forget_turn();
            self.say(Event::Halted);
            self.say(Event::Finished(None));
        }
    }

    /// Ends whatever is running, for good, background runs included.
    fn stop(&mut self) {
        self.shared.questions.decline_all();
        self.shared.jobs.stop_all();
        if let Some((_, task)) = self.running.take() {
            task.abort();
            self.forget_turn();
        }
    }

    /// Records in the saved history that the turn that was stopped never happened.
    fn forget_turn(&mut self) {
        self.shared.checkpoint.clear();
        self.keep_fresh_window();
        if let Some(Err(error)) = self
            .journal
            .as_ref()
            .map(|j| j.cut_losing(self.history.len(), omc_agent::STOPPED))
        {
            eprintln!("history not saved: {error}");
        }
        if let Some(Err(error)) = self
            .journal
            .as_ref()
            .map(|j| j.save_notes(&self.shared.notebook.read()))
        {
            eprintln!("notes not saved: {error}");
        }
    }

    /// When the turn that ended without an answer had started a fresh context window, the old history is gone for good, so the conversation goes on from the notes message that window began with.
    fn keep_fresh_window(&mut self) {
        if self.shared.notebook.resets() == self.resets_at_start {
            return;
        }
        if let Some(opening) = self.shared.notebook.opening() {
            self.history = vec![Message::User(opening)];
        }
    }

    /// Gives the setting `setting` the value `value` and saves the choice with the conversation; the new value applies from the next request.
    fn set(&mut self, setting: &str, value: &str) {
        if self.apply(setting, value) {
            self.shared.save_setting(setting, value);
            self.shared.announce();
        }
    }

    /// Gives the setting `setting` the value `value` without saving it; whether the setting took it.
    fn apply(&mut self, setting: &str, value: &str) -> bool {
        match setting {
            MODEL if self.open.is_some() => {
                self.shared.set_model(value);
                true
            }
            MODE => Mode::parse(value).is_some_and(|mode| {
                self.shared.set_mode(mode);
                true
            }),
            EFFORT => self.shared.set_effort(value),
            CHECKPOINTS => match value {
                ON | OFF => {
                    self.shared.set_checkpoints(value == ON);
                    true
                }
                _ => false,
            },
            _ => false,
        }
    }

    /// Signs in to `provider` in the background, reporting the address to open and how it ended.
    fn login(&self, provider: &str) {
        let Some(provider) = Provider::from_id(provider) else {
            return self.say(Event::Failed(format!("unknown provider: {provider}")));
        };
        let out = self.shared.out.clone();
        let shared = self.shared.clone();
        tokio::spawn(async move {
            let address = out.clone();
            let outcome = shared
                .with_home(omc_providers::login(provider, move |prompt| {
                    let _ = address.send(match prompt {
                        LoginPrompt::Url(url) => Event::LoginUrl(url),
                        LoginPrompt::Code { url, code } => Event::LoginCode { url, code },
                    });
                }))
                .await;
            let _ = out.send(match outcome {
                Ok(()) => Event::SignedIn,
                Err(error) => Event::Failed(error.to_string()),
            });
        });
    }
}

/// The names Windows keeps for devices, which cannot be file names there whatever the extension.
const RESERVED_NAMES: [&str; 22] = [
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

/// The file the saved history of the conversation `id` is kept in, inside `folder` or a folder under the harness's own data folder.
///
/// # Errors
///
/// Returns what to tell the client when `id` is not usable as a file name or there is no data folder.
fn history_file(id: &str, folder: Option<PathBuf>) -> Result<PathBuf, String> {
    let usable = !id.is_empty()
        && !id.starts_with('.')
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
    let device = id
        .split('.')
        .next()
        .is_some_and(|stem| RESERVED_NAMES.contains(&stem.to_ascii_uppercase().as_str()));
    if !usable || device {
        return Err(format!("unusable conversation id: {id}"));
    }
    let folder = match folder {
        Some(folder) => folder,
        None => dirs::data_dir()
            .ok_or_else(|| "no data folder available".to_owned())?
            .join("oh-my-crab")
            .join("conversations"),
    };
    Ok(history_path(&folder, id))
}

/// Opens the saved history of the conversation `id` in `folder`, or in a folder under the harness's own data folder, keeping `keep` turns when that is given and cutting it back to before the turn `before` when that is.
///
/// # Errors
///
/// Returns what to tell the client when `id` is not usable as a file name, there is no data folder, or the file cannot be read.
fn load_history(
    id: &str,
    folder: Option<PathBuf>,
    keep: Option<usize>,
    before: Option<&str>,
) -> Result<(Arc<Journal>, Vec<Message>), String> {
    Journal::open_before(&history_file(id, folder)?, keep, before)
        .map_err(|error| format!("cannot open the conversation: {error}"))
}

/// The saved conversation `id` in `folder`: its name, the choices made about it and its turns, as the events a live conversation would have sent for them, read without opening the conversation, so that it works while the conversation is open elsewhere; an empty conversation when nothing was saved. The context size is left out, since it depends on a model.
///
/// # Errors
///
/// Returns what to tell the client when `id` is not usable, the file cannot be read, or the built-in tools cannot be made.
pub fn history(id: &str, folder: Option<PathBuf>) -> Result<omc_wire::Conversation, String> {
    let path = history_file(id, folder)?;
    if !path.exists() {
        return Ok(omc_wire::Conversation::default());
    }
    let saved = Journal::read(&path).map_err(|error| error.to_string())?;
    let directory = std::env::current_dir().map_err(|error| error.to_string())?;
    let registry = Registry::builtin(&directory).map_err(|error| error.to_string())?;
    let (offered, commands, window) = (
        saved.fact("settings"),
        saved.fact("commands"),
        saved.window(),
    );
    Ok(omc_wire::Conversation {
        title: saved.title,
        settings: saved.settings,
        turns: crate::translate::replay(
            &saved.history,
            &saved.marks,
            &saved.lost,
            &saved.changes,
            &registry,
            window,
        ),
        offered,
        commands,
        window,
    })
}

/// Names the saved conversation `id` in `folder` `title`, which works on a conversation nobody has open.
///
/// # Errors
///
/// Returns what to tell the client when `id` is not usable, the conversation is open elsewhere, or the name cannot be written.
pub fn rename(id: &str, folder: Option<PathBuf>, title: &str) -> Result<(), String> {
    let path = history_file(id, folder)?;
    Journal::rename(&path, title).map_err(|error| error.to_string())
}

/// Saves a conversation a client kept itself as the saved conversation `id` in `folder`: its turns as the reader's prompts and the agent's replies, without the tools the agent used, its name, its settings and what was offered.
///
/// # Errors
///
/// Returns what to tell the client when `id` is not usable, the conversation already holds a history or is open elsewhere, or it cannot be written.
pub fn import(
    id: &str,
    folder: Option<PathBuf>,
    imported: &omc_wire::Imported,
) -> Result<(), String> {
    let path = history_file(id, folder)?;
    let (journal, existing) = Journal::open(&path, None)
        .map_err(|error| format!("cannot open the conversation: {error}"))?;
    if !existing.is_empty() || !journal.marks().is_empty() {
        return Err("the conversation already has a history".to_owned());
    }
    let failed = |error: std::io::Error| format!("cannot save the conversation: {error}");
    let mut length = 0;
    for turn in &imported.turns {
        let scribe = journal.scribe();
        let mark = omc_agent::Mark {
            id: turn.id.clone(),
            text: turn.prompt.clone(),
            files: turn.files.clone(),
        };
        journal.mark(length, &mark).map_err(failed)?;
        scribe
            .append(&Message::User(turn.prompt.clone()))
            .map_err(failed)?;
        match &turn.end {
            None => {
                scribe
                    .append(&Message::Assistant(omc_providers::Reply {
                        text: turn.said.clone(),
                        ..omc_providers::Reply::default()
                    }))
                    .map_err(failed)?;
                length += 2;
            }
            Some(reason) => scribe.cut_losing(length, reason).map_err(failed)?,
        }
    }
    for (setting, value) in &imported.settings {
        journal.save_setting(setting, value).map_err(failed)?;
    }
    let facts = [
        ("settings", serde_json::to_value(&imported.offered)),
        ("commands", serde_json::to_value(&imported.commands)),
        ("window", serde_json::to_value(imported.window)),
    ];
    for (key, value) in facts {
        let value = value.map_err(std::io::Error::other).map_err(failed)?;
        if !value.is_null() && value != serde_json::json!([]) {
            journal.save_fact(key, &value).map_err(failed)?;
        }
    }
    if let Some(title) = imported.title.as_deref().filter(|title| !title.is_empty()) {
        journal.save_title(title).map_err(failed)?;
    }
    Ok(())
}

/// The file the history of conversation `id` is kept in, inside `folder`.
fn history_path(folder: &Path, id: &str) -> PathBuf {
    folder.join(format!("{id}.jsonl"))
}

/// The settings to apply when a conversation opens: the `defaults` for those the saved conversation has no choice for, then what was `chosen`, with the model before the effort that depends on it.
fn restored_settings(
    defaults: &[(String, String)],
    chosen: &[(String, String)],
) -> Vec<(String, String)> {
    let mut wanted: Vec<(String, String)> = defaults
        .iter()
        .filter(|(id, _)| !chosen.iter().any(|(saved, _)| saved == id))
        .cloned()
        .collect();
    wanted.extend(chosen.iter().cloned());
    wanted.sort_by_key(|(id, _)| match id.as_str() {
        MODEL => 0,
        EFFORT => 2,
        _ => 1,
    });
    wanted
}

/// The name the skill tool is registered under.
const SKILL_TOOL: &str = "skill";

/// The command that compacts the conversation.
const COMPACT_COMMAND: &str = "compact";

/// The commands the client offers behind a slash for what was `discovered`: compacting the conversation, then one for each skill.
fn commands_of(discovered: &Discovered) -> Vec<Slash> {
    let compact = Slash {
        name: COMPACT_COMMAND.to_owned(),
        description: "Summarize the older part of the conversation to free up context; text after the command says what the summary should keep in particular".to_owned(),
    };
    std::iter::once(compact)
        .chain(discovered.skills.iter().map(|skill| Slash {
            name: skill.name.clone(),
            description: skill.description.clone(),
        }))
        .collect()
}

/// What to give particular attention to when `text` asks for the conversation to be compacted, as `/compact` followed by an optional focus; nothing when it asks for something else.
fn compact_request(text: &str) -> Option<Option<String>> {
    let rest = text.trim().strip_prefix('/')?;
    let (name, focus) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
    (name == COMPACT_COMMAND)
        .then(|| Some(focus.trim().to_owned()).filter(|focus| !focus.is_empty()))
}

/// Tests for the conversation's re-reading of what is on disk.
#[cfg(test)]
mod tests {
    use super::*;

    /// Writes a skill called `name` under `directory/.claude/skills`.
    fn add_skill(directory: &std::path::Path, name: &str) {
        let folder = directory.join(".claude/skills").join(name);
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(
            folder.join("SKILL.md"),
            format!("---\nname: {name}\ndescription: The {name} skill\n---\nDo it.\n"),
        )
        .unwrap();
    }

    /// A skill added after the conversation opened is announced as a command and loaded by the next prompt, and an unchanged scan announces nothing.
    #[tokio::test]
    async fn rescan_announces_a_skill_added_since() {
        let directory = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../build/tests/omc-session/rescan");
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory).unwrap();
        let locations = Locations {
            directory: directory.clone(),
            root: Some(directory.clone()),
            home: None,
            claude: None,
            codex: None,
        };
        let (out, mut events) = unbounded_channel();
        let (ended, _finished) = unbounded_channel();
        let discovered = discover(&locations, &[]);
        let mut session = Session {
            shared: Shared::new(out),
            history: Vec::new(),
            journal: None,
            open: Some(Open {
                provider: Provider::Claude,
                base: Arc::new(Registry::builtin(&directory).unwrap()),
                denied: Arc::new(Vec::new()),
                discovered: Arc::new(discovered),
                locations: Arc::new(locations),
                disabled: Vec::new(),
            }),
            running: None,
            turns: 0,
            resets_at_start: 0,
            ended,
        };
        session.rescan();
        assert!(events.try_recv().is_err());
        add_skill(&directory, "fresh");
        session.rescan();
        let Ok(Event::Commands(commands)) = events.try_recv() else {
            panic!("the new skill was not announced");
        };
        let names: Vec<&str> = commands.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, vec![COMPACT_COMMAND, "fresh"]);
        let open = session.open.as_ref().unwrap();
        assert!(open.base.get("skill").is_some());
        session.rescan();
        assert!(events.try_recv().is_err());
        std::fs::remove_dir_all(&directory).unwrap();
    }

    /// A client that offers language services answers over the wire: `write` asks it to format the file and then what is wrong with it, each as a JSON line, and its answers, sent back as commands, end up in the file and in the tool's result.
    #[tokio::test]
    async fn a_client_answers_language_requests_over_the_wire() {
        let directory = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../build/tests/omc-session/languages");
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory).unwrap();
        let directory = directory.canonicalize().unwrap();
        let (out, mut events) = unbounded_channel();
        let (ended, _finished) = unbounded_channel();
        let mut session = Session {
            shared: Shared::new(out),
            history: Vec::new(),
            journal: None,
            open: None,
            running: None,
            turns: 0,
            resets_at_start: 0,
            ended,
        };
        session
            .shared
            .languages
            .set(session.languages_for(&directory, true, false));
        let registry = Arc::new(
            Registry::builtin_in(&directory, None, session.shared.languages.clone()).unwrap(),
        );
        let writing = tokio::spawn({
            let registry = registry.clone();
            async move {
                registry
                    .call_as(
                        "c1",
                        "write",
                        serde_json::json!({"path": "lib.rs", "content": "fn  x(){}\n"}),
                    )
                    .await
                    .unwrap()
            }
        });
        let file = directory.join("lib.rs");
        let mut next_request = async || {
            let event = events.recv().await.unwrap();
            let line = omc_wire::encode(&event).unwrap();
            let Event::Language(request) = omc_wire::decode::<Event>(&line).unwrap() else {
                panic!("not a language request: {line}");
            };
            request
        };
        let request = next_request().await;
        assert_eq!(
            request.ask,
            omc_wire::LanguageAsk::Format { path: file.clone() }
        );
        let answer = Command::Language {
            ticket: request.ticket,
            answer: omc_wire::LanguageAnswer::Formatted {
                text: Some("fn x() {}\n".to_owned()),
            },
        };
        session
            .handle(omc_wire::decode(&omc_wire::encode(&answer).unwrap()).unwrap())
            .await;
        let request = next_request().await;
        let omc_wire::LanguageAsk::Diagnostics { paths, wait_ms } = request.ask else {
            panic!("not a diagnostics request");
        };
        assert_eq!(paths, std::slice::from_ref(&file));
        assert_eq!(wait_ms, 3000);
        let answer = Command::Language {
            ticket: request.ticket,
            answer: omc_wire::LanguageAnswer::Diagnostics {
                diagnostics: vec![omc_wire::Diagnostic {
                    path: file.clone(),
                    line: 1,
                    column: 4,
                    severity: omc_wire::Severity::Warning,
                    message: "function `x` is never used".to_owned(),
                    source: Some("rustc".to_owned()),
                    code: Some("dead_code".to_owned()),
                }],
            },
        };
        session
            .handle(omc_wire::decode(&omc_wire::encode(&answer).unwrap()).unwrap())
            .await;
        let output = writing.await.unwrap();
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "fn x() {}\n");
        assert_eq!(
            output.text,
            format!(
                "Created lib.rs (10 bytes); formatted by the language server to 10 bytes, tag {}\n\nDiagnostics: 0 error(s), 1 warning(s)\nlib.rs:1:4 warning: function `x` is never used (rustc dead_code)",
                omc_tools::snapshot_tag(b"fn x() {}\n")
            )
        );
        std::fs::remove_dir_all(&directory).unwrap();
    }

    /// The language services a program gives the conversation win over the client's, and with neither the harness's own servers are used.
    #[cfg(feature = "lsp")]
    #[test]
    fn chooses_the_language_services() {
        let (out, _events) = unbounded_channel();
        let (ended, _finished) = unbounded_channel();
        let given: Arc<dyn LanguageService> = Arc::new(omc_lsp::Servers::new("/w"));
        let session = Session {
            shared: Shared::with_languages(out, Some(given.clone())),
            history: Vec::new(),
            journal: None,
            open: None,
            running: None,
            turns: 0,
            resets_at_start: 0,
            ended,
        };
        let chosen = session.languages_for(Path::new("/w"), true, false).unwrap();
        assert!(Arc::ptr_eq(&chosen, &given));
        let (out, _events) = unbounded_channel();
        let (ended, _finished) = unbounded_channel();
        let session = Session {
            shared: Shared::new(out),
            history: Vec::new(),
            journal: None,
            open: None,
            running: None,
            turns: 0,
            resets_at_start: 0,
            ended,
        };
        let client: Arc<dyn LanguageService> = session.shared.client_languages.clone();
        let chosen = session.languages_for(Path::new("/w"), true, false).unwrap();
        assert!(Arc::ptr_eq(&chosen, &client));
        let own = session
            .languages_for(Path::new("/w"), false, false)
            .unwrap();
        assert!(!Arc::ptr_eq(&own, &client));
    }

    /// Cancelling a running turn leaves no active checkpoint behind.
    #[tokio::test]
    async fn cancelling_a_turn_clears_the_checkpoint() {
        let (out, _events) = unbounded_channel();
        let (ended, _finished) = unbounded_channel();
        let mut session = Session {
            shared: Shared::new(out),
            history: Vec::new(),
            journal: None,
            open: None,
            running: None,
            turns: 0,
            resets_at_start: 0,
            ended,
        };
        let task = tokio::spawn(std::future::pending::<()>());
        session.running = Some((1, task.abort_handle()));
        session.shared.checkpoint.begin().unwrap();
        session.shared.checkpoint.settle(2);
        assert!(session.shared.checkpoint.active());
        session.cancel();
        assert!(!session.shared.checkpoint.active());
    }

    /// A session with nothing open, for the tests of how cancelling and stopping treat background runs.
    fn bare_session() -> Session {
        let (out, _events) = unbounded_channel();
        let (ended, _finished) = unbounded_channel();
        Session {
            shared: Shared::new(out),
            history: Vec::new(),
            journal: None,
            open: None,
            running: None,
            turns: 0,
            resets_at_start: 0,
            ended,
        }
    }

    /// Cancelling a turn leaves background runs going, and stopping the session kills them.
    #[tokio::test]
    async fn cancelling_keeps_background_runs_and_stopping_kills_them() {
        let mut session = bare_session();
        let jobs = session.shared.jobs.clone();
        jobs.spawn("slow".into(), None, async {
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            Ok("done".to_owned())
        });
        let task = tokio::spawn(std::future::pending::<()>());
        session.running = Some((1, task.abort_handle()));
        session.cancel();
        assert_eq!(jobs.wait().await.unwrap().text, "done");
        jobs.spawn("forever".into(), None, std::future::pending());
        session.stop();
        assert!(jobs.wait().await.is_err());
    }

    /// `/compact` asks for a compaction, with what follows it as the focus; other text, and commands that only start the same way, do not.
    #[test]
    fn recognises_a_compact_request() {
        assert_eq!(compact_request("/compact"), Some(None));
        assert_eq!(compact_request("  /compact  \n"), Some(None));
        assert_eq!(
            compact_request("/compact keep the API notes"),
            Some(Some("keep the API notes".to_owned()))
        );
        assert_eq!(compact_request("/compacting"), None);
        assert_eq!(compact_request("please /compact"), None);
        assert_eq!(compact_request("compact"), None);
    }

    /// A conversation id is a file name, so it may not climb out of the folder, and the history of an id is found again by the same id.
    #[test]
    fn load_history_checks_the_id_and_finds_the_file_again() {
        let folder = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../build/tests/omc-session/history");
        let _ = std::fs::remove_dir_all(&folder);
        for id in ["", "../x", "a/b", ".hidden", "nul", "Com1.txt"] {
            assert!(load_history(id, Some(folder.clone()), None, None).is_err());
        }
        let (journal, history) = load_history("c-1", Some(folder.clone()), None, None).unwrap();
        assert!(history.is_empty());
        let scribe = journal.scribe();
        scribe.append(&Message::User("hi".into())).unwrap();
        scribe
            .append(&Message::Assistant(omc_providers::Reply::default()))
            .unwrap();
        drop(scribe);
        drop(journal);
        let (_, history) = load_history("c-1", Some(folder.clone()), None, None).unwrap();
        assert_eq!(history.len(), 2);
        std::fs::write(history_path(&folder, "bad"), "nonsense\n").unwrap();
        assert!(load_history("bad", Some(folder.clone()), None, None).is_err());
        std::fs::remove_dir_all(&folder).unwrap();
    }

    /// Defaults fill only the settings nothing was chosen for, and the model comes before the effort that depends on it.
    #[test]
    fn restored_settings_prefer_what_was_chosen() {
        let pair = |id: &str, value: &str| (id.to_owned(), value.to_owned());
        let wanted = restored_settings(
            &[pair("checkpoints", "on"), pair("mode", "yolo")],
            &[
                pair("effort", "high"),
                pair("mode", "plan"),
                pair("model", "m"),
            ],
        );
        assert_eq!(
            wanted,
            vec![
                pair("model", "m"),
                pair("checkpoints", "on"),
                pair("mode", "plan"),
                pair("effort", "high"),
            ]
        );
    }

    /// A conversation a client kept comes back as saved: answered turns as prompts and replies, a stopped turn as lost, and the name, settings and offer with it; importing over a history is refused.
    #[test]
    fn an_imported_conversation_reads_back() {
        let folder = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../build/tests/omc-session/import");
        let _ = std::fs::remove_dir_all(&folder);
        let turn = |id: &str, said: &str, end: Option<&str>| omc_wire::ImportedTurn {
            id: id.into(),
            prompt: format!("prompt {id}"),
            files: vec!["a.txt".into()],
            said: said.into(),
            end: end.map(str::to_owned),
        };
        let imported = omc_wire::Imported {
            title: Some("Old chat".into()),
            settings: vec![("effort".into(), "high".into())],
            offered: vec![omc_wire::Setting {
                id: "effort".into(),
                category: "thought_level".into(),
                name: "Effort".into(),
                current: "high".into(),
                picks: Vec::new(),
            }],
            commands: Vec::new(),
            window: Some(1000),
            turns: vec![
                turn("5", "first answer", None),
                turn("9", "", Some("stopped")),
                turn("12", "second answer", None),
            ],
        };
        import("old", Some(folder.clone()), &imported).unwrap();
        let conversation = history("old", Some(folder.clone())).unwrap();
        assert_eq!(conversation.title.as_deref(), Some("Old chat"));
        assert_eq!(conversation.settings, imported.settings);
        assert_eq!(conversation.window, Some(1000));
        assert_eq!(conversation.offered.len(), 1);
        let prompts: Vec<(&str, &str)> = conversation
            .turns
            .iter()
            .map(|turn| (turn.id.as_deref().unwrap_or(""), turn.prompt.as_str()))
            .collect();
        assert_eq!(
            prompts,
            vec![("5", "prompt 5"), ("9", "prompt 9"), ("12", "prompt 12")]
        );
        assert!(
            conversation.turns[1]
                .events
                .contains(&omc_wire::Event::Halted)
        );
        assert!(
            conversation.turns[2]
                .events
                .contains(&omc_wire::Event::Said("second answer".into()))
        );
        assert!(import("old", Some(folder), &imported).is_err());
    }
}
