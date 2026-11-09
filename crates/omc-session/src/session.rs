//! One conversation: the commands the client sends, and the turns they start.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use omc_agent::{Isolation, Journal};
use omc_auth::{AuthError, LoginPrompt};
use omc_context::{Discovered, Locations, discover, expand_skill};
use omc_dap::DebugTool;
use omc_hooks::{Assessment, Engine, Setting, Standing, TrustStore, assess};
use omc_providers::Message;
use omc_providers::Provider;
use omc_script::Script;
use omc_tools::{
    LanguageService, Languages, Registry, Sandbox, Skill, Spill, Tool, ToolError, ToolOutput,
};
use omc_wire::{Command, Event, Opening, QueueState, Slash};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use tokio::task::AbortHandle;

use crate::lifecycle::IDLE_AFTER;
use crate::permissions::{self, FileKeeper, PermissionsFile};
use crate::prompt::compose;
use crate::settings::{
    CACHE, CHECKPOINTS, EFFORT, MEMORY, MODE, MODEL, Mode, OFF, ON, PLAN_AUTOSAVE, PLAN_MODEL,
    WARMING, parse_retention,
};
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
    /// How subagents get git worktrees of their own, when they can.
    isolation: Option<Arc<Isolation>>,
    /// The hooks that run, when there are any.
    hooks: Option<Arc<Engine>>,
    /// Whether the project may be used, and what it brings that is left out.
    trust: Trust,
    /// The tool servers reached, and the sign-ins they wait for.
    hub: Arc<omc_mcp::Hub>,
    /// The tool servers the client passed.
    client_servers: Vec<omc_wire::ToolServer>,
    /// The tool servers of the user's own files.
    user_servers: Vec<omc_mcp::Server>,
    /// The extensions started for the conversation.
    extensions: Arc<omc_extensions::Extensions>,
    /// The extensions of the user's own file.
    user_extensions: Vec<omc_extensions::Extension>,
}

/// Where a conversation's project stands on trust, and what the client was told about it.
struct Trust {
    /// The latest assessment of the project.
    assessment: Arc<Assessment>,
    /// Whether the reader trusted the project for this conversation only.
    vouched: bool,
    /// The trust request the client was sent last and has not answered, by its ticket.
    ticket: Option<u64>,
    /// The standing and what was left out, as the client was told them last, so they are told again only when they change.
    told: Option<(Standing, Vec<omc_wire::Skipped>)>,
    /// What the hooks of the conversation are run with.
    setting: Arc<dyn Fn() -> Setting + Send + Sync>,
    /// The reader's own hooks file for crab, when there is one.
    own: Option<PathBuf>,
    /// Where trust is recorded, when it can be.
    store: Option<TrustStore>,
}

impl Trust {
    /// The trust of a conversation whose project is assessed as `assessment`, whose hooks run with `setting`, reading the reader's own hooks from `own` and recording trust in `store`.
    fn new(
        assessment: Assessment,
        setting: Arc<dyn Fn() -> Setting + Send + Sync>,
        own: Option<PathBuf>,
        store: Option<TrustStore>,
    ) -> Self {
        Self {
            assessment: Arc::new(assessment),
            vouched: false,
            ticket: None,
            told: None,
            setting,
            own,
            store,
        }
    }

    /// What a conversation for `locations` may use now, leaving out what `disabled` names; what cannot be read is reported on standard error.
    fn assess(&self, locations: &Locations, disabled: &[String]) -> Assessment {
        assessed(
            locations,
            self.own.as_deref(),
            self.store.as_ref(),
            self.vouched,
            disabled,
        )
    }
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
    /// The number the last trust request was sent under.
    trust_tickets: u64,
    /// The wait after the last turn ended before the hooks are told crab is idle, while it runs.
    idle: Option<AbortHandle>,
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
///
/// When the conversation ends, its debugging sessions are disconnected cleanly first, and then everything still running is stopped.
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
        trust_tickets: 0,
        idle: None,
    };
    let reason = loop {
        tokio::select! {
            command = commands.recv() => match command {
                None => break END_OTHER,
                Some(Command::Close) if session.running.is_none() => break END_EXIT,
                Some(Command::Close) => break END_OTHER,
                Some(command) => session.handle(command).await,
            },
            Some((number, how)) = finished.recv() => session.finish(number, how),
        }
    };
    session.calm();
    session.shared.debuggers.close().await;
    session.stop();
    if session.open.is_some() {
        session.shared.lifecycle.end(reason).await;
    }
}

/// Why a conversation ended, as `SessionEnd` hooks are told it, when the client closed it while no turn ran.
const END_EXIT: &str = "prompt_input_exit";

/// Why a conversation ended, as `SessionEnd` hooks are told it, when the client hung up or closed it while a turn ran.
const END_OTHER: &str = "other";

/// Why a conversation ended, as `SessionEnd` hooks are told it, when another saved conversation was opened in its place.
const END_RESUME: &str = "resume";

/// Why a conversation ended, as `SessionEnd` hooks are told it, when a new conversation was opened in its place.
const END_CLEAR: &str = "clear";

impl Session {
    /// Carries out one command; any command means the reader is there, so crab is no longer waiting to tell the hooks it is idle.
    async fn handle(&mut self, command: Command) {
        self.calm();
        match command {
            Command::Open(opening) => self.open(opening).await,
            Command::Prompt {
                text,
                files,
                turn,
                context,
            } => {
                self.rescan();
                self.reach_servers().await;
                if let Some(focus) = compact_request(&text) {
                    if !files.is_empty() {
                        return self.say(Event::Failed(
                            "/compact takes no files; send them with a prompt instead".to_owned(),
                        ));
                    }
                    if self.running.is_some() {
                        return self.say(Event::Failed(
                            "/compact cannot run while a turn is running; send it again when the turn has finished".to_owned(),
                        ));
                    }
                    let mark = turn.map(|id| omc_agent::Mark {
                        id,
                        text: text.clone(),
                        files: Vec::new(),
                    });
                    return self.start(Job::Compact(focus), mark);
                }
                let names: Vec<String> = files
                    .iter()
                    .filter_map(|file| file.file_name())
                    .map(|name| name.to_string_lossy().into_owned())
                    .collect();
                let told = format!("{}{}", context.unwrap_or_default(), compose(&text, &files));
                self.prompt(told, turn, text, names);
            }
            Command::Choose {
                ticket,
                option,
                note,
            } => {
                self.shared
                    .questions
                    .answer(ticket, option.as_deref(), note.as_deref());
            }
            Command::Answer { ticket, replies } => self.shared.questions.reply(ticket, replies),
            Command::Rename { title } => {
                let title = title.split_whitespace().collect::<Vec<_>>().join(" ");
                if !title.is_empty() {
                    self.shared.name(&title, false);
                }
            }
            Command::Cancel => self.cancel(),
            Command::Set { setting, value } => self.set(&setting, &value).await,
            Command::Login { provider } => self.login(&provider),
            Command::Language { ticket, answer } => {
                self.shared.client_languages.answer(ticket, answer);
            }
            Command::Trust { ticket, verdict } => {
                self.trust(ticket, verdict);
                self.reach_servers().await;
            }
            Command::Switch { branch } => self.switch(&branch),
            Command::Close => {}
        }
    }

    /// Sends `event` to the client.
    fn say(&self, event: Event) {
        self.shared.say(event);
    }

    /// Opens the conversation: finds the provider, the tools, the skills and the agents, and says what can be set.
    ///
    /// A turn of the conversation open before is cancelled first, and the prompts queued for it are dropped, so that nothing of it reaches the conversation opened now; its background runs are stopped, its debugging sessions disconnected, and its `SessionEnd` hooks run. Once open, the `SessionStart` hooks start, for a resumed conversation when it was saved, and their context is told to the model with the first prompt.
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
            navigation,
        } = opening;
        let provider = provider.as_str();
        self.shared.set_home(home);
        let Some(provider) = Provider::from_id(provider) else {
            return self.say(Event::Failed(format!("unknown provider: {provider}")));
        };
        self.cancel();
        let replaced = self.open.is_some();
        self.journal = None;
        self.shared.jobs.stop_all();
        self.shared.debuggers.close().await;
        if self.open.is_some() {
            let reason = if conversation.is_some() {
                END_RESUME
            } else {
                END_CLEAR
            };
            self.shared.lifecycle.end(reason).await;
        }
        self.shared.lifecycle.set_engine(None);
        self.shared.lifecycle.forget_start();
        self.shared
            .client_languages
            .set_navigation(languages && navigation);
        let saved = match &conversation {
            Some(id) => match load_history(id, history, keep, before.as_deref()) {
                Ok(saved) => Some(saved),
                Err(reason) => return self.say(Event::Failed(reason)),
            },
            None => None,
        };
        self.shared.languages.set(None);
        let spill = spill_for(conversation.as_deref());
        let own_languages = self.shared.given_languages.is_none() && !languages;
        let isolation = isolation_for(&directory, &spill, conversation.as_deref(), own_languages);
        let sandbox = match Sandbox::new(&directory) {
            Ok(sandbox) => sandbox.with_spill(spill.clone()),
            Err(error) => return self.say(Event::Failed(error.to_string())),
        };
        let mut registry = Registry::builtin_for(
            sandbox.clone(),
            Some(self.shared.jobs.clone()),
            self.shared.languages.clone(),
        );
        registry.register(DebugTool::new(sandbox, self.shared.debuggers.clone()));
        registry.register(Script::new(spill.clone()));
        let setting = hook_setting(&self.shared, &directory, conversation.clone());
        let (own, store) = (omc_hooks::own_file(), TrustStore::locate());
        let assessment = assessed(
            &Locations::detect(&directory),
            own.as_deref(),
            store.as_ref(),
            false,
            &disabled,
        );
        self.shared.approvals.forget_session();
        self.put_rules_in_force(&Locations::detect(&directory), &assessment);
        let locations = assessment.locations.clone();
        let hooks = engine_for(&assessment, setting());
        let discovered = discover(&locations, &disabled);
        register_skills(&mut registry, &discovered.skills);
        let hub = omc_mcp::Hub::new(
            spill.clone(),
            omc_mcp::Tokens::locate(self.shared.home().as_deref()),
            sign_in_listener(&self.shared),
        );
        let user_servers = user_servers_for(&locations);
        let wanted = omc_mcp::config::merged(&servers, &assessment.servers, &user_servers);
        let reached = hub.connect(&wanted).await;
        for tool in reached.tools {
            registry.register_shared(tool);
        }
        for reason in reached.failed {
            eprintln!("tool server unavailable: {reason}");
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
        let made = setting();
        let extensions = omc_extensions::Extensions::new(omc_extensions::Setting {
            session: made.session,
            folder: made.folder,
            mode: made.mode,
        });
        let user_extensions = user_extensions_for();
        let started = extensions
            .start(&wanted_extensions(&user_extensions, &assessment.extensions))
            .await;
        register_new(&mut registry, started);
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
        self.shared.lifecycle.set_engine(hooks.clone());
        self.shared
            .lifecycle
            .set_extensions(Some(extensions.clone()));
        self.open = Some(Open {
            provider,
            base: Arc::new(registry),
            denied: Arc::new(denied),
            discovered: Arc::new(discovered),
            locations: Arc::new(locations),
            disabled,
            isolation,
            hooks,
            trust: Trust::new(assessment, setting, own, store),
            hub,
            client_servers: servers,
            user_servers,
            extensions,
            user_extensions,
        });
        let mode = if yolo { Mode::Yolo } else { Mode::Asking };
        self.shared
            .set_memories(memories_for(&directory, conversation.clone()));
        self.shared.open(provider, models, model, mode);
        self.shared.plans.set_directory(directory.clone());
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
        let trusted = self.shared.mode() == Mode::Yolo
            && self
                .open
                .as_ref()
                .is_some_and(|open| open.trust.assessment.standing.reads_project());
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
        let source = if self.journal.is_some() {
            START_RESUME
        } else if replaced {
            START_CLEAR
        } else {
            START_NEW
        };
        let model = self.shared.model().map(|(_, model)| model);
        self.shared.lifecycle.begin(source, model);
        self.tell_trust();
    }

    /// Switches the saved conversation to the branch `branch`, at its tip, and tells the client the turns it holds; a running turn is cancelled first, and the prompts queued for it are dropped. Background runs and debugging sessions go on, since the conversation is the same.
    ///
    /// The client is told why with [`Event::Failed`] when the conversation is not saved, has no such branch, or its file cannot be opened again; the conversation then stays where it was when it can.
    fn switch(&mut self, branch: &str) {
        let Some(journal) = self.journal.clone() else {
            return self.say(Event::Failed(
                "the conversation is not saved, so it has no branches".to_owned(),
            ));
        };
        if !journal.branches().1.iter().any(|held| held.id == branch) {
            return self.say(Event::Failed(format!("no branch has the id {branch}")));
        }
        self.cancel();
        self.shared.warmer.stop();
        let path = journal.path().to_path_buf();
        drop(journal);
        self.journal = None;
        self.shared.attach(None);
        let (journal, history) = match Journal::open_branch(&path, None, Some(branch), None) {
            Ok(opened) => opened,
            Err(error) => {
                self.say(Event::Failed(format!(
                    "cannot switch to branch {branch}: {error}"
                )));
                match Journal::open(&path, None) {
                    Ok(opened) => opened,
                    Err(error) => {
                        return self.say(Event::Failed(format!(
                            "cannot open the conversation again: {error}"
                        )));
                    }
                }
            }
        };
        self.shared.notebook.load(journal.notes());
        self.shared.checkpoint.clear();
        self.history = history;
        self.journal = Some(journal);
        self.shared.attach(self.journal.clone());
        self.say_history();
    }

    /// Answers the trust request under `ticket` with the reader's `verdict`: trusts the project for good, recording it for every worktree of its repository, for this conversation only, or records that it is not trusted; then reads what the project brings again, so the next turn uses it. A ticket that is not the one waiting is ignored.
    fn trust(&mut self, ticket: u64, verdict: omc_wire::Verdict) {
        let Some(open) = &mut self.open else {
            return;
        };
        if open.trust.ticket != Some(ticket) {
            return;
        }
        open.trust.ticket = None;
        let assessment = open.trust.assessment.clone();
        let recorded = match verdict {
            omc_wire::Verdict::Allow => {
                open.trust.vouched = true;
                Ok(())
            }
            omc_wire::Verdict::Always => open.trust.store.as_ref().map_or(Ok(()), |store| {
                store.set(&assessment.project, true, assessment.project_hooks.clone())
            }),
            omc_wire::Verdict::Reject => open.trust.store.as_ref().map_or(Ok(()), |store| {
                store.set(&assessment.project, false, Vec::new())
            }),
        };
        if let Err(error) = recorded {
            self.say(Event::Failed(format!(
                "cannot record the project's trust: {error}"
            )));
        }
        self.reread();
        if verdict == omc_wire::Verdict::Reject
            && let Some(open) = &mut self.open
        {
            let assessment = &open.trust.assessment;
            open.trust.told = Some((assessment.standing, assessment.skipped.clone()));
        }
        self.tell_trust();
    }

    /// Reaches the tool servers the conversation has now and has not tried yet, those of a project trusted since among them, and gives the agent their tools, and those of servers signed in to since, from the next turn on; the extensions not started yet, those of a project trusted since among them, are started, and their tools given too.
    async fn reach_servers(&mut self) {
        let Some(open) = &mut self.open else {
            return;
        };
        let wanted = wanted_extensions(&open.user_extensions, &open.trust.assessment.extensions);
        let started = open.extensions.start(&wanted).await;
        if !started.is_empty() {
            let mut registry = (*open.base).clone();
            register_new(&mut registry, started);
            open.base = Arc::new(registry);
        }
        let wanted = omc_mcp::config::merged(
            &open.client_servers,
            &open.trust.assessment.servers,
            &open.user_servers,
        );
        let reached = open.hub.connect(&wanted).await;
        for reason in reached.failed {
            eprintln!("tool server unavailable: {reason}");
        }
        add_tools(open, reached.tools);
    }

    /// Tells the client what the project brings that is left out until it is trusted, under a new ticket, when something is and it differs from what the client was told last.
    fn tell_trust(&mut self) {
        let Some(open) = &mut self.open else {
            return;
        };
        let assessment = &open.trust.assessment;
        let now = (assessment.standing, assessment.skipped.clone());
        if open.trust.told.as_ref() == Some(&now) {
            return;
        }
        open.trust.told = Some(now);
        let Some(standing) = assessment
            .standing
            .wire()
            .filter(|_| !assessment.skipped.is_empty())
        else {
            open.trust.ticket = None;
            return;
        };
        self.trust_tickets += 1;
        open.trust.ticket = Some(self.trust_tickets);
        let request = omc_wire::TrustRequest {
            ticket: self.trust_tickets,
            root: assessment.project.root.clone(),
            standing,
            skipped: assessment.skipped.clone(),
        };
        self.shared.say(Event::Trust(request));
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
        let (branch, branches) = self
            .journal
            .as_ref()
            .map(|journal| journal.branches())
            .map_or((None, Vec::new()), |(on, all)| {
                (Some(on), crate::translate::branches(&all))
            });
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
            branch,
            branches,
        }));
    }

    /// Answers the prompt that tells the model `told`, which the client sent as `text` with the files named `files` under the turn id `turn`: starts a turn with it, or queues it for the turn that is running and tells the client so.
    ///
    /// The skills must have been read again first, as [`Self::rescan`] does, so that a skill added since the last turn can be invoked.
    fn prompt(&mut self, told: String, turn: Option<String>, text: String, files: Vec<String>) {
        if self.open.is_none() {
            return self.say(Event::Failed("the conversation is not open".to_owned()));
        }
        let Some(open) = &self.open else {
            return;
        };
        let told = expand_skill(&told, &open.discovered.skills);
        if self.running.is_some() {
            let waiting = self.shared.queue.push(told, turn, text, files);
            return self.say(waiting);
        }
        let mark = turn.map(|id| omc_agent::Mark { id, text, files });
        self.start(Job::Prompt(told), mark);
    }

    /// Starts a turn doing `job`, whose prompt the client described as `mark` when it did, unless one is running or nothing is open; the skill a prompt invokes must already be expanded in it. A compaction stops warming the last request first, since the request after it no longer starts the same way.
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
        let Some(open) = &self.open else {
            return;
        };
        if matches!(job, Job::Compact(_)) {
            self.shared.warmer.stop();
        }
        self.turns += 1;
        let number = self.turns;
        self.resets_at_start = self.shared.notebook.resets();
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
            isolation: open.isolation.clone(),
            hooks: open.hooks.clone(),
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
    /// The project's trust is assessed again first, so hooks added or changed since are found, and those of a project whose hooks changed since it was trusted wait for trust again; the client is told when what is left out changed. The skill tool is rebuilt from the new list only when the list differs, which keeps the common turn to a scan of a few folders.
    fn rescan(&mut self) {
        self.reread();
        self.tell_trust();
    }

    /// Does what [`Self::rescan`] does, without telling the client what is left out; the tools of tool servers signed in to since are given to the agent too.
    fn reread(&mut self) {
        let Some(open) = &mut self.open else {
            return;
        };
        let arrived = open.hub.arrived();
        add_tools(open, arrived);
        let assessment = open.trust.assess(&open.locations, &open.disabled);
        let locations = open.locations.clone();
        self.put_rules_in_force(&locations, &assessment);
        let Some(open) = &mut self.open else {
            return;
        };
        open.hooks = engine_for(&assessment, (open.trust.setting)());
        self.shared.lifecycle.set_engine(open.hooks.clone());
        open.locations = Arc::new(assessment.locations.clone());
        open.trust.assessment = Arc::new(assessment);
        let found = discover(&open.locations, &open.disabled);
        let changed = found.skills != open.discovered.skills;
        if changed {
            let mut registry = open.base.filtered(|tool| tool.name() != SKILL_TOOL);
            register_skills(&mut registry, &found.skills);
            open.base = Arc::new(registry);
        }
        open.discovered = Arc::new(found);
        if changed {
            let commands = commands_of(&open.discovered);
            self.say(Event::Commands(commands));
        }
    }

    /// Puts in force the permission rules for a conversation in `locations` whose project's trust is `assessment`'s, read from crab's permissions file and Claude Code's settings, and keeps "always allow" answers in crab's file when there is one; what cannot be read is reported on standard error, and what cannot be kept is told to the client.
    fn put_rules_in_force(&self, locations: &Locations, assessment: &Assessment) {
        let file = PermissionsFile::locate();
        let loaded = permissions::load(locations, assessment, file.as_ref());
        for problem in &loaded.problems {
            eprintln!("permissions: {problem}");
        }
        self.shared.approvals.set_rules(loaded.book);
        let shared = Arc::downgrade(&self.shared);
        let keeper = file.map(|file| {
            FileKeeper::new(
                file,
                assessment.project.clone(),
                Box::new(move |problem| {
                    if let Some(shared) = shared.upgrade() {
                        shared.say(Event::Failed(problem));
                    }
                }),
            ) as Arc<dyn omc_agent::Keeper>
        });
        self.shared.approvals.set_keeper(keeper);
    }

    /// Takes in the end of turn `number`; a turn that was cancelled meanwhile is ignored.
    ///
    /// A turn that answered is followed by a turn for the prompt that has waited longest for it, when one still waits; the others wait for that turn. When the turn failed or needs signing in again, the prompts waiting are dropped instead, and its last request, which a failed turn's history no longer ends with, is no longer kept warm. When no turn follows, crab starts waiting to tell the hooks it is idle.
    fn finish(&mut self, number: u64, how: Ended) {
        if self.running.as_ref().map(|(running, _)| *running) != Some(number) {
            return;
        }
        self.running = None;
        match how {
            Ended::Done(history) => {
                self.history = history;
                self.say(Event::Finished(None));
                self.follow_up();
            }
            Ended::SignedOut(reason) => {
                self.shared.warmer.stop();
                self.say(Event::SignedOut {
                    reason,
                    account: None,
                });
                self.drop_queue();
            }
            Ended::Failed(reason) => {
                self.shared.warmer.stop();
                self.shared.checkpoint.clear();
                self.keep_fresh_window();
                self.say(Event::Finished(Some(reason)));
                self.drop_queue();
            }
        }
        if self.running.is_none() {
            self.wait_idle();
        }
    }

    /// Starts the wait after which the `Notification` hooks are told crab is idle, in place of any wait already running; they are told only when no command comes within [`IDLE_AFTER`].
    fn wait_idle(&mut self) {
        self.calm();
        let lifecycle = self.shared.lifecycle.clone();
        let waiting = tokio::spawn(async move {
            tokio::time::sleep(IDLE_AFTER).await;
            lifecycle.notify(IDLE, "", "crab is waiting for your input");
        });
        self.idle = Some(waiting.abort_handle());
    }

    /// Stops the wait to tell the hooks crab is idle, when one runs.
    fn calm(&mut self) {
        if let Some(waiting) = self.idle.take() {
            waiting.abort();
        }
    }

    /// Starts a turn with the prompt that has waited longest, when one waits, and tells the client it started; when the turn cannot start, that prompt and the ones still waiting are dropped, telling the client of each.
    fn follow_up(&mut self) {
        let Some(waiting) = self.shared.queue.pop() else {
            return;
        };
        self.say(waiting.event(QueueState::Started));
        self.rescan();
        let mark = waiting.mark();
        let dropped = waiting.event(QueueState::Dropped);
        self.start(Job::Prompt(waiting.told), mark);
        if self.running.is_none() {
            self.say(dropped);
            self.drop_queue();
        }
    }

    /// Drops every prompt waiting for the turn, telling the client of each, so that none is lost without a word.
    fn drop_queue(&self) {
        for waiting in self.shared.queue.drain() {
            self.say(waiting.event(QueueState::Dropped));
        }
    }

    /// Stops the turn that is running, subagents included, declines what it waited on and drops the prompts queued for it, telling the client of each.
    ///
    /// The turn's task owns everything it started, so aborting it ends them all; its history is never taken in. Queued prompts it had already taken in are cut back with it and saved as stopped.
    fn cancel(&mut self) {
        self.shared.questions.decline_all();
        if let Some((_, task)) = self.running.take() {
            task.abort();
            self.forget_turn();
            self.say(Event::Halted);
            self.say(Event::Finished(None));
        }
        self.drop_queue();
    }

    /// Ends whatever is running, for good, background runs and debugging sessions included.
    fn stop(&mut self) {
        self.shared.questions.decline_all();
        self.shared.jobs.stop_all();
        self.shared.debuggers.stop_all();
        if let Some((_, task)) = self.running.take() {
            task.abort();
            self.forget_turn();
        }
    }

    /// Records in the saved history that the turn that was stopped never happened, and stops warming its last request, which the next one will not start with.
    fn forget_turn(&mut self) {
        self.shared.warmer.stop();
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
    ///
    /// A switch to another model is put to the `PreModelSwitch` hooks first, which may keep it from happening, and the client is told why with [`Event::Failed`]; once the model changed, the `PostModelSwitch` hooks start, and their context is told to the model with the next prompt.
    async fn set(&mut self, setting: &str, value: &str) {
        let from = self.shared.model().map(|(_, model)| model);
        let switching = from
            .clone()
            .filter(|from| setting == MODEL && self.open.is_some() && from != value);
        if let Some(from) = &switching
            && let Some(engine) = self.shared.lifecycle.engine()
            && let Some(reason) = engine.model_switch(from, value).await
        {
            return self.say(Event::Failed(format!(
                "a PreModelSwitch hook kept the model from changing to {value}: {}",
                reason.trim()
            )));
        }
        if self.apply(setting, value) {
            self.shared.save_setting(setting, value);
            self.shared.announce();
            if let Some(from) = switching {
                self.shared.lifecycle.switched(from, value.to_owned());
            }
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
            CACHE => parse_retention(value).is_some_and(|retention| {
                self.shared.set_retention(retention);
                true
            }),
            WARMING => match value {
                ON | OFF => {
                    self.shared.set_warming(value == ON);
                    true
                }
                _ => false,
            },
            MEMORY => match value {
                ON | OFF => {
                    self.shared.set_memory(value == ON);
                    true
                }
                _ => false,
            },
            PLAN_MODEL if self.open.is_some() => self.shared.set_plan_model(value),
            PLAN_AUTOSAVE => match value {
                ON | OFF => {
                    self.shared.plans.set_autosave(value == ON);
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

/// How a conversation started, as `SessionStart` hooks are told it, when it is new.
const START_NEW: &str = "startup";

/// How a conversation started, as `SessionStart` hooks are told it, when it is new and was opened in place of another.
const START_CLEAR: &str = "clear";

/// How a conversation started, as `SessionStart` hooks are told it, when it was saved and is opened again.
const START_RESUME: &str = "resume";

/// The kind of notification the hooks are told when crab has waited [`IDLE_AFTER`] for the reader after a turn.
const IDLE: &str = "idle_prompt";

/// What a conversation for `locations` may use as its project's trust stands, with the reader's own hooks file `own` and the trust recorded in `store`, `vouched` when the reader trusted it for the conversation, leaving out what `disabled` names; what cannot be read is reported on standard error.
fn assessed(
    locations: &Locations,
    own: Option<&Path>,
    store: Option<&TrustStore>,
    vouched: bool,
    disabled: &[String],
) -> Assessment {
    let assessment = assess(locations, own, store, vouched, disabled);
    for problem in &assessment.problems {
        eprintln!("hooks: {problem}");
    }
    assessment
}

/// Gives the agent of `open` the tools `tools` too, from the next turn on.
fn add_tools(open: &mut Open, tools: Vec<Arc<dyn Tool>>) {
    if tools.is_empty() {
        return;
    }
    let mut registry = (*open.base).clone();
    for tool in tools {
        registry.register_shared(tool);
    }
    open.base = Arc::new(registry);
}

/// The extensions of the user's own file, the problems found in it reported on standard error.
fn user_extensions_for() -> Vec<omc_extensions::Extension> {
    let found =
        omc_extensions::config::user_extensions(omc_extensions::config::own_file().as_deref());
    for problem in &found.problems {
        eprintln!("extensions: {problem}");
    }
    found.extensions
}

/// The extensions to start: every one of `user`, then those of `project` whose names are not taken yet, so that the user's run first.
fn wanted_extensions(
    user: &[omc_extensions::Extension],
    project: &[omc_extensions::Extension],
) -> Vec<omc_extensions::Extension> {
    let mut wanted = user.to_vec();
    for extension in project {
        if !wanted.iter().any(|kept| kept.name == extension.name) {
            wanted.push(extension.clone());
        }
    }
    wanted
}

/// Adds `tools` to `registry`, except those named like a tool it already has, which are reported on standard error and left out.
fn register_new(registry: &mut Registry, tools: Vec<Arc<dyn Tool>>) {
    for tool in tools {
        if registry.get(tool.name()).is_some() {
            eprintln!(
                "extension tool skipped: a tool named {} already exists",
                tool.name()
            );
            continue;
        }
        registry.register_shared(tool);
    }
}

/// The tool servers of the user's own files for work in `locations`, the problems found in them reported on standard error.
fn user_servers_for(locations: &Locations) -> Vec<omc_mcp::Server> {
    let found = omc_mcp::config::user_servers(
        &locations.directory,
        locations.root.as_deref(),
        omc_mcp::config::own_file().as_deref(),
        locations.home.as_deref(),
        locations.claude.as_deref(),
    );
    for problem in &found.problems {
        eprintln!("tool servers: {problem}");
    }
    found.servers
}

/// What tells the client of `shared` about signing in to tool servers: the page to open as a [`Event::LoginUrl`], noted on standard error with how the sign-in ends.
fn sign_in_listener(shared: &Arc<Shared>) -> omc_mcp::Listener {
    let shared = Arc::downgrade(shared);
    Arc::new(move |notice| match notice {
        omc_mcp::Notice::SignIn { server, url } => {
            eprintln!("tool server {server} wants signing in: {url}");
            if let Some(shared) = shared.upgrade() {
                shared.say(Event::LoginUrl(url));
            }
        }
        omc_mcp::Notice::SignedIn { server } => {
            eprintln!("tool server {server} signed in; its tools come with the next prompt");
        }
        omc_mcp::Notice::Failed { server, reason } => {
            eprintln!("tool server sign-in failed: {server}: {reason}");
        }
    })
}

/// The engine running the hooks `assessment` lets run, with `setting`; nothing when there are none.
fn engine_for(assessment: &Assessment, setting: Setting) -> Option<Arc<Engine>> {
    (!assessment.hooks.is_empty()).then(|| Arc::new(Engine::new(assessment.hooks.clone(), setting)))
}

/// What the hooks of the conversation `id` in `directory` are run with, as a maker of it: the conversation's id, its folder and the mode `shared` is in when a hook runs, in Claude Code's words.
fn hook_setting(
    shared: &Arc<Shared>,
    directory: &Path,
    id: Option<String>,
) -> Arc<dyn Fn() -> Setting + Send + Sync> {
    let shared = Arc::downgrade(shared);
    let folder = directory.to_path_buf();
    Arc::new(move || {
        let shared = shared.clone();
        Setting {
            session: id.clone(),
            folder: folder.clone(),
            mode: Arc::new(move || match shared.upgrade().map(|shared| shared.mode()) {
                Some(Mode::Plan) => "plan",
                Some(Mode::Yolo) => "bypassPermissions",
                _ => "default",
            }),
        }
    })
}

/// The names Windows keeps for devices, which cannot be file names there whatever the extension.
const RESERVED_NAMES: [&str; 22] = [
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

/// How long the output a conversation saved is kept after it was last written to.
const SAVED_OUTPUT_AGE: Duration = Duration::from_secs(7 * 24 * 60 * 60);

/// Where the tools of the conversation `id`, or of a conversation that is not saved when there is none, save output too long to show, after clearing away the output of conversations nobody wrote to for [`SAVED_OUTPUT_AGE`].
fn spill_for(id: Option<&str>) -> Spill {
    Spill::prune(SAVED_OUTPUT_AGE, id);
    match id {
        Some(id) => Spill::for_conversation(id),
        None => Spill::default(),
    }
}

/// The memories of the project `directory` belongs to and of the user, for the conversation `id` when it has one; nothing when [`omc_memory::SWITCH_VARIABLE`] turns memory off or there is no folder to keep them in.
fn memories_for(directory: &Path, id: Option<String>) -> Option<Arc<omc_memory::Memories>> {
    if omc_memory::disabled_by_environment() {
        return None;
    }
    omc_memory::Memories::open(directory, id).map(Arc::new)
}

/// How long the worktrees of other conversations are kept after they were made, before they are taken to be left behind by a run that stopped or crashed.
const WORKTREE_AGE: Duration = Duration::from_secs(24 * 60 * 60);

/// How the subagents of the conversation `id`, working in `directory`, get git worktrees of their own: made in a folder named like its folder of saved output `spill` under the harness's cache folder, after clearing away the worktrees runs left behind, with the tools that touch files confined to the worktree and asking the harness's own language servers, started for the worktree, when `own_languages` says the conversation uses those; nothing when there is no cache folder.
///
/// The language services of a client or of a program embedding the harness are not asked about files in a worktree, since they work on the main working tree.
fn isolation_for(
    directory: &Path,
    spill: &Spill,
    id: Option<&str>,
    own_languages: bool,
) -> Option<Arc<Isolation>> {
    let root = dirs::cache_dir()?.join("oh-my-crab").join("worktrees");
    let name = spill.dir()?.file_name()?.to_owned();
    omc_agent::prune_worktrees(&root, id, WORKTREE_AGE);
    let saved = spill.clone();
    Some(Arc::new(Isolation {
        directory: directory.to_path_buf(),
        store: root.join(name),
        patches: spill.clone(),
        tools: Arc::new(move |folder: &Path| {
            let sandbox = Sandbox::new(folder)?.with_spill(saved.clone());
            let languages = Languages::new(if own_languages {
                crate::languages::own_servers(folder, false)
            } else {
                None
            });
            Ok(Registry::builtin_for(sandbox, None, languages))
        }),
    }))
}

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
        branch: Some(saved.branch),
        branches: crate::translate::branches(&saved.branches),
    })
}

/// Copies the branch the saved conversation `id` in `folder` is on, up to the turn `before` when that is given or whole otherwise, to the new saved conversation `copy` in the same folder, which a client can then open as a conversation of its own. It works while `id` is open elsewhere.
///
/// # Errors
///
/// Returns what to tell the client when either id is not usable, `id` cannot be read or has no turn `before`, or `copy` already holds a history, is open elsewhere or cannot be written.
pub fn fork(
    id: &str,
    copy: &str,
    folder: Option<PathBuf>,
    before: Option<&str>,
) -> Result<(), String> {
    let source = history_file(id, folder.clone())?;
    let destination = history_file(copy, folder)?;
    if source == destination {
        return Err("a conversation cannot be forked into itself".to_owned());
    }
    if !source.exists() {
        return Err(format!("no conversation is saved as {id}"));
    }
    Journal::fork(&source, &destination, before).map_err(|error| error.to_string())
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

/// Registers the skill tool for `skills` with `registry`: the tool that loads them, or, when there are none, one described the same way that says so when called, so that the agent is offered `skill` from its first turn and the tool list does not change when a skill appears later.
pub(crate) fn register_skills(registry: &mut Registry, skills: &[omc_context::Skill]) {
    match skills.is_empty() {
        true => registry.register(NoSkills(Skill::new(Vec::new()))),
        false => registry.register(Skill::new(skills.to_vec())),
    }
}

/// The skill tool of a conversation that has no skills: described like the tool that loads them, and refusing every call.
struct NoSkills(Skill);

#[async_trait::async_trait]
impl Tool for NoSkills {
    /// The name of the skill tool.
    fn name(&self) -> &'static str {
        self.0.name()
    }

    /// The description of the skill tool.
    fn description(&self) -> &str {
        self.0.description()
    }

    /// The argument schema of the skill tool.
    fn schema(&self) -> serde_json::Value {
        self.0.schema()
    }

    /// Refusing changes nothing.
    fn read_only(&self) -> bool {
        true
    }

    /// Describes a call as the skill tool does.
    fn describe(&self, args: &serde_json::Value) -> String {
        self.0.describe(args)
    }

    /// Refuses the call, since there is no skill to load.
    ///
    /// # Errors
    ///
    /// Always returns [`ToolError::Failed`] saying that no skills are available.
    async fn call(&self, _args: serde_json::Value) -> Result<ToolOutput, ToolError> {
        Err(ToolError::Failed(NO_SKILLS.to_owned()))
    }
}

/// What the agent is told when it calls `skill` in a conversation that has no skills.
const NO_SKILLS: &str = "no skills are available in this conversation";

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

    /// A hub with no tool servers, keeping no sign-ins.
    fn test_hub() -> Arc<omc_mcp::Hub> {
        omc_mcp::Hub::new(Spill::disabled(), None, Arc::new(|_| {}))
    }

    /// Extensions for a test conversation, none started.
    fn test_extensions() -> Arc<omc_extensions::Extensions> {
        omc_extensions::Extensions::new(omc_extensions::Setting {
            session: None,
            folder: PathBuf::from("."),
            mode: Arc::new(|| "default"),
        })
    }

    /// The trust of a conversation in `directory`, recorded in `store` when there is one, reading no hooks of the user's, vouched for when `vouched` is set.
    fn test_trust(directory: &Path, store: Option<TrustStore>, vouched: bool) -> Trust {
        let locations = Locations {
            directory: directory.to_path_buf(),
            root: Some(directory.to_path_buf()),
            home: None,
            claude: None,
            codex: None,
            project: true,
        };
        let assessment = assess(&locations, None, store.as_ref(), vouched, &[]);
        let folder = directory.to_path_buf();
        let setting: Arc<dyn Fn() -> Setting + Send + Sync> = Arc::new(move || Setting {
            session: None,
            folder: folder.clone(),
            mode: Arc::new(|| "default"),
        });
        let mut trust = Trust::new(assessment, setting, None, store);
        trust.vouched = vouched;
        trust
    }

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
            project: true,
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
                isolation: None,
                hooks: None,
                trust: test_trust(&directory, None, true),
                hub: test_hub(),
                client_servers: Vec::new(),
                user_servers: Vec::new(),
                extensions: test_extensions(),
                user_extensions: Vec::new(),
            }),
            running: None,
            turns: 0,
            resets_at_start: 0,
            ended,
            trust_tickets: 0,
            idle: None,
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
            trust_tickets: 0,
            idle: None,
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

    /// A session with nothing open whose events come back, opened with the `Open` command `line` decoded from the wire for a scratch folder named `name` holding `src/lib.rs` with `text`; the open fails on a conversation id that cannot be loaded, after the client's language offer is taken in. Returns the session, its events, the folder and the built-in tools, which ask the client's language services.
    async fn opened_with_languages(
        name: &str,
        line: impl Fn(&Path) -> String,
        text: &str,
    ) -> (Session, UnboundedReceiver<Event>, PathBuf, Arc<Registry>) {
        let directory = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../build/tests/omc-session")
            .join(name);
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(directory.join("src")).unwrap();
        let directory = directory.canonicalize().unwrap();
        std::fs::write(directory.join("src/lib.rs"), text).unwrap();
        let (out, events) = unbounded_channel();
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
            trust_tickets: 0,
            idle: None,
        };
        session
            .handle(omc_wire::decode(&line(&directory)).unwrap())
            .await;
        session
            .shared
            .languages
            .set(session.languages_for(&directory, true, false));
        let registry = Arc::new(
            Registry::builtin_in(&directory, None, session.shared.languages.clone()).unwrap(),
        );
        (session, events, directory, registry)
    }

    /// The `Open` line a client written before navigation existed sends: it offers language services and says nothing of navigating code.
    fn open_line_without_navigation(directory: &Path) -> String {
        serde_json::json!({"Open": {
            "directory": directory,
            "provider": "claude",
            "yolo": true,
            "conversation": "not/usable",
            "languages": true,
        }})
        .to_string()
    }

    /// An `Open` that leaves out `navigation`, as a client written before it does, reads as not navigating, and the `lsp` tool then never sends that client a question it does not know: every action but a file's diagnostics is refused without a `Language` event, and a file's diagnostics still go out as the `diagnostics` request they always were.
    #[tokio::test]
    async fn an_open_without_navigation_never_asks_the_new_questions() {
        let opening: Command =
            omc_wire::decode(&open_line_without_navigation(Path::new("/w"))).unwrap();
        let Command::Open(opening) = opening else {
            panic!("not an open");
        };
        assert!(opening.languages && !opening.navigation);
        let (mut session, mut events, directory, registry) = opened_with_languages(
            "no-navigation",
            open_line_without_navigation,
            "pub fn helper() {}\n",
        )
        .await;
        while events.try_recv().is_ok() {}
        let calls = [
            serde_json::json!({"action": "references", "path": "src/lib.rs", "symbol": "helper", "line": 1}),
            serde_json::json!({"action": "definition", "path": "src/lib.rs", "symbol": "helper"}),
            serde_json::json!({"action": "hover", "path": "src/lib.rs", "symbol": "helper"}),
            serde_json::json!({"action": "symbols", "path": "src/lib.rs"}),
            serde_json::json!({"action": "symbols", "query": "helper"}),
            serde_json::json!({"action": "diagnostics"}),
            serde_json::json!({"action": "rename", "path": "src/lib.rs", "symbol": "helper", "new_name": "aid"}),
            serde_json::json!({"action": "code_actions", "path": "src/lib.rs", "line": 1}),
            serde_json::json!({"action": "code_actions", "path": "src/lib.rs", "line": 1, "apply": 1}),
            serde_json::json!({"action": "format", "path": "src/lib.rs", "line": 1}),
            serde_json::json!({"action": "rename_file", "path": "src/lib.rs", "new_path": "src/aid.rs"}),
            serde_json::json!({"action": "status"}),
            serde_json::json!({"action": "reload"}),
        ];
        for call in calls {
            let Err(error) = registry.call_as("c", "lsp", call.clone()).await else {
                panic!("{call} was answered");
            };
            assert!(error.to_string().contains("cannot answer this"), "{error}");
            assert!(events.try_recv().is_err(), "{call} asked the client");
        }
        let checking = tokio::spawn({
            let registry = registry.clone();
            async move {
                registry
                    .call_as(
                        "c",
                        "lsp",
                        serde_json::json!({"action": "diagnostics", "path": "src/lib.rs"}),
                    )
                    .await
                    .unwrap()
            }
        });
        let Some(Event::Language(request)) = events.recv().await else {
            panic!("a file's diagnostics did not go to the client");
        };
        assert!(
            matches!(request.ask, omc_wire::LanguageAsk::Diagnostics { .. }),
            "{:?}",
            request.ask
        );
        session
            .handle(Command::Language {
                ticket: request.ticket,
                answer: omc_wire::LanguageAnswer::Diagnostics {
                    diagnostics: Vec::new(),
                },
            })
            .await;
        checking.await.unwrap();
        assert_eq!(
            std::fs::read_to_string(directory.join("src/lib.rs")).unwrap(),
            "pub fn helper() {}\n"
        );
        let formatting = tokio::spawn({
            let registry = registry.clone();
            async move {
                registry
                    .call_as(
                        "c",
                        "lsp",
                        serde_json::json!({"action": "format", "path": "src/lib.rs", "dry_run": true}),
                    )
                    .await
                    .unwrap()
            }
        });
        let Some(Event::Language(request)) = events.recv().await else {
            panic!("formatting a whole file did not go to the client");
        };
        assert!(
            matches!(request.ask, omc_wire::LanguageAsk::Format { .. }),
            "{:?}",
            request.ask
        );
        session
            .handle(Command::Language {
                ticket: request.ticket,
                answer: omc_wire::LanguageAnswer::Formatted {
                    text: Some("pub fn helper() {\n}\n".to_owned()),
                },
            })
            .await;
        let formatted = formatting.await.unwrap();
        assert!(
            formatted.text.starts_with("Dry run: formatting src/lib.rs"),
            "{}",
            formatted.text
        );
        assert!(events.try_recv().is_err());
        std::fs::remove_dir_all(&directory).unwrap();
    }

    /// A client that opens with `navigation` lists and applies code actions and answers the move of a file over the wire: the action's edits and the move's edits come back as edits that the harness writes, formats through the client and checks, and the client is told the file moved once it has.
    #[tokio::test]
    async fn a_navigating_client_applies_code_actions_and_moves_files() {
        let line = |directory: &Path| {
            serde_json::json!({"Open": {
                "directory": directory,
                "provider": "claude",
                "yolo": true,
                "conversation": "not/usable",
                "languages": true,
                "navigation": true,
            }})
            .to_string()
        };
        let (mut session, mut events, directory, registry) =
            opened_with_languages("navigation-actions", line, "pub mod util;\n").await;
        std::fs::write(directory.join("src/util.rs"), "pub fn one() {}\n").unwrap();
        while events.try_recv().is_ok() {}
        let (lib, util) = (directory.join("src/lib.rs"), directory.join("src/util.rs"));
        let call = |args: serde_json::Value| {
            let registry = registry.clone();
            tokio::spawn(async move { registry.call_as("c", "lsp", args).await })
        };
        let mut next_request = async || {
            let event = events.recv().await.unwrap();
            let line = omc_wire::encode(&event).unwrap();
            let Event::Language(request) = omc_wire::decode::<Event>(&line).unwrap() else {
                panic!("not a language request: {line}");
            };
            request
        };
        let edit = |line, column, end_column, text: &str| omc_wire::TextEdit {
            line,
            column,
            end_line: line,
            end_column,
            text: text.to_owned(),
        };
        let listing = call(
            serde_json::json!({"action": "code_actions", "path": "src/util.rs", "line": 1, "kind": "quickfix"}),
        );
        let request = next_request().await;
        let omc_wire::LanguageAsk::CodeActions { range, only } = &request.ask else {
            panic!("not a code actions request: {:?}", request.ask);
        };
        assert_eq!(
            (&range.path, range.line, range.column, range.end_column),
            (&util, 1, 1, 16)
        );
        assert_eq!(only, &["quickfix"]);
        session
            .handle(Command::Language {
                ticket: request.ticket,
                answer: omc_wire::LanguageAnswer::Actions {
                    actions: vec![omc_wire::ActionInfo {
                        title: "Add doc comment".to_owned(),
                        kind: Some("quickfix".to_owned()),
                        preferred: true,
                        disabled: None,
                    }],
                },
            })
            .await;
        let listed = listing.await.unwrap().unwrap();
        assert!(
            listed
                .text
                .contains("1. Add doc comment  [quickfix]  (preferred)"),
            "{}",
            listed.text
        );
        let applying = call(
            serde_json::json!({"action": "code_actions", "path": "src/util.rs", "line": 1, "apply": 1, "title": "Add doc comment"}),
        );
        let request = next_request().await;
        let omc_wire::LanguageAsk::ApplyCodeAction { index, title, .. } = &request.ask else {
            panic!("not an apply request: {:?}", request.ask);
        };
        assert_eq!((*index, title.as_deref()), (1, Some("Add doc comment")));
        session
            .handle(Command::Language {
                ticket: request.ticket,
                answer: omc_wire::LanguageAnswer::Applied {
                    title: "Add doc comment".to_owned(),
                    files: vec![omc_wire::FileEdits {
                        path: util.clone(),
                        edits: vec![edit(1, 1, 1, "/// One.\n")],
                    }],
                },
            })
            .await;
        for _ in 0..2 {
            let request = next_request().await;
            let answer = match request.ask {
                omc_wire::LanguageAsk::Format { .. } => {
                    omc_wire::LanguageAnswer::Formatted { text: None }
                }
                omc_wire::LanguageAsk::Diagnostics { .. } => {
                    omc_wire::LanguageAnswer::Diagnostics {
                        diagnostics: Vec::new(),
                    }
                }
                other => panic!("not a format or diagnostics request: {other:?}"),
            };
            session
                .handle(Command::Language {
                    ticket: request.ticket,
                    answer,
                })
                .await;
        }
        let applied = applying.await.unwrap().unwrap();
        assert!(
            applied
                .text
                .starts_with("Applied the code action \"Add doc comment\""),
            "{}",
            applied.text
        );
        assert_eq!(
            std::fs::read_to_string(&util).unwrap(),
            "/// One.\npub fn one() {}\n"
        );
        let helpers = directory.join("src/helpers.rs");
        let moving = call(
            serde_json::json!({"action": "rename_file", "path": "src/util.rs", "new_path": "src/helpers.rs"}),
        );
        let request = next_request().await;
        assert_eq!(
            request.ask,
            omc_wire::LanguageAsk::WillRenameFile {
                from: util.clone(),
                to: helpers.clone()
            }
        );
        session
            .handle(Command::Language {
                ticket: request.ticket,
                answer: omc_wire::LanguageAnswer::Edits {
                    files: vec![omc_wire::FileEdits {
                        path: lib.clone(),
                        edits: vec![edit(1, 9, 13, "helpers")],
                    }],
                },
            })
            .await;
        let mut told = false;
        while !told {
            let request = next_request().await;
            let answer = match request.ask {
                omc_wire::LanguageAsk::Format { .. } => {
                    omc_wire::LanguageAnswer::Formatted { text: None }
                }
                omc_wire::LanguageAsk::Diagnostics { .. } => {
                    omc_wire::LanguageAnswer::Diagnostics {
                        diagnostics: Vec::new(),
                    }
                }
                omc_wire::LanguageAsk::FileRenamed { from, to } => {
                    assert_eq!((&from, &to), (&util, &helpers));
                    assert!(!util.exists() && helpers.exists());
                    told = true;
                    omc_wire::LanguageAnswer::Done {}
                }
                other => panic!("not a request a move makes: {other:?}"),
            };
            session
                .handle(Command::Language {
                    ticket: request.ticket,
                    answer,
                })
                .await;
        }
        let moved = moving.await.unwrap().unwrap();
        assert!(
            moved
                .text
                .starts_with("Moved src/util.rs to src/helpers.rs"),
            "{}",
            moved.text
        );
        assert_eq!(std::fs::read_to_string(&lib).unwrap(), "pub mod helpers;\n");
        assert_eq!(
            std::fs::read_to_string(&helpers).unwrap(),
            "/// One.\npub fn one() {}\n"
        );
        std::fs::remove_dir_all(&directory).unwrap();
    }

    /// A client that opens with `navigation` is asked about code over the wire: its references come back as the tool's places, and a rename is answered with edits that the harness, not the client, writes, formats through the client and checks, giving each file's new tag.
    #[tokio::test]
    async fn a_navigating_client_answers_references_and_renames() {
        let line = |directory: &Path| {
            serde_json::json!({"Open": {
                "directory": directory,
                "provider": "claude",
                "yolo": true,
                "conversation": "not/usable",
                "languages": true,
                "navigation": true,
            }})
            .to_string()
        };
        let (mut session, mut events, directory, registry) = opened_with_languages(
            "navigation",
            line,
            "pub fn helper() {}\npub fn main() { helper(); }\n",
        )
        .await;
        while events.try_recv().is_ok() {}
        let file = directory.join("src/lib.rs");
        let call = |args: serde_json::Value| {
            let registry = registry.clone();
            tokio::spawn(async move { registry.call_as("c", "lsp", args).await })
        };
        let mut next_request = async || {
            let event = events.recv().await.unwrap();
            let line = omc_wire::encode(&event).unwrap();
            let Event::Language(request) = omc_wire::decode::<Event>(&line).unwrap() else {
                panic!("not a language request: {line}");
            };
            request
        };
        let looking = call(
            serde_json::json!({"action": "references", "path": "src/lib.rs", "symbol": "helper", "line": 1}),
        );
        let request = next_request().await;
        let omc_wire::LanguageAsk::References { at } = &request.ask else {
            panic!("not a references request: {:?}", request.ask);
        };
        assert_eq!((&at.path, at.line, at.column), (&file, 1, 8));
        let stretch = |line, column| omc_wire::Stretch {
            path: file.clone(),
            line,
            column,
            end_line: line,
            end_column: column + 6,
        };
        session
            .handle(Command::Language {
                ticket: request.ticket,
                answer: omc_wire::LanguageAnswer::Locations {
                    locations: vec![stretch(1, 8), stretch(2, 17)],
                },
            })
            .await;
        let found = looking.await.unwrap().unwrap();
        assert!(found.text.contains("2:17"), "{}", found.text);
        let renaming = call(
            serde_json::json!({"action": "rename", "path": "src/lib.rs", "symbol": "helper", "line": 1, "new_name": "aid"}),
        );
        let request = next_request().await;
        let omc_wire::LanguageAsk::Rename { new_name, .. } = &request.ask else {
            panic!("not a rename request: {:?}", request.ask);
        };
        assert_eq!(new_name, "aid");
        let edit = |line, column| omc_wire::TextEdit {
            line,
            column,
            end_line: line,
            end_column: column + 6,
            text: "aid".to_owned(),
        };
        session
            .handle(Command::Language {
                ticket: request.ticket,
                answer: omc_wire::LanguageAnswer::Edits {
                    files: vec![omc_wire::FileEdits {
                        path: file.clone(),
                        edits: vec![edit(1, 8), edit(2, 17)],
                    }],
                },
            })
            .await;
        let request = next_request().await;
        assert_eq!(
            request.ask,
            omc_wire::LanguageAsk::Format { path: file.clone() }
        );
        session
            .handle(Command::Language {
                ticket: request.ticket,
                answer: omc_wire::LanguageAnswer::Formatted { text: None },
            })
            .await;
        let request = next_request().await;
        assert!(matches!(
            request.ask,
            omc_wire::LanguageAsk::Diagnostics { .. }
        ));
        session
            .handle(Command::Language {
                ticket: request.ticket,
                answer: omc_wire::LanguageAnswer::Diagnostics {
                    diagnostics: Vec::new(),
                },
            })
            .await;
        let renamed = renaming.await.unwrap().unwrap();
        let written = "pub fn aid() {}\npub fn main() { aid(); }\n";
        assert_eq!(std::fs::read_to_string(&file).unwrap(), written);
        assert!(
            renamed.text.contains(&format!(
                "src/lib.rs: 2 edit(s), tag {}",
                omc_tools::snapshot_tag(written.as_bytes())
            )),
            "{}",
            renamed.text
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
            trust_tickets: 0,
            idle: None,
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
            trust_tickets: 0,
            idle: None,
        };
        let client: Arc<dyn LanguageService> = session.shared.client_languages.clone();
        let chosen = session.languages_for(Path::new("/w"), true, false).unwrap();
        assert!(Arc::ptr_eq(&chosen, &client));
        let own = session
            .languages_for(Path::new("/w"), false, false)
            .unwrap();
        assert!(!Arc::ptr_eq(&own, &client));
    }

    /// Makes `directory` a fresh git repository of its own, so it is a project apart from the one the tests run in.
    fn repository(directory: &Path) {
        std::fs::create_dir_all(directory).unwrap();
        let status = std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(directory)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .stdout(std::process::Stdio::null())
            .status()
            .unwrap();
        assert!(status.success());
    }

    /// An untrusted project's hook and instruction file are left out and the client is asked about them over the wire, once; a stale ticket is ignored, and trusting it for good records it, runs the hook and reads the file from then on.
    #[tokio::test]
    async fn trust_is_asked_over_the_wire_and_granted() {
        let base = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../build/tests/omc-session/trust-wire");
        let _ = std::fs::remove_dir_all(&base);
        let directory = base.join("repo");
        repository(&directory);
        let directory = directory.canonicalize().unwrap();
        std::fs::create_dir_all(directory.join(".claude")).unwrap();
        std::fs::write(
            directory.join(".claude/settings.json"),
            r#"{"hooks":{"PreToolUse":[{"matcher":"Bash","hooks":[{"type":"command","command":"./no-rm.sh"}]}]}}"#,
        )
        .unwrap();
        std::fs::write(directory.join("AGENTS.md"), "Project rules.").unwrap();
        let store = TrustStore::new(base.join("data/trust.json"));
        let mut session = bare_session();
        let (out, mut events) = unbounded_channel();
        session.shared = Shared::new(out);
        let trust = test_trust(&directory, Some(store.clone()), false);
        let locations = trust.assessment.locations.clone();
        session.open = Some(Open {
            provider: Provider::Claude,
            base: Arc::new(Registry::builtin(&directory).unwrap()),
            denied: Arc::new(Vec::new()),
            discovered: Arc::new(discover(&locations, &[])),
            locations: Arc::new(locations),
            disabled: Vec::new(),
            isolation: None,
            hooks: None,
            trust,
            hub: test_hub(),
            client_servers: Vec::new(),
            user_servers: Vec::new(),
            extensions: test_extensions(),
            user_extensions: Vec::new(),
        });
        session.tell_trust();
        session.rescan();
        let told: Vec<Event> = std::iter::from_fn(|| events.try_recv().ok()).collect();
        let asked: Vec<&omc_wire::TrustRequest> = told
            .iter()
            .filter_map(|event| match event {
                Event::Trust(request) => Some(request),
                _ => None,
            })
            .collect();
        assert_eq!(asked.len(), 1, "{told:?}");
        let request = asked[0].clone();
        let line = omc_wire::encode(&Event::Trust(request.clone())).unwrap();
        assert!(line.starts_with(r#"{"Trust":"#));
        assert_eq!(request.standing, omc_wire::Standing::Unknown);
        assert_eq!(request.root, directory);
        let kinds: Vec<omc_wire::Input> = request.skipped.iter().map(|s| s.kind).collect();
        assert_eq!(
            kinds,
            vec![omc_wire::Input::Hook, omc_wire::Input::Instruction]
        );
        let open = session.open.as_ref().unwrap();
        assert!(open.hooks.is_none() && open.discovered.instructions.is_empty());
        let answer = |ticket| {
            omc_wire::decode::<Command>(&format!(
                r#"{{"Trust":{{"ticket":{ticket},"verdict":"Always"}}}}"#
            ))
            .unwrap()
        };
        session.handle(answer(request.ticket + 1)).await;
        assert!(store.get(&omc_memory::Project::of(&directory)).is_none());
        session.handle(answer(request.ticket)).await;
        let record = store.get(&omc_memory::Project::of(&directory)).unwrap();
        assert!(record.trusted);
        assert_eq!(
            record.hooks,
            vec!["PreToolUse\tBash\t./no-rm.sh".to_owned()]
        );
        let open = session.open.as_ref().unwrap();
        assert_eq!(open.trust.assessment.standing, Standing::Trusted);
        assert!(
            open.hooks
                .as_ref()
                .is_some_and(|hooks| hooks.has(omc_hooks::HookEvent::PreToolUse))
        );
        assert_eq!(open.discovered.instructions.len(), 1);
        assert!(
            !std::iter::from_fn(|| events.try_recv().ok())
                .any(|event| matches!(event, Event::Trust(_)))
        );
    }

    /// Declining trust is recorded and not asked about again in the same conversation; trusting for the conversation only records nothing.
    #[tokio::test]
    async fn trust_can_be_declined_or_given_once() {
        let base = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../build/tests/omc-session/trust-decline");
        let _ = std::fs::remove_dir_all(&base);
        let directory = base.join("repo");
        repository(&directory);
        std::fs::write(directory.join("AGENTS.md"), "Project rules.").unwrap();
        let store = TrustStore::new(base.join("data/trust.json"));
        for (verdict, recorded) in [("Reject", Some(false)), ("Allow", None)] {
            let _ = std::fs::remove_file(store.path());
            let mut session = bare_session();
            let (out, mut events) = unbounded_channel();
            session.shared = Shared::new(out);
            let trust = test_trust(&directory, Some(store.clone()), false);
            let locations = trust.assessment.locations.clone();
            session.open = Some(Open {
                provider: Provider::Claude,
                base: Arc::new(Registry::builtin(&directory).unwrap()),
                denied: Arc::new(Vec::new()),
                discovered: Arc::new(discover(&locations, &[])),
                locations: Arc::new(locations),
                disabled: Vec::new(),
                isolation: None,
                hooks: None,
                trust,
                hub: test_hub(),
                client_servers: Vec::new(),
                user_servers: Vec::new(),
                extensions: test_extensions(),
                user_extensions: Vec::new(),
            });
            session.tell_trust();
            let Ok(Event::Trust(request)) = events.try_recv() else {
                panic!("no trust request");
            };
            session
                .handle(
                    omc_wire::decode(&format!(
                        r#"{{"Trust":{{"ticket":{},"verdict":"{verdict}"}}}}"#,
                        request.ticket
                    ))
                    .unwrap(),
                )
                .await;
            session.rescan();
            assert!(
                !std::iter::from_fn(|| events.try_recv().ok())
                    .any(|event| matches!(event, Event::Trust(_))),
                "{verdict} asked again"
            );
            let project = omc_memory::Project::of(&directory);
            assert_eq!(store.get(&project).map(|record| record.trusted), recorded);
            let read = !session
                .open
                .as_ref()
                .unwrap()
                .discovered
                .instructions
                .is_empty();
            assert_eq!(read, verdict == "Allow");
        }
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
            trust_tickets: 0,
            idle: None,
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
            trust_tickets: 0,
            idle: None,
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

    /// A session opened on an empty scratch folder named `name`, with no login and a turn that never ends running, and the events it sends.
    fn busy_session(name: &str) -> (Session, UnboundedReceiver<Event>) {
        let directory = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../build/tests/omc-session")
            .join(name);
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory).unwrap();
        let locations = Locations {
            directory: directory.clone(),
            root: Some(directory.clone()),
            home: None,
            claude: None,
            codex: None,
            project: true,
        };
        let (out, events) = unbounded_channel();
        let (ended, _finished) = unbounded_channel();
        let shared = Shared::new(out);
        shared.set_home(Some(directory.join("home")));
        shared.open(Provider::Claude, vec!["m".into()], "m".into(), Mode::Asking);
        let mut session = Session {
            shared,
            history: Vec::new(),
            journal: None,
            open: Some(Open {
                provider: Provider::Claude,
                base: Arc::new(Registry::builtin(&directory).unwrap()),
                denied: Arc::new(Vec::new()),
                discovered: Arc::new(discover(&locations, &[])),
                locations: Arc::new(locations),
                disabled: Vec::new(),
                isolation: None,
                hooks: None,
                trust: test_trust(&directory, None, true),
                hub: test_hub(),
                client_servers: Vec::new(),
                user_servers: Vec::new(),
                extensions: test_extensions(),
                user_extensions: Vec::new(),
            }),
            running: None,
            turns: 1,
            resets_at_start: 0,
            ended,
            trust_tickets: 0,
            idle: None,
        };
        let task = tokio::spawn(std::future::pending::<()>());
        session.running = Some((1, task.abort_handle()));
        (session, events)
    }

    /// The prompt command `text` with no files, under the turn id `turn`.
    fn prompt(text: &str, turn: &str) -> Command {
        Command::Prompt {
            text: text.into(),
            files: Vec::new(),
            turn: Some(turn.into()),
            context: None,
        }
    }

    /// The events sent so far, with the queue's events as their ticket, turn id and state.
    fn queue_events(events: &mut UnboundedReceiver<Event>) -> Vec<(u64, String, QueueState)> {
        std::iter::from_fn(|| events.try_recv().ok())
            .filter_map(|event| match event {
                Event::Queued(queued) => {
                    Some((queued.ticket, queued.turn.unwrap_or_default(), queued.state))
                }
                _ => None,
            })
            .collect()
    }

    /// Prompts sent while a turn runs are queued and said to wait, a `/compact` is refused, and cancelling the turn drops what waits, saying so for each.
    #[tokio::test]
    async fn prompts_sent_while_running_wait_and_cancelling_drops_them() {
        let (mut session, mut events) = busy_session("queue-cancel");
        session.handle(prompt("first", "a")).await;
        session.handle(prompt("second", "b")).await;
        session.handle(prompt("/compact", "c")).await;
        let told: Vec<Event> = std::iter::from_fn(|| events.try_recv().ok()).collect();
        assert!(
            told.iter().any(
                |event| matches!(event, Event::Failed(reason) if reason.starts_with("/compact"))
            )
        );
        let waiting: Vec<(u64, Option<String>, QueueState)> = told
            .into_iter()
            .filter_map(|event| match event {
                Event::Queued(queued) => Some((queued.ticket, queued.turn, queued.state)),
                _ => None,
            })
            .collect();
        assert_eq!(
            waiting,
            vec![
                (1, Some("a".into()), QueueState::Waiting),
                (2, Some("b".into()), QueueState::Waiting),
            ]
        );
        session.cancel();
        assert_eq!(
            queue_events(&mut events),
            vec![
                (1, "a".into(), QueueState::Dropped),
                (2, "b".into(), QueueState::Dropped),
            ]
        );
        assert!(session.shared.queue.pop().is_none());
    }

    /// A turn that answers is followed by a turn for the prompt that waited longest, and the rest keep waiting; a turn that fails drops what waits.
    #[tokio::test]
    async fn a_finished_turn_starts_the_next_prompt_and_a_failed_one_drops_them() {
        let (mut session, mut events) = busy_session("queue-follow");
        session.handle(prompt("first", "a")).await;
        session.handle(prompt("second", "b")).await;
        let _ = queue_events(&mut events);
        session.finish(1, Ended::Done(Vec::new()));
        assert_eq!(
            queue_events(&mut events),
            vec![(1, "a".into(), QueueState::Started)]
        );
        assert_eq!(session.running.as_ref().map(|(number, _)| *number), Some(2));
        session.finish(2, Ended::Failed("broken".into()));
        assert_eq!(
            queue_events(&mut events),
            vec![(2, "b".into(), QueueState::Dropped)]
        );
        assert!(session.running.is_none());
        session.stop();
    }

    /// When the turn for the prompt that waited longest cannot start, that prompt is dropped too, after the client was told it started, and so are the rest.
    #[tokio::test]
    async fn a_follow_up_that_cannot_start_is_dropped() {
        let (mut session, mut events) = busy_session("queue-unstartable");
        session.handle(prompt("first", "a")).await;
        session.handle(prompt("second", "b")).await;
        let _ = queue_events(&mut events);
        session.open = None;
        session.finish(1, Ended::Done(Vec::new()));
        assert_eq!(
            queue_events(&mut events),
            vec![
                (1, "a".into(), QueueState::Started),
                (1, "a".into(), QueueState::Dropped),
                (2, "b".into(), QueueState::Dropped),
            ]
        );
        assert!(session.running.is_none());
    }

    /// Opening a conversation while a turn runs stops that turn and drops the prompts queued for it, saying so for each, before anything of the new conversation is touched.
    #[tokio::test]
    async fn opening_while_a_turn_runs_stops_it_and_drops_the_queue() {
        let (mut session, mut events) = busy_session("queue-reopen");
        session.handle(prompt("first", "a")).await;
        session.handle(prompt("second", "b")).await;
        let _ = queue_events(&mut events);
        let directory = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../build/tests/omc-session/queue-reopen");
        session
            .handle(Command::Open(Opening {
                directory,
                home: None,
                provider: "claude".into(),
                model: None,
                yolo: false,
                tools: Vec::new(),
                denied: Vec::new(),
                disabled: Vec::new(),
                conversation: Some("not/usable".into()),
                history: None,
                keep: None,
                defaults: Vec::new(),
                before: None,
                languages: false,
                navigation: false,
            }))
            .await;
        assert!(session.running.is_none());
        let told: Vec<Event> = std::iter::from_fn(|| events.try_recv().ok()).collect();
        assert!(told.contains(&Event::Halted));
        let dropped: Vec<(u64, QueueState)> = told
            .iter()
            .filter_map(|event| match event {
                Event::Queued(queued) => Some((queued.ticket, queued.state)),
                _ => None,
            })
            .collect();
        assert_eq!(
            dropped,
            vec![(1, QueueState::Dropped), (2, QueueState::Dropped)]
        );
        assert!(session.shared.queue.pop().is_none());
    }

    /// The last request stops being kept warm when the turn is cancelled, when it fails, and before a compaction.
    #[tokio::test]
    async fn warming_stops_with_a_turn_that_ends_badly_and_before_compacting() {
        let (mut session, _events) = busy_session("warm-cancel");
        session.shared.warmer.arm();
        session.cancel();
        assert!(!session.shared.warmer.armed());
        let (mut session, _events) = busy_session("warm-failed");
        session.shared.warmer.arm();
        session.finish(1, Ended::Failed("broken".into()));
        assert!(!session.shared.warmer.armed());
        let (mut session, _events) = busy_session("warm-compact");
        session.finish(1, Ended::Done(Vec::new()));
        session.shared.warmer.arm();
        session.handle(prompt("/compact", "c")).await;
        assert!(!session.shared.warmer.armed());
        session.stop();
    }

    /// A conversation without skills still has a `skill` tool, described like the one that loads skills, which says there are none.
    #[tokio::test]
    async fn the_skill_tool_is_there_without_skills() {
        let mut registry = Registry::new();
        register_skills(&mut registry, &[]);
        let tool = registry.get(SKILL_TOOL).unwrap();
        let loader = Skill::new(Vec::new());
        assert_eq!(
            (tool.description(), tool.schema()),
            (loader.description(), loader.schema())
        );
        let refused = registry
            .call(SKILL_TOOL, serde_json::json!({"name": "pdf"}))
            .await
            .unwrap_err();
        assert!(refused.to_string().contains(NO_SKILLS), "{refused}");
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

    /// Going back keeps the turns left behind on their branch: `Switch` brings that branch back at its tip and tells the client its turns and every branch, an unknown branch or an unsaved conversation fails, and the branch can be forked into a conversation of its own.
    #[tokio::test]
    async fn switching_branches_brings_back_the_turns_left_behind() {
        let folder = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../build/tests/omc-session/branches");
        let _ = std::fs::remove_dir_all(&folder);
        let turn = |id: &str| omc_wire::ImportedTurn {
            id: id.into(),
            prompt: format!("prompt {id}"),
            files: Vec::new(),
            said: format!("answer {id}"),
            end: None,
        };
        let imported = omc_wire::Imported {
            turns: vec![turn("1"), turn("2"), turn("3")],
            ..omc_wire::Imported::default()
        };
        import("br", Some(folder.clone()), &imported).unwrap();
        let (journal, messages) =
            load_history("br", Some(folder.clone()), None, Some("2")).unwrap();
        assert_eq!(messages.len(), 2);
        let (mut session, mut events) = busy_session("switch");
        if let Some((_, task)) = session.running.take() {
            task.abort();
        }
        session.history = messages;
        session.journal = Some(journal);
        session.shared.attach(session.journal.clone());
        session.handle(Command::Switch { branch: "1".into() }).await;
        let mut resumed = None;
        while let Ok(event) = events.try_recv() {
            if let Event::Resumed(conversation) = event {
                resumed = Some(conversation);
            }
        }
        let resumed = resumed.expect("no Resumed after switching");
        assert_eq!(resumed.branch.as_deref(), Some("1"));
        assert_eq!(resumed.turns.len(), 3);
        assert_eq!(resumed.branches.len(), 2);
        assert_eq!(resumed.branches[1].before.as_deref(), Some("2"));
        assert_eq!(resumed.branches[1].turns.len(), 1);
        assert_eq!(session.history.len(), 6);
        session.handle(Command::Switch { branch: "9".into() }).await;
        assert!(matches!(events.try_recv(), Ok(Event::Failed(_))));
        assert_eq!(session.history.len(), 6);
        session.journal = None;
        session.shared.attach(None);
        let saved = history("br", Some(folder.clone())).unwrap();
        assert_eq!(saved.branch.as_deref(), Some("1"));
        assert_eq!(saved.branches.len(), 2);
        fork("br", "br-copy", Some(folder.clone()), Some("3")).unwrap();
        let copy = history("br-copy", Some(folder.clone())).unwrap();
        assert_eq!(copy.turns.len(), 2);
        assert!(fork("br", "br", Some(folder.clone()), None).is_err());
        assert!(fork("none", "other", Some(folder), None).is_err());
        let mut unsaved = bare_session();
        let (out, mut told) = unbounded_channel();
        unsaved.shared = Shared::new(out);
        unsaved.handle(Command::Switch { branch: "1".into() }).await;
        assert!(matches!(told.try_recv(), Ok(Event::Failed(_))));
    }
}
