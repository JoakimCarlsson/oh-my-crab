//! One turn of the conversation, owned so it can run on a task of its own.

use std::sync::Arc;

use omc_agent::{Context, EventSink, Live, run_turn};
use omc_auth::{AuthError, Provider};
use omc_context::{Discovered, Environment, Locations};
use omc_providers::{ChatModel, Message, RemoteModel};
use omc_tools::{Host, Registry};

use crate::settings::Mode;
use crate::shared::{SessionHost, Shared};
use crate::toolset::{Inputs, Toolset};
use crate::translate::translate;

/// How a turn ended.
pub(crate) enum Ended {
    /// It answered; this is the conversation so far.
    Done(Vec<Message>),
    /// The account needs signing in.
    SignedOut(String),
    /// It failed for this reason.
    Failed(String),
}

/// Everything one turn needs.
pub(crate) struct Turn {
    /// The provider that serves the model.
    pub(crate) provider: Provider,
    /// The model to ask.
    pub(crate) model: String,
    /// The tools before the ones that talk to the reader are added.
    pub(crate) base: Arc<Registry>,
    /// The names of the tools the agent is not given.
    pub(crate) denied: Arc<Vec<String>>,
    /// What was found on disk.
    pub(crate) discovered: Arc<Discovered>,
    /// Where the work is.
    pub(crate) locations: Arc<Locations>,
    /// What the conversation shares.
    pub(crate) shared: Arc<Shared>,
    /// What was said before this turn.
    pub(crate) history: Vec<Message>,
}

impl Turn {
    /// Runs the turn for `prompt` against the hosted model and says how it ended.
    pub(crate) async fn run(self, prompt: String) -> Ended {
        let credentials = match omc_auth::credentials_for(self.provider).await {
            Ok(credentials) => credentials,
            Err(AuthError::NotLoggedIn) => return Ended::SignedOut("not logged in".to_owned()),
            Err(error) => return Ended::Failed(error.to_string()),
        };
        let model = RemoteModel {
            provider: self.provider,
            credentials,
            model: self.model.clone(),
        };
        self.run_with(Arc::new(model), prompt).await
    }

    /// Runs the turn for `prompt` against `model` and says how it ended.
    pub(crate) async fn run_with(mut self, model: Arc<dyn ChatModel>, prompt: String) -> Ended {
        let window = omc_providers::context_window(self.provider, &self.model);
        let effort = self.shared.effort_now();
        let out = self.shared.out.clone();
        let events: Arc<EventSink> = Arc::new(move |event| {
            if let Some(event) = translate(event, window) {
                let _ = out.send(event);
            }
        });
        let host: Arc<dyn Host> = Arc::new(SessionHost {
            shared: self.shared.clone(),
        });
        let toolset = Toolset::build(&Inputs {
            base: &self.base,
            denied: &self.denied,
            host,
            model: model.clone(),
            approvals: self.shared.approvals.clone(),
            events: events.clone(),
            effort,
            discovered: self.discovered.clone(),
            environment: Arc::new(Environment::detect(&self.locations, &self.model)),
        });
        let shared = self.shared.clone();
        let refresh = move || toolset.live(shared.mode() == Mode::Plan);
        let initial: Live = refresh();
        let context = Context {
            model: model.as_ref(),
            registry: &initial.registry,
            approvals: &self.shared.approvals,
            system: &initial.system,
            effort,
            refresh: Some(&refresh),
        };
        match run_turn(&context, &mut self.history, prompt, events.as_ref()).await {
            Ok(()) => Ended::Done(self.history),
            Err(error) => Ended::Failed(error.to_string()),
        }
    }
}

#[cfg(test)]
/// Tests for running a turn through the session's tools and modes.
mod tests {
    use std::sync::Mutex;

    use async_trait::async_trait;
    use omc_providers::{ProviderError, Reply, Request, Sink, ToolCall};
    use omc_wire::Event;
    use serde_json::json;

    use super::*;

    /// A model that calls `exit_plan_mode` on its first request, then answers, and keeps what it was offered.
    struct Planner {
        /// The tool names and whether plan mode was in the system prompt, at each request.
        seen: Mutex<Vec<(Vec<String>, bool)>>,
    }

    #[async_trait]
    impl ChatModel for Planner {
        /// Plans first, then says it is done.
        async fn complete(
            &self,
            request: Request<'_>,
            _: &mut dyn Sink,
        ) -> Result<Reply, ProviderError> {
            let names = request.tools.iter().map(|tool| tool.name.clone()).collect();
            let mut seen = self.seen.lock().unwrap();
            seen.push((names, request.system.contains("Plan mode active")));
            if seen.len() == 1 {
                return Ok(Reply {
                    tool_calls: vec![ToolCall {
                        id: "p".into(),
                        name: "exit_plan_mode".into(),
                        arguments: json!({"plan": "# Plan\n1. do it"}),
                    }],
                    ..Reply::default()
                });
            }
            Ok(Reply {
                text: "implemented".into(),
                ..Reply::default()
            })
        }
    }

    /// Plan mode offers read-only tools; approving the plan switches the mode back, announces it, and the same turn goes on with writing tools.
    #[tokio::test]
    async fn approving_a_plan_ends_plan_mode_mid_turn() {
        let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../build/tests/omc-session/turn-plan");
        std::fs::create_dir_all(&dir).unwrap();
        let (out, mut events) = tokio::sync::mpsc::unbounded_channel();
        let shared = Shared::new(out);
        shared.open(Provider::Claude, vec!["m".into()], "m".into(), Mode::Yolo);
        shared.set_mode(Mode::Plan);
        assert!(!shared.approvals.yolo());
        let turn = Turn {
            provider: Provider::Claude,
            model: "m".into(),
            base: Arc::new(Registry::builtin(&dir).unwrap()),
            denied: Arc::new(Vec::new()),
            discovered: Arc::new(Discovered::default()),
            locations: Arc::new(Locations {
                directory: dir,
                root: None,
                home: None,
                claude: None,
                codex: None,
            }),
            shared: shared.clone(),
            history: Vec::new(),
        };
        let model = Arc::new(Planner {
            seen: Mutex::new(Vec::new()),
        });
        let running = tokio::spawn(turn.run_with(model.clone(), "plan it".into()));
        let ticket = loop {
            match events.recv().await {
                Some(Event::Ask(asked)) => {
                    assert_eq!(asked.plan.as_deref(), Some("# Plan\n1. do it"));
                    break asked.ticket;
                }
                Some(_) => {}
                None => panic!("no plan was proposed"),
            }
        };
        shared.questions.answer(ticket, Some("allow"));
        let Ended::Done(history) = running.await.unwrap() else {
            panic!("turn failed");
        };
        assert_eq!(history.len(), 4);
        assert_eq!(shared.mode(), Mode::Yolo);
        assert!(shared.approvals.yolo());
        let mut announced = None;
        while let Ok(event) = events.try_recv() {
            if let Event::Settings(settings) = event {
                announced = Some(settings);
            }
        }
        let settings = announced.expect("settings were announced");
        assert_eq!(
            settings.iter().find(|s| s.id == "mode").unwrap().current,
            "yolo"
        );
        let seen = model.seen.lock().unwrap();
        assert!(seen[0].1 && !seen[1].1);
        assert!(
            seen[0].0.contains(&"exit_plan_mode".to_owned())
                && !seen[0].0.contains(&"write".to_owned())
        );
        assert!(
            !seen[1].0.contains(&"exit_plan_mode".to_owned())
                && seen[1].0.contains(&"write".to_owned())
        );
    }

    /// Rejecting the plan keeps plan mode on.
    #[tokio::test]
    async fn rejecting_a_plan_keeps_plan_mode() {
        let (out, mut events) = tokio::sync::mpsc::unbounded_channel();
        let shared = Shared::new(out);
        shared.open(Provider::Claude, Vec::new(), "m".into(), Mode::Asking);
        shared.set_mode(Mode::Plan);
        let asking = {
            let shared = shared.clone();
            tokio::spawn(async move { SessionHost { shared }.approve_plan("p").await })
        };
        let Some(Event::Ask(asked)) = events.recv().await else {
            panic!("no question");
        };
        shared.questions.answer(asked.ticket, Some("reject"));
        assert!(!asking.await.unwrap());
        assert_eq!(shared.mode(), Mode::Plan);
    }
}
