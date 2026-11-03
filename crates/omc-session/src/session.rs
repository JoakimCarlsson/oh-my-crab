//! One conversation: the commands the client sends, and the turns they start.

use std::sync::Arc;

use omc_auth::{AuthError, Provider};
use omc_context::{Discovered, Locations, discover, expand_skill};
use omc_providers::Message;
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
    /// The conversation once it is opened.
    open: Option<Open>,
    /// The turn that is running and its number, when one is.
    running: Option<(u64, AbortHandle)>,
    /// The number the next turn gets.
    turns: u64,
    /// Where finished turns report.
    ended: UnboundedSender<(u64, Ended)>,
}

/// Runs a conversation: reads `commands` and reports what happens as events on `out`, until the client closes it or hangs up.
pub async fn run(mut commands: UnboundedReceiver<Command>, out: UnboundedSender<Event>) {
    let (ended, mut finished) = unbounded_channel();
    let mut session = Session {
        shared: Shared::new(out),
        history: Vec::new(),
        open: None,
        running: None,
        turns: 0,
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
        } = opening;
        let provider = provider.as_str();
        if let Some(home) = home {
            omc_auth::set_home(home);
        }
        let Some(provider) = parse_provider(provider) else {
            return self.say(Event::Failed(format!("unknown provider: {provider}")));
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
        let credentials = match omc_auth::credentials_for(provider).await {
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
            Ended::Failed(reason) => self.say(Event::Finished(Some(reason))),
        }
    }

    /// Stops the turn that is running, subagents included, and declines what it waited on.
    ///
    /// The turn's task owns everything it started, so aborting it ends them all; its history is never taken in.
    fn cancel(&mut self) {
        self.shared.questions.decline_all();
        if let Some((_, task)) = self.running.take() {
            task.abort();
            self.say(Event::Halted);
            self.say(Event::Finished(None));
        }
    }

    /// Ends whatever is running, for good.
    fn stop(&mut self) {
        self.shared.questions.decline_all();
        if let Some((_, task)) = self.running.take() {
            task.abort();
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
        let Some(provider) = parse_provider(provider) else {
            return self.say(Event::Failed(format!("unknown provider: {provider}")));
        };
        let out = self.shared.out.clone();
        tokio::spawn(async move {
            let address = out.clone();
            let outcome = omc_auth::login(provider, move |url| {
                let _ = address.send(Event::LoginUrl(url));
            })
            .await;
            let _ = out.send(match outcome {
                Ok(()) => Event::SignedIn,
                Err(error) => Event::Failed(error.to_string()),
            });
        });
    }
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

/// The provider `name` names, when it names one.
fn parse_provider(name: &str) -> Option<Provider> {
    match name {
        "claude" => Some(Provider::Claude),
        "codex" => Some(Provider::Codex),
        _ => None,
    }
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
}
