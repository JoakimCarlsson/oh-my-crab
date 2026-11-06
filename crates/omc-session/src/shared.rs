//! The state of a conversation that turns, tools and commands all reach.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use omc_agent::Approvals;
use omc_providers::Effort;
use omc_providers::{Provider, Retention};
use omc_tools::{Checkpoint, Host, Jobs, LanguageService, Languages, Notebook};
use omc_wire::{Event, PlanStep, Question, Reply};
use tokio::sync::mpsc::UnboundedSender;

use crate::bridge::Questions;
use crate::languages::ClientLanguages;
use crate::prefix::{Key, Prefix};
use crate::queue::Queue;
use crate::settings::{Mode, Offer, default_effort, offered};
use crate::warmer::Warmer;

/// What can be set, and how it stands.
struct State {
    /// Whether the conversation is open.
    opened: bool,
    /// The provider that serves it.
    provider: Option<Provider>,
    /// The models the provider listed.
    models: Vec<String>,
    /// The model in use.
    model: String,
    /// The mode in use.
    mode: Mode,
    /// The mode that returns when plan mode ends.
    resume: Mode,
    /// The effort the reader picked, when they did.
    effort: Option<Effort>,
    /// Whether the agent has the checkpoint and rewind tools.
    checkpoints: bool,
    /// How long the provider is asked to keep the prompt cache entries requests write.
    retention: Retention,
    /// Whether the prompt cache is kept warm while the agent waits.
    warming: bool,
}

/// What a conversation's commands, turns and tools share.
pub(crate) struct Shared {
    /// Where events go.
    pub(crate) out: UnboundedSender<Event>,
    /// The questions waiting for the client.
    pub(crate) questions: Arc<Questions>,
    /// What is allowed without asking.
    pub(crate) approvals: Arc<Approvals>,
    /// The notes the agent keeps, and its request for a fresh context window.
    pub(crate) notebook: Arc<Notebook>,
    /// The checkpoint the agent marks, and its request to rewind to it.
    pub(crate) checkpoint: Arc<Checkpoint>,
    /// The commands and subagents running in the background.
    pub(crate) jobs: Arc<Jobs>,
    /// The language services `edit` and `write` ask, chosen when the conversation is opened.
    pub(crate) languages: Languages,
    /// The language services of the client, which are used when it says it offers them.
    pub(crate) client_languages: Arc<ClientLanguages>,
    /// The language services the program running the conversation gave it, which win over the client's and the harness's own.
    pub(crate) given_languages: Option<Arc<dyn LanguageService>>,
    /// The prompts the client sent while a turn was running.
    pub(crate) queue: Queue,
    /// The conversation's name and the history it is saved in.
    naming: Mutex<Naming>,
    /// Where the conversation keeps its logins.
    login: Mutex<Login>,
    /// What can be set, and how it stands.
    state: Mutex<State>,
    /// The system prompt and tool list of the conversation, once a turn made them.
    prefix: Mutex<Option<Arc<Prefix>>>,
    /// What keeps the prompt cache warm.
    pub(crate) warmer: Arc<Warmer>,
    /// The date turns tell the agent instead of today's, in tests.
    #[cfg(test)]
    pub(crate) today: Mutex<Option<String>>,
}

/// Where the conversation keeps its logins.
#[derive(Default)]
struct Login {
    /// The folder, or the default one when there is none.
    home: Option<std::path::PathBuf>,
}

/// The conversation's name, and the history file it and the settings are saved in.
#[derive(Default)]
struct Naming {
    /// The name, when it has one.
    title: Option<String>,
    /// The history the name and the settings are saved in, when the conversation is saved.
    journal: Option<Arc<omc_agent::Journal>>,
}

impl Shared {
    /// The shared state of a conversation that has not been opened.
    pub(crate) fn new(out: UnboundedSender<Event>) -> Arc<Self> {
        Self::with_languages(out, None)
    }

    /// The shared state of a conversation that has not been opened, whose tools ask `given` for language services when there is one.
    pub(crate) fn with_languages(
        out: UnboundedSender<Event>,
        given: Option<Arc<dyn LanguageService>>,
    ) -> Arc<Self> {
        let questions = Arc::new(Questions::new(out.clone()));
        let announce = out.clone();
        Arc::new(Self {
            languages: Languages::default(),
            client_languages: Arc::new(ClientLanguages::new(out.clone())),
            given_languages: given,
            queue: Queue::default(),
            out,
            approvals: Arc::new(Approvals::new(questions.clone(), false)),
            questions,
            notebook: Arc::new(Notebook::new()),
            checkpoint: Arc::new(Checkpoint::new()),
            jobs: Arc::new(Jobs::new(move |run| {
                let _ = announce.send(Event::Background(run));
            })),
            naming: Mutex::new(Naming::default()),
            login: Mutex::new(Login::default()),
            state: Mutex::new(State {
                opened: false,
                provider: None,
                models: Vec::new(),
                model: String::new(),
                mode: Mode::Asking,
                resume: Mode::Asking,
                effort: None,
                checkpoints: false,
                retention: Retention::Short,
                warming: true,
            }),
            prefix: Mutex::new(None),
            warmer: Arc::new(Warmer::default()),
            #[cfg(test)]
            today: Mutex::new(None),
        })
    }

    /// The prefix made for `key`, made with `freeze` when there is none for it yet; it replaces one made for anything else.
    pub(crate) fn prefix(&self, key: &Key, freeze: impl FnOnce() -> Prefix) -> Arc<Prefix> {
        let mut held = self.prefix.lock().unwrap_or_else(|held| held.into_inner());
        if let Some(prefix) = held.as_ref().filter(|prefix| prefix.key() == key) {
            return prefix.clone();
        }
        let made = Arc::new(freeze());
        *held = Some(made.clone());
        made
    }

    /// Runs `with` on the state.
    fn state<T>(&self, with: impl FnOnce(&mut State) -> T) -> T {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        with(&mut state)
    }

    /// Keeps the conversation's logins in `home`, or in the default folder when there is none.
    pub(crate) fn set_home(&self, home: Option<std::path::PathBuf>) {
        self.login
            .lock()
            .unwrap_or_else(|held| held.into_inner())
            .home = home;
    }

    /// Runs `work` against the folder the conversation keeps its logins in.
    pub(crate) async fn with_home<F: std::future::Future>(&self, work: F) -> F::Output {
        let home = self
            .login
            .lock()
            .unwrap_or_else(|held| held.into_inner())
            .home
            .clone();
        omc_auth::with_home(home, work).await
    }

    /// Takes the history the conversation is saved in, and the name it was saved with.
    pub(crate) fn attach(&self, journal: Option<Arc<omc_agent::Journal>>) {
        let mut naming = self.naming.lock().unwrap_or_else(|held| held.into_inner());
        naming.title = journal.as_ref().and_then(|journal| journal.title());
        naming.journal = journal;
    }

    /// The history the conversation is saved in, when it is saved.
    pub(crate) fn journal(&self) -> Option<Arc<omc_agent::Journal>> {
        self.naming
            .lock()
            .unwrap_or_else(|held| held.into_inner())
            .journal
            .clone()
    }

    /// The conversation's name, when it has one.
    pub(crate) fn title(&self) -> Option<String> {
        self.naming
            .lock()
            .unwrap_or_else(|held| held.into_inner())
            .title
            .clone()
    }

    /// Names the conversation `title`, saves the name and tells the client; `only_if_unnamed` leaves a name that is there alone. Whether the name was taken.
    pub(crate) fn name(&self, title: &str, only_if_unnamed: bool) -> bool {
        let mut naming = self.naming.lock().unwrap_or_else(|held| held.into_inner());
        if only_if_unnamed && naming.title.is_some() {
            return false;
        }
        if let Some(journal) = &naming.journal
            && let Err(error) = journal.save_title(title)
        {
            drop(naming);
            self.say(Event::Failed(format!("cannot save the name: {error}")));
            return false;
        }
        naming.title = Some(title.to_owned());
        drop(naming);
        self.say(Event::Titled(title.to_owned()));
        true
    }

    /// Saves `value` under `key` with the conversation, when it is saved; the harness's own record of what it last found out, such as what can be set.
    pub(crate) fn save_fact(&self, key: &str, value: &serde_json::Value) {
        let journal = self
            .naming
            .lock()
            .unwrap_or_else(|held| held.into_inner())
            .journal
            .clone();
        if let Some(Err(error)) = journal.map(|journal| journal.save_fact(key, value)) {
            self.say(Event::Failed(format!(
                "cannot save the conversation: {error}"
            )));
        }
    }

    /// Saves that the setting `id` was given the value `value`, when the conversation is saved.
    pub(crate) fn save_setting(&self, id: &str, value: &str) {
        let journal = self
            .naming
            .lock()
            .unwrap_or_else(|held| held.into_inner())
            .journal
            .clone();
        if let Some(Err(error)) = journal.map(|journal| journal.save_setting(id, value)) {
            self.say(Event::Failed(format!("cannot save the setting: {error}")));
        }
    }

    /// Sends `event` to the client.
    pub(crate) fn say(&self, event: Event) {
        let _ = self.out.send(event);
    }

    /// Records that the conversation is open with `provider`, `models` and `model`, in the mode `mode`; the system prompt and tools of a conversation opened before are forgotten and its cache is no longer kept warm.
    pub(crate) fn open(&self, provider: Provider, models: Vec<String>, model: String, mode: Mode) {
        *self.prefix.lock().unwrap_or_else(|held| held.into_inner()) = None;
        self.warmer.stop();
        self.state(|state| {
            state.opened = true;
            state.provider = Some(provider);
            state.models = models;
            state.model = model;
            state.mode = mode;
            state.resume = mode;
        });
        self.approvals.set_yolo(mode == Mode::Yolo);
    }

    /// Tells the client what can be set and how it stands, once the conversation is open.
    pub(crate) fn announce(&self) {
        let settings = self.state(|state| {
            let provider = state.provider.filter(|_| state.opened)?;
            let efforts = omc_providers::efforts(provider, &state.model);
            Some(offered(&Offer {
                models: &state.models,
                model: &state.model,
                mode: state.mode,
                efforts: &efforts,
                effort: state.effort,
                checkpoints: state.checkpoints,
                retention: omc_providers::cache_ttl(provider, &state.model, Retention::Long)
                    .map(|_| state.retention),
                warming: omc_providers::warmable(provider, &state.model, None)
                    .then_some(state.warming),
            }))
        });
        if let Some(settings) = settings {
            self.save_fact(
                "settings",
                &serde_json::to_value(&settings).unwrap_or_default(),
            );
            if let Some((provider, model)) = self.model() {
                self.save_fact(
                    "window",
                    &serde_json::json!(omc_providers::context_window(provider, &model)),
                );
            }
            self.say(Event::Settings(settings));
        }
    }

    /// The mode in use.
    pub(crate) fn mode(&self) -> Mode {
        self.state(|state| state.mode)
    }

    /// Puts the conversation in `mode`, remembering what to return to when it is plan mode.
    pub(crate) fn set_mode(&self, mode: Mode) {
        self.state(|state| {
            if mode == Mode::Plan && state.mode != Mode::Plan {
                state.resume = state.mode;
            }
            state.mode = mode;
        });
        let yolo = mode == Mode::Yolo;
        self.approvals.set_yolo(yolo);
    }

    /// Ends plan mode, returning to the mode it began in.
    pub(crate) fn leave_plan(&self) {
        let resume = self.state(|state| state.resume);
        self.set_mode(resume);
        self.announce();
    }

    /// The provider and model in use, once the conversation is open.
    pub(crate) fn model(&self) -> Option<(Provider, String)> {
        self.state(|state| {
            state
                .provider
                .map(|provider| (provider, state.model.clone()))
        })
    }

    /// Switches to `model`; an effort the new model does not take is dropped.
    pub(crate) fn set_model(&self, model: &str) {
        self.state(|state| {
            state.model = model.to_owned();
            if let (Some(provider), Some(effort)) = (state.provider, state.effort)
                && !omc_providers::efforts(provider, model).contains(&effort)
            {
                state.effort = None;
            }
        });
    }

    /// Picks the effort level `value` names, when the model in use takes it; whether it was taken.
    pub(crate) fn set_effort(&self, value: &str) -> bool {
        self.state(|state| {
            let (Some(provider), Some(effort)) = (state.provider, Effort::parse(value)) else {
                return false;
            };
            let taken = omc_providers::efforts(provider, &state.model).contains(&effort);
            if taken {
                state.effort = Some(effort);
            }
            taken
        })
    }

    /// Whether the agent has the checkpoint and rewind tools.
    pub(crate) fn checkpoints(&self) -> bool {
        self.state(|state| state.checkpoints)
    }

    /// Turns the checkpoint and rewind tools on or off for the turns that start from now on, and ends the checkpoint that is active.
    pub(crate) fn set_checkpoints(&self, enabled: bool) {
        self.state(|state| state.checkpoints = enabled);
        self.checkpoint.clear();
    }

    /// How long the provider is asked to keep the prompt cache entries requests write.
    pub(crate) fn retention(&self) -> Retention {
        self.state(|state| state.retention)
    }

    /// Asks the provider to keep the prompt cache entries of the requests from now on as long as `retention` says.
    pub(crate) fn set_retention(&self, retention: Retention) {
        self.state(|state| state.retention = retention);
    }

    /// Whether the prompt cache is kept warm while the agent waits.
    pub(crate) fn warming(&self) -> bool {
        self.state(|state| state.warming)
    }

    /// Turns keeping the prompt cache warm on or off; turning it off stops it at once.
    pub(crate) fn set_warming(&self, enabled: bool) {
        self.state(|state| state.warming = enabled);
        if !enabled {
            self.warmer.stop();
        }
    }

    /// The effort the next turn thinks at; nothing when the model takes none.
    pub(crate) fn effort_now(&self) -> Option<Effort> {
        self.state(|state| {
            let provider = state.provider?;
            let efforts = omc_providers::efforts(provider, &state.model);
            state
                .effort
                .filter(|effort| efforts.contains(effort))
                .or_else(|| default_effort(&efforts))
        })
    }
}

/// The reader, as the tools that report to them and ask them see it.
pub(crate) struct SessionHost {
    /// The conversation the reader is in.
    pub(crate) shared: Arc<Shared>,
}

#[async_trait]
impl Host for SessionHost {
    /// Sends the plan to the client.
    fn plan(&self, steps: Vec<PlanStep>) {
        self.shared.say(Event::Plan(steps));
    }

    /// Names the conversation when it has no name.
    fn name(&self, title: &str) -> bool {
        self.shared.name(title, true)
    }

    /// Puts the form to the client and waits for its replies.
    async fn inquire(&self, questions: Vec<Question>) -> Option<Vec<Reply>> {
        self.shared.questions.inquire(questions).await
    }

    /// Puts the plan to the client; when it is approved, plan mode ends and the settings are announced.
    async fn approve_plan(&self, plan: &str) -> bool {
        let approved = self.shared.questions.propose(plan).await;
        if approved {
            self.shared.leave_plan();
        }
        approved
    }
}

#[cfg(test)]
/// Tests for the shared state.
mod tests {
    use super::*;

    /// Opens a conversation on Claude with model `m` and returns it with the events it sends.
    fn opened() -> (Arc<Shared>, tokio::sync::mpsc::UnboundedReceiver<Event>) {
        let (out, events) = tokio::sync::mpsc::unbounded_channel();
        let shared = Shared::new(out);
        shared.open(
            Provider::Claude,
            vec!["m".into(), "n".into()],
            "m".into(),
            Mode::Asking,
        );
        (shared, events)
    }

    /// The settings the last announcement carried.
    fn announced(
        events: &mut tokio::sync::mpsc::UnboundedReceiver<Event>,
    ) -> Vec<omc_wire::Setting> {
        let mut last = Vec::new();
        while let Ok(event) = events.try_recv() {
            if let Event::Settings(settings) = event {
                last = settings;
            }
        }
        last
    }

    /// The effort setting follows what the model takes, and a pick applies to the next turn.
    #[test]
    fn effort_is_offered_from_the_models_levels() {
        let (shared, mut events) = opened();
        shared.announce();
        let settings = announced(&mut events);
        let levels = omc_providers::efforts(Provider::Claude, "m");
        let effort = settings.iter().find(|setting| setting.id == "effort");
        assert_eq!(effort.is_some(), !levels.is_empty());
        if let Some(level) = levels.last() {
            assert!(shared.set_effort(level.as_str()));
            assert_eq!(shared.effort_now(), Some(*level));
            shared.announce();
            let settings = announced(&mut events);
            let effort = settings
                .iter()
                .find(|setting| setting.id == "effort")
                .unwrap();
            assert_eq!(effort.current, level.as_str());
        }
        assert!(!shared.set_effort("nonsense"));
    }

    /// Changing the model announces the settings again with the new model current.
    #[test]
    fn changing_the_model_refreshes_the_settings() {
        let (shared, mut events) = opened();
        shared.set_model("n");
        shared.announce();
        let settings = announced(&mut events);
        assert_eq!(
            settings.iter().find(|s| s.id == "model").unwrap().current,
            "n"
        );
        assert_eq!(shared.model(), Some((Provider::Claude, "n".to_owned())));
    }

    /// The checkpoint tools are off until the setting turns them on, and turning the setting either way ends an active checkpoint.
    #[test]
    fn checkpoints_are_a_setting() {
        let (shared, mut events) = opened();
        assert!(!shared.checkpoints());
        shared.checkpoint.begin().unwrap();
        shared.set_checkpoints(true);
        assert!(shared.checkpoints());
        assert!(!shared.checkpoint.active());
        shared.announce();
        let settings = announced(&mut events);
        let setting = settings.iter().find(|s| s.id == "checkpoints").unwrap();
        assert_eq!(setting.current, "on");
    }

    /// Plan mode remembers the mode it came from and gives it back.
    #[test]
    fn plan_mode_returns_to_the_previous_mode() {
        let (shared, mut events) = opened();
        shared.set_mode(Mode::Yolo);
        shared.set_mode(Mode::Plan);
        assert!(!shared.approvals.yolo());
        shared.leave_plan();
        assert_eq!(shared.mode(), Mode::Yolo);
        assert!(shared.approvals.yolo());
        let settings = announced(&mut events);
        assert_eq!(
            settings.iter().find(|s| s.id == "mode").unwrap().current,
            "yolo"
        );
    }

    /// The agent names an unnamed conversation once and the reader's name replaces it; both are saved and told to the client.
    #[test]
    fn naming_is_first_come_for_the_agent_and_always_for_the_reader() {
        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../build/tests/omc-session/naming.jsonl");
        let _ = std::fs::remove_file(&path);
        let (out, mut events) = tokio::sync::mpsc::unbounded_channel();
        let shared = Shared::new(out);
        let (journal, _) = omc_agent::Journal::open(&path, None).unwrap();
        shared.attach(Some(journal.clone()));
        assert!(shared.name("Agent name", true));
        assert!(!shared.name("Another", true));
        assert_eq!(shared.title().as_deref(), Some("Agent name"));
        assert!(shared.name("Reader name", false));
        assert_eq!(journal.title().as_deref(), Some("Reader name"));
        shared.save_setting("model", "m");
        assert_eq!(
            journal.settings(),
            vec![("model".to_owned(), "m".to_owned())]
        );
        let told: Vec<Event> = std::iter::from_fn(|| events.try_recv().ok()).collect();
        assert_eq!(
            told,
            vec![
                Event::Titled("Agent name".to_owned()),
                Event::Titled("Reader name".to_owned())
            ]
        );
        drop(journal);
        shared.attach(None);
        assert_eq!(shared.title(), None);
    }
}
