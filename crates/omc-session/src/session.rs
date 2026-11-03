//! One conversation: the commands the client sends, and the turns they start.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use omc_agent::Journal;
use omc_auth::{AuthError, LoginPrompt};
use omc_context::{Discovered, Locations, discover, expand_skill};
use omc_providers::Message;
use omc_providers::Provider;
use omc_tools::{Registry, Skill};
use omc_wire::{Command, Event, Opening, Slash};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use tokio::task::AbortHandle;

use crate::prompt::compose;
use crate::settings::{EFFORT, MODE, MODEL, Mode};
use crate::shared::Shared;
use crate::turn::{Ended, Turn};

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

/// Runs a conversation: reads `commands` and reports what happens as events on `out`, until the client closes it or hangs up.
pub async fn run(mut commands: UnboundedReceiver<Command>, out: UnboundedSender<Event>) {
    let (ended, mut finished) = unbounded_channel();
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
            Command::Prompt { text, files } => self.prompt(compose(&text, &files)),
            Command::Choose { ticket, option } => {
                self.shared.questions.answer(ticket, option.as_deref());
            }
            Command::Answer { ticket, replies } => self.shared.questions.reply(ticket, replies),
            Command::Cancel => self.cancel(),
            Command::Set { setting, value } => self.set(&setting, &value),
            Command::Login { provider } => self.login(&provider),
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
        } = opening;
        let provider = provider.as_str();
        if let Some(home) = home {
            omc_auth::set_home(home);
        }
        let Some(provider) = Provider::from_id(provider) else {
            return self.say(Event::Failed(format!("unknown provider: {provider}")));
        };
        self.journal = None;
        let saved = match conversation {
            Some(id) => match load_history(&id, history, keep) {
                Ok(saved) => Some(saved),
                Err(reason) => return self.say(Event::Failed(reason)),
            },
            None => None,
        };
        let mut registry = match Registry::builtin(&directory) {
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
        let credentials = match omc_providers::credentials_for(provider).await {
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
        let model = match model.or_else(|| models.first().cloned()) {
            Some(model) => model,
            None => match omc_providers::default_model(provider, &credentials).await {
                Ok(model) => model,
                Err(error) => return self.say(Event::Failed(error.to_string())),
            },
        };
        let commands = commands_of(&discovered);
        let (journal, messages) = saved.unzip();
        self.shared
            .notebook
            .load(journal.as_ref().map_or("", |journal| journal.notes()));
        self.history = messages.unwrap_or_default();
        self.journal = journal;
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
        self.shared.announce();
        self.say(Event::Commands(commands));
    }

    /// Starts a turn with `prompt`, unless one is running or nothing is open.
    fn prompt(&mut self, prompt: String) {
        let (true, Some((_, model))) = (self.open.is_some(), self.shared.model()) else {
            return self.say(Event::Failed("the conversation is not open".to_owned()));
        };
        if self.running.is_some() {
            return;
        }
        self.rescan();
        let Some(open) = &self.open else {
            return;
        };
        self.turns += 1;
        let number = self.turns;
        self.resets_at_start = self.shared.notebook.resets();
        let prompt = expand_skill(&prompt, &open.discovered.skills);
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
            let how = turn.run(prompt).await;
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

    /// Ends whatever is running, for good.
    fn stop(&mut self) {
        self.shared.questions.decline_all();
        if let Some((_, task)) = self.running.take() {
            task.abort();
            self.forget_turn();
        }
    }

    /// Records in the saved history that the turn that was stopped never happened.
    fn forget_turn(&mut self) {
        self.keep_fresh_window();
        if let Some(Err(error)) = self.journal.as_ref().map(|j| j.cut(self.history.len())) {
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

    /// Gives the setting `setting` the value `value`; the new value applies from the next request.
    fn set(&mut self, setting: &str, value: &str) {
        let applied = match setting {
            MODEL if self.open.is_some() => {
                self.shared.set_model(value);
                true
            }
            MODE => Mode::parse(value).is_some_and(|mode| {
                self.shared.set_mode(mode);
                true
            }),
            EFFORT => self.shared.set_effort(value),
            _ => false,
        };
        if applied {
            self.shared.announce();
        }
    }

    /// Signs in to `provider` in the background, reporting the address to open and how it ended.
    fn login(&self, provider: &str) {
        let Some(provider) = Provider::from_id(provider) else {
            return self.say(Event::Failed(format!("unknown provider: {provider}")));
        };
        let out = self.shared.out.clone();
        tokio::spawn(async move {
            let address = out.clone();
            let outcome = omc_providers::login(provider, move |prompt| {
                let _ = address.send(match prompt {
                    LoginPrompt::Url(url) => Event::LoginUrl(url),
                    LoginPrompt::Code { url, code } => Event::LoginCode { url, code },
                });
            })
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

/// Opens the saved history of the conversation `id` in `folder`, or in a folder under the harness's own data folder, keeping `keep` turns when that is given.
///
/// # Errors
///
/// Returns what to tell the client when `id` is not usable as a file name, there is no data folder, or the file cannot be read.
fn load_history(
    id: &str,
    folder: Option<PathBuf>,
    keep: Option<usize>,
) -> Result<(Arc<Journal>, Vec<Message>), String> {
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
    Journal::open(&history_path(&folder, id), keep)
        .map_err(|error| format!("cannot open the conversation: {error}"))
}

/// The file the history of conversation `id` is kept in, inside `folder`.
fn history_path(folder: &Path, id: &str) -> PathBuf {
    folder.join(format!("{id}.jsonl"))
}

/// The name the skill tool is registered under.
const SKILL_TOOL: &str = "skill";

/// The commands the client offers behind a slash for what was `discovered`: one for each skill.
fn commands_of(discovered: &Discovered) -> Vec<Slash> {
    discovered
        .skills
        .iter()
        .map(|skill| Slash {
            name: skill.name.clone(),
            description: skill.description.clone(),
        })
        .collect()
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
        assert_eq!(commands.len(), 1);
        assert_eq!(commands[0].name, "fresh");
        let open = session.open.as_ref().unwrap();
        assert!(open.base.get("skill").is_some());
        session.rescan();
        assert!(events.try_recv().is_err());
        std::fs::remove_dir_all(&directory).unwrap();
    }

    /// A conversation id is a file name, so it may not climb out of the folder, and the history of an id is found again by the same id.
    #[test]
    fn load_history_checks_the_id_and_finds_the_file_again() {
        let folder = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../build/tests/omc-session/history");
        let _ = std::fs::remove_dir_all(&folder);
        for id in ["", "../x", "a/b", ".hidden", "nul", "Com1.txt"] {
            assert!(load_history(id, Some(folder.clone()), None).is_err());
        }
        let (journal, history) = load_history("c-1", Some(folder.clone()), None).unwrap();
        assert!(history.is_empty());
        let scribe = journal.scribe();
        scribe.append(&Message::User("hi".into())).unwrap();
        scribe
            .append(&Message::Assistant(omc_providers::Reply::default()))
            .unwrap();
        drop(scribe);
        drop(journal);
        let (_, history) = load_history("c-1", Some(folder.clone()), None).unwrap();
        assert_eq!(history.len(), 2);
        std::fs::write(history_path(&folder, "bad"), "nonsense\n").unwrap();
        assert!(load_history("bad", Some(folder.clone()), None).is_err());
        std::fs::remove_dir_all(&folder).unwrap();
    }
}
