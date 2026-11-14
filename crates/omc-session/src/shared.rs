//! The state of a conversation that turns, tools and commands all reach.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use omc_agent::Approvals;
use omc_providers::Effort;
use omc_providers::{Provider, Retention};
use omc_tools::{Checkpoint, Host, Jobs, LanguageService, Languages, Notebook, PlanVerdict};
use omc_wire::{Event, PlanStep, Question, Reply};
use tokio::sync::mpsc::UnboundedSender;

use crate::bridge::Questions;
use crate::languages::ClientLanguages;
use crate::lifecycle::Lifecycle;
use crate::plans::Plans;
use crate::prefix::{Key, Prefix};
use crate::queue::Queue;
use crate::settings::{Mode, Offer, SAME, default_effort, offered};
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
    /// The model used while plan mode is on, when it is another than `model`.
    plan_model: Option<String>,
    /// The mode in use.
    mode: Mode,
    /// The mode that returns when plan mode ends.
    resume: Mode,
    /// The effort the reader picked, when they did.
    effort: Option<Effort>,
    /// Whether the agent has the checkpoint and rewind tools.
    checkpoints: bool,
    /// Whether the files are kept before and after each turn.
    snapshots: bool,
    /// How long the provider is asked to keep the prompt cache entries requests write.
    retention: Retention,
    /// Whether the prompt cache is kept warm while the agent waits.
    warming: bool,
    /// Whether the agent has its memory, when the conversation has memories to reach.
    memory: bool,
    /// The memories of the conversation's project and of the user, when memory is not turned off for the process.
    memories: Option<Arc<omc_memory::Memories>>,
}

/// What a conversation's commands, turns and tools share.
pub(crate) struct Shared {
    /// Where events go.
    pub(crate) out: UnboundedSender<Event>,
    /// The questions waiting for the client.
    pub(crate) questions: Arc<Questions>,
    /// The hooks that run around the conversation, and the context of its last start the model has not been told yet.
    pub(crate) lifecycle: Arc<Lifecycle>,
    /// What is allowed without asking.
    pub(crate) approvals: Arc<Approvals>,
    /// The notes the agent keeps, and its request for a fresh context window.
    pub(crate) notebook: Arc<Notebook>,
    /// The checkpoint the agent marks, and its request to rewind to it.
    pub(crate) checkpoint: Arc<Checkpoint>,
    /// The plans the agent submitted: where they are saved, and the one the reader approved.
    pub(crate) plans: Arc<Plans>,
    /// The commands and subagents running in the background.
    pub(crate) jobs: Arc<Jobs>,
    /// The seats subagents take to run, shared by every level of subagents and every turn, so that only so many run at once.
    pub(crate) throttle: Arc<omc_agent::Throttle>,
    /// The subagents running in the conversation, at every level and in every turn, by which the reader stops one.
    pub(crate) roster: Arc<omc_agent::Roster>,
    /// The long-lived subagents of the conversation, alive across turns until killed or the conversation ends.
    pub(crate) crew: Arc<omc_agent::Crew>,
    /// The debugging sessions the `debug` tool keeps across calls.
    pub(crate) debuggers: Arc<omc_dap::Sessions>,
    /// The language services `edit` and `write` ask, chosen when the conversation is opened.
    pub(crate) languages: Languages,
    /// The language services of the client, which are used when it says it offers them.
    pub(crate) client_languages: Arc<ClientLanguages>,
    /// The language services the program running the conversation gave it, which win over the client's and the harness's own.
    pub(crate) given_languages: Option<Arc<dyn LanguageService>>,
    /// The prompts the client sent while a turn was running.
    pub(crate) queue: Queue,
    /// The text of the reply being written, which the saved history does not hold until the reply is whole; kept so that a turn stopped midway keeps what it had said.
    pub(crate) streamed: Arc<Mutex<String>>,
    /// The conversation's name and the history it is saved in.
    naming: Mutex<Naming>,
    /// Where the conversation keeps its logins.
    login: Mutex<Login>,
    /// What can be set, and how it stands.
    state: Mutex<State>,
    /// The system prompt and tool list of the conversation, once a turn made them.
    prefix: Mutex<Option<Arc<Prefix>>>,
    /// The outline of the working directory the system prompts carry, once a turn made it.
    tree: Mutex<Option<Arc<str>>>,
    /// What keeps the prompt cache warm.
    pub(crate) warmer: Arc<Warmer>,
    /// The goal the agent works toward across turns, when the reader set one.
    pub(crate) goals: crate::goal::Goals,
    /// What the conversation cost so far.
    pub(crate) ledger: crate::spend::Ledger,
    /// Where the model slots are kept: the user's file, and the project's once a trusted project is open.
    slot_files: Mutex<crate::slots::SlotFiles>,
    /// The date turns tell the agent instead of today's, in tests.
    #[cfg(test)]
    pub(crate) today: Mutex<Option<String>>,
    /// The model turns and questions on the side ask instead of the provider's, in tests.
    #[cfg(test)]
    pub(crate) model_override: Mutex<Option<Arc<dyn omc_providers::ChatModel>>>,
    /// The models the picker gives for model ids instead of the providers', in tests.
    #[cfg(test)]
    pub(crate) named_models:
        Mutex<std::collections::HashMap<String, Arc<dyn omc_providers::ChatModel>>>,
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
        let lifecycle = Arc::new(Lifecycle::default());
        let questions = Arc::new(Questions::new(out.clone()).heeded_by(lifecycle.clone()));
        let announce = out.clone();
        Arc::new(Self {
            languages: Languages::default(),
            client_languages: Arc::new(ClientLanguages::new(out.clone())),
            given_languages: given,
            queue: Queue::default(),
            streamed: Arc::default(),
            out,
            approvals: Arc::new(Approvals::new(questions.clone(), false)),
            questions,
            lifecycle,
            notebook: Arc::new(Notebook::new()),
            checkpoint: Arc::new(Checkpoint::new()),
            plans: Arc::new(Plans::default()),
            jobs: Arc::new(Jobs::new(move |run| {
                let _ = announce.send(Event::Background(run));
            })),
            throttle: Arc::new(omc_agent::Throttle::default()),
            roster: Arc::new(omc_agent::Roster::default()),
            crew: Arc::new(omc_agent::Crew::default()),
            debuggers: Arc::new(omc_dap::Sessions::new()),
            naming: Mutex::new(Naming::default()),
            login: Mutex::new(Login::default()),
            state: Mutex::new(State {
                opened: false,
                provider: None,
                models: Vec::new(),
                model: String::new(),
                plan_model: None,
                mode: Mode::Asking,
                resume: Mode::Asking,
                effort: None,
                checkpoints: false,
                snapshots: true,
                retention: Retention::Short,
                warming: true,
                memory: true,
                memories: None,
            }),
            prefix: Mutex::new(None),
            tree: Mutex::new(None),
            warmer: Arc::new(Warmer::default()),
            goals: crate::goal::Goals::default(),
            ledger: crate::spend::Ledger::default(),
            slot_files: Mutex::new(if cfg!(test) {
                crate::slots::SlotFiles::default()
            } else {
                crate::slots::SlotFiles::locate(None)
            }),
            #[cfg(test)]
            today: Mutex::new(None),
            #[cfg(test)]
            model_override: Mutex::new(None),
            #[cfg(test)]
            named_models: Mutex::new(std::collections::HashMap::new()),
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

    /// The outline of `directory` the system prompts of the conversation carry, made the first time it is asked for after the conversation was opened and kept unchanged after that, so that a system prompt made again for another model stays the same and the agent's subagents see the same outline.
    ///
    /// The walk that makes it runs on a thread for blocking work, with nothing locked, so it holds up neither the other tasks nor anyone else asking; when two ask at once, the outline that is ready first is kept.
    pub(crate) async fn tree(&self, directory: &std::path::Path) -> Arc<str> {
        if let Some(made) = self.made_tree() {
            return made;
        }
        let directory = directory.to_path_buf();
        let walked = tokio::task::spawn_blocking(move || omc_context::workspace_tree(&directory))
            .await
            .unwrap_or_default();
        self.tree
            .lock()
            .unwrap_or_else(|held| held.into_inner())
            .get_or_insert_with(|| Arc::from(walked))
            .clone()
    }

    /// The outline of the working directory, once it has been made.
    fn made_tree(&self) -> Option<Arc<str>> {
        self.tree
            .lock()
            .unwrap_or_else(|held| held.into_inner())
            .clone()
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

    /// The folder the conversation keeps its logins in, or nothing for the default one.
    pub(crate) fn home(&self) -> Option<std::path::PathBuf> {
        self.login
            .lock()
            .unwrap_or_else(|held| held.into_inner())
            .home
            .clone()
    }

    /// Runs `work` against the folder the conversation keeps its logins in.
    pub(crate) async fn with_home<F: std::future::Future>(&self, work: F) -> F::Output {
        omc_auth::with_home(self.home(), work).await
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

    /// Tells the client how the goal `goal` stands and saves it with the conversation; a cleared goal is saved as none.
    pub(crate) fn announce_goal(&self, goal: &omc_wire::Goal) {
        let saved = match goal.state {
            omc_wire::GoalState::Cleared => serde_json::Value::Null,
            _ => serde_json::to_value(goal).unwrap_or_default(),
        };
        self.save_fact(crate::goal::GOAL_FACT, &saved);
        self.say(Event::Goal(goal.clone()));
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

    /// Records that the conversation is open with `provider`, `models` and `model`, in the mode `mode`; the system prompt, tools and workspace outline of a conversation opened before are forgotten and its cache is no longer kept warm.
    pub(crate) fn open(&self, provider: Provider, models: Vec<String>, model: String, mode: Mode) {
        *self.prefix.lock().unwrap_or_else(|held| held.into_inner()) = None;
        *self.tree.lock().unwrap_or_else(|held| held.into_inner()) = None;
        self.warmer.stop();
        self.state(|state| {
            state.opened = true;
            state.provider = Some(provider);
            state.models = models;
            state.model = model;
            state.plan_model = None;
            state.mode = mode;
            state.resume = mode;
        });
        self.plans.set_approved(None);
        self.plans.set_autosave(false);
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
                plan_model: state.plan_model.as_deref(),
                autosave: self.plans.autosave(),
                mode: state.mode,
                efforts: &efforts,
                effort: state.effort,
                checkpoints: state.checkpoints,
                snapshots: state.snapshots,
                retention: omc_providers::cache_ttl(provider, &state.model, Retention::Long)
                    .map(|_| state.retention),
                warming: omc_providers::warmable(provider, &state.model, None)
                    .then_some(state.warming),
                memory: state.memories.as_ref().map(|_| state.memory),
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

    /// The model used while plan mode is on, when one other than the model in use is set.
    pub(crate) fn plan_model(&self) -> Option<String> {
        self.state(|state| {
            state
                .plan_model
                .clone()
                .filter(|planner| *planner != state.model)
        })
    }

    /// Uses `value` while plan mode is on from the next request, or the model in use when it is [`SAME`] or empty; whether it was taken, which it always is. Another model stops warming the last request when plan mode is on, since the next one goes to the new model.
    pub(crate) fn set_plan_model(&self, value: &str) -> bool {
        let value = value.trim();
        let wanted = (!value.is_empty() && value != SAME).then(|| value.to_owned());
        let changed = self.state(|state| {
            let changed = state.plan_model != wanted;
            state.plan_model = wanted;
            changed && state.mode == Mode::Plan
        });
        if changed {
            self.warmer.stop();
        }
        true
    }

    /// Saves `plan`, as the agent submitted it, beside the conversation's history when it is saved and in the project when autosave is on, and tells the client when it could not be saved.
    pub(crate) fn save_plan(&self, plan: &str) {
        let history = self.journal().map(|journal| journal.path().to_path_buf());
        if let Err(error) = self.plans.save(plan, history.as_deref()) {
            self.say(Event::Failed(error));
        }
    }

    /// Where the model slots are kept now.
    pub(crate) fn slot_files(&self) -> crate::slots::SlotFiles {
        self.slot_files
            .lock()
            .unwrap_or_else(|held| held.into_inner())
            .clone()
    }

    /// Keeps the model slots in `files` from now on.
    pub(crate) fn set_slot_files(&self, files: crate::slots::SlotFiles) {
        *self
            .slot_files
            .lock()
            .unwrap_or_else(|held| held.into_inner()) = files;
    }

    /// The model slots as their files and the environment set them now, resolved for the conversation's provider when one is open; a file that cannot be read is reported on standard error and sets nothing.
    pub(crate) fn slot_layers(&self) -> crate::slots::Layers {
        let provider = self.model().map(|(provider, _)| provider.def().id);
        self.slot_files()
            .load()
            .unwrap_or_else(|reason| {
                eprintln!("model slots ignored: {reason}");
                crate::slots::Layers::with_env(Default::default(), None)
            })
            .for_provider(provider)
    }

    /// The models the provider listed when the conversation was opened, best first.
    pub(crate) fn models_listed(&self) -> Vec<String> {
        self.state(|state| state.models.clone())
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

    /// Picks the effort level `value` names, when the model in use takes it; whether it was taken. A new effort stops warming the last request, which was sent with the old one.
    pub(crate) fn set_effort(&self, value: &str) -> bool {
        let (taken, changed) = self.state(|state| {
            let (Some(provider), Some(effort)) = (state.provider, Effort::parse(value)) else {
                return (false, false);
            };
            let taken = omc_providers::efforts(provider, &state.model).contains(&effort);
            let changed = taken && state.effort != Some(effort);
            if taken {
                state.effort = Some(effort);
            }
            (taken, changed)
        });
        if changed {
            self.warmer.stop();
        }
        taken
    }

    /// Whether the agent has the checkpoint and rewind tools.
    pub(crate) fn checkpoints(&self) -> bool {
        self.state(|state| state.checkpoints)
    }

    /// Turns the checkpoint and rewind tools on or off for the turns that start from now on, and ends the checkpoint that is active; turning them either way stops warming the last request, whose tool list the next turn no longer sends.
    pub(crate) fn set_checkpoints(&self, enabled: bool) {
        let was = self.state(|state| std::mem::replace(&mut state.checkpoints, enabled));
        if was != enabled {
            self.warmer.stop();
        }
        self.checkpoint.clear();
    }

    /// Whether the files are kept before and after each turn.
    pub(crate) fn snapshots(&self) -> bool {
        self.state(|state| state.snapshots)
    }

    /// Turns keeping the files before and after each turn on or off for the turns that start from now on.
    pub(crate) fn set_snapshots(&self, enabled: bool) {
        self.state(|state| state.snapshots = enabled);
    }

    /// Gives the conversation `memories`, the memories of its project and of the user, or none when memory is turned off for the process or has no folder.
    pub(crate) fn set_memories(&self, memories: Option<Arc<omc_memory::Memories>>) {
        self.state(|state| state.memories = memories);
    }

    /// The memories the turns that start now are given: nothing when the setting is off or there are none.
    pub(crate) fn memories_now(&self) -> Option<Arc<omc_memory::Memories>> {
        self.state(|state| state.memories.clone().filter(|_| state.memory))
    }

    /// Turns the agent's memory on or off for the turns that start from now on; turning it either way stops warming the last request, whose tool list the next turn no longer sends.
    pub(crate) fn set_memory(&self, enabled: bool) {
        let was = self.state(|state| std::mem::replace(&mut state.memory, enabled));
        if was != enabled {
            self.warmer.stop();
        }
    }

    /// Changes the effort the reader picked and the checkpoint setting with `with`, without anything else that changing them does.
    #[cfg(test)]
    pub(crate) fn change_quietly(&self, with: impl FnOnce(&mut Option<Effort>, &mut bool)) {
        self.state(|state| with(&mut state.effort, &mut state.checkpoints));
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

    /// Saves the plan and puts it to the client; when it is approved, it becomes the plan subagents are told of, plan mode ends and the settings are announced.
    async fn approve_plan(&self, plan: &str) -> PlanVerdict {
        self.shared.save_plan(plan);
        let verdict = self.shared.questions.propose(plan).await;
        if verdict.approved() {
            self.shared.plans.set_approved(Some(plan.trim()));
            self.shared.leave_plan();
        }
        verdict
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

    /// Memory is offered as a setting, on, only when the conversation has memories; turning it off withholds them and stops warming the last request.
    #[tokio::test]
    async fn memory_is_a_setting() {
        let (shared, mut events) = opened();
        shared.announce();
        assert!(!announced(&mut events).iter().any(|s| s.id == "memory"));
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../build/tests/omc-session/shared-memory");
        let memories = Arc::new(omc_memory::Memories::new(
            &omc_memory::Store::new(root),
            omc_memory::Project::of(std::path::Path::new(env!("CARGO_MANIFEST_DIR"))),
            None,
        ));
        shared.set_memories(Some(memories));
        shared.announce();
        let settings = announced(&mut events);
        let setting = settings.iter().find(|s| s.id == "memory").unwrap();
        assert_eq!(setting.current, "on");
        assert!(shared.memories_now().is_some());
        shared.warmer.arm();
        shared.set_memory(false);
        assert!(!shared.warmer.armed());
        assert!(shared.memories_now().is_none());
        shared.announce();
        let settings = announced(&mut events);
        assert_eq!(
            settings.iter().find(|s| s.id == "memory").unwrap().current,
            "off"
        );
    }

    /// Another effort or turning the checkpoint tools on or off stops warming the last request; picking the same effort again does not.
    #[tokio::test]
    async fn effort_and_checkpoints_stop_warming() {
        let (shared, _events) = opened();
        let levels = omc_providers::efforts(Provider::Claude, "m");
        if let [first, second, ..] = levels[..] {
            assert!(shared.set_effort(first.as_str()));
            shared.warmer.arm();
            assert!(shared.set_effort(first.as_str()));
            assert!(shared.warmer.armed());
            assert!(shared.set_effort(second.as_str()));
            assert!(!shared.warmer.armed());
        }
        shared.warmer.arm();
        shared.set_checkpoints(false);
        assert!(shared.warmer.armed());
        shared.set_checkpoints(true);
        assert!(!shared.warmer.armed());
    }

    /// The outline is walked off the runtime's threads: the task asking for it gives way while the walk runs, and the outline it gets is kept.
    ///
    /// The folder walked is a small one of the test's own, so that the walk stays well inside its time limit however busy the machine is with the other tests.
    #[tokio::test(flavor = "current_thread")]
    async fn the_outline_is_walked_without_holding_up_other_tasks() {
        let (shared, _events) = opened();
        let directory = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../build/tests/omc-session/outline");
        std::fs::create_dir_all(directory.join("crates")).unwrap();
        let order = Mutex::new(Vec::new());
        let (made, ()) = tokio::join!(
            async {
                let made = shared.tree(&directory).await;
                order.lock().unwrap().push("tree");
                made
            },
            async { order.lock().unwrap().push("other") }
        );
        assert_eq!(*order.lock().unwrap(), vec!["other", "tree"]);
        assert!(made.contains("crates/"));
        assert!(Arc::ptr_eq(&made, &shared.tree(&directory).await));
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

    /// The plan model is used only when it is another than the model in use, `same` clears it, and opening the conversation again forgets it.
    #[tokio::test]
    async fn the_plan_model_is_another_model_or_none() {
        let (shared, mut events) = opened();
        assert_eq!(shared.plan_model(), None);
        assert!(shared.set_plan_model("n"));
        assert_eq!(shared.plan_model().as_deref(), Some("n"));
        shared.announce();
        let settings = announced(&mut events);
        let setting = settings.iter().find(|s| s.id == "plan_model").unwrap();
        assert_eq!(setting.current, "n");
        shared.set_model("n");
        assert_eq!(shared.plan_model(), None);
        shared.set_model("m");
        shared.set_mode(Mode::Plan);
        shared.warmer.arm();
        assert!(shared.set_plan_model("same"));
        assert!(!shared.warmer.armed());
        assert_eq!(shared.plan_model(), None);
        shared.set_plan_model("n");
        shared.open(Provider::Claude, Vec::new(), "m".into(), Mode::Asking);
        assert_eq!(shared.plan_model(), None);
    }

    /// A submitted plan is saved beside the conversation's history, numbered.
    #[test]
    fn plans_are_saved_with_the_conversation() {
        let folder = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../build/tests/omc-session/shared-plans");
        let _ = std::fs::remove_dir_all(&folder);
        let (shared, _events) = opened();
        shared.save_plan("# Unsaved");
        let (journal, _) = omc_agent::Journal::open(&folder.join("c.jsonl"), None).unwrap();
        shared.attach(Some(journal));
        shared.save_plan("# One");
        shared.save_plan("# Two");
        let plans = folder.join("c.plans");
        assert_eq!(
            std::fs::read_to_string(plans.join("plan-2.md")).unwrap(),
            "# Two\n"
        );
        assert!(!plans.join("plan-3.md").exists());
        shared.attach(None);
        let _ = std::fs::remove_dir_all(&folder);
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
