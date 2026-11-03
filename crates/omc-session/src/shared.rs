//! The state of a conversation that turns, tools and commands all reach.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use omc_agent::Approvals;
use omc_providers::Effort;
use omc_providers::Provider;
use omc_tools::{Host, Notebook};
use omc_wire::{Event, PlanStep, Question, Reply};
use tokio::sync::mpsc::UnboundedSender;

use crate::bridge::Questions;
use crate::settings::{Mode, Offer, default_effort, offered};

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
    /// What can be set, and how it stands.
    state: Mutex<State>,
}

impl Shared {
    /// The shared state of a conversation that has not been opened.
    pub(crate) fn new(out: UnboundedSender<Event>) -> Arc<Self> {
        let questions = Arc::new(Questions::new(out.clone()));
        Arc::new(Self {
            out,
            approvals: Arc::new(Approvals::new(questions.clone(), false)),
            questions,
            notebook: Arc::new(Notebook::new()),
            state: Mutex::new(State {
                opened: false,
                provider: None,
                models: Vec::new(),
                model: String::new(),
                mode: Mode::Asking,
                resume: Mode::Asking,
                effort: None,
            }),
        })
    }

    /// Runs `with` on the state.
    fn state<T>(&self, with: impl FnOnce(&mut State) -> T) -> T {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        with(&mut state)
    }

    /// Sends `event` to the client.
    pub(crate) fn say(&self, event: Event) {
        let _ = self.out.send(event);
    }

    /// Records that the conversation is open with `provider`, `models` and `model`, in the mode `mode`.
    pub(crate) fn open(&self, provider: Provider, models: Vec<String>, model: String, mode: Mode) {
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
            }))
        });
        if let Some(settings) = settings {
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
}
