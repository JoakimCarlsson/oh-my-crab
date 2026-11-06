//! One turn of the conversation, owned so it can run on a task of its own.

use std::sync::Arc;

use omc_agent::{Context, EventSink, Live, Scribe, compact, run_turn_recorded};
use omc_auth::AuthError;
use omc_context::{Discovered, Environment, Locations};
use omc_providers::Provider;
use omc_providers::{ChatModel, Message, RemoteModel};
use omc_tools::{Host, Registry};

use crate::settings::Mode;
use crate::shared::{SessionHost, Shared};
use crate::toolset::{Inputs, Toolset};
use crate::translate::translate_all;

/// How a turn ended.
pub(crate) enum Ended {
    /// It answered; this is the conversation so far.
    Done(Vec<Message>),
    /// The account needs signing in.
    SignedOut(String),
    /// It failed for this reason.
    Failed(String),
}

/// What a turn is asked to do.
pub(crate) enum Job {
    /// Answer this prompt.
    Prompt(String),
    /// Summarize the older part of the conversation, giving particular attention to this when it is given.
    Compact(Option<String>),
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
    /// Where the turn's messages are saved, when the conversation is saved.
    pub(crate) scribe: Option<Scribe>,
}

impl Turn {
    /// Does `job` with the hosted model and says how it ended.
    pub(crate) async fn run(self, job: Job) -> Ended {
        let credentials = match self
            .shared
            .with_home(omc_providers::credentials_for(self.provider))
            .await
        {
            Ok(credentials) => credentials,
            Err(AuthError::NotLoggedIn) => return Ended::SignedOut("not logged in".to_owned()),
            Err(error) => return Ended::Failed(error.to_string()),
        };
        let model = RemoteModel {
            provider: self.provider,
            credentials,
            model: self.model.clone(),
        };
        self.run_with(Arc::new(model), job).await
    }

    /// Does `job` with `model` and says how it ended.
    pub(crate) async fn run_with(mut self, model: Arc<dyn ChatModel>, job: Job) -> Ended {
        let window = omc_providers::context_window(self.provider, &self.model);
        let effort = self.shared.effort_now();
        let out = self.shared.out.clone();
        let events: Arc<EventSink> = Arc::new(move |event| {
            for event in translate_all(event, window) {
                let _ = out.send(event);
            }
        });
        let host: Arc<dyn Host> = Arc::new(SessionHost {
            shared: self.shared.clone(),
        });
        let checkpoint = self
            .shared
            .checkpoints()
            .then(|| self.shared.checkpoint.clone());
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
            notebook: self.shared.notebook.clone(),
            checkpoint: checkpoint.clone(),
            jobs: self.shared.jobs.clone(),
            unnamed: self.shared.title().is_none(),
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
            notebook: Some(&self.shared.notebook),
            window: Some(window),
            checkpoint: checkpoint.as_deref(),
        };
        let result = match job {
            Job::Prompt(prompt) => {
                run_turn_recorded(
                    &context,
                    &mut self.history,
                    prompt,
                    events.as_ref(),
                    self.scribe.as_ref(),
                )
                .await
            }
            Job::Compact(focus) => {
                compact(
                    &context,
                    &mut self.history,
                    focus.as_deref(),
                    events.as_ref(),
                    self.scribe.as_ref(),
                )
                .await
            }
        };
        match result {
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
            scribe: None,
        };
        let model = Arc::new(Planner {
            seen: Mutex::new(Vec::new()),
        });
        let running = tokio::spawn(turn.run_with(model.clone(), Job::Prompt("plan it".into())));
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

    /// A model that reads a file, edits two lines with the tag it was given, then repeats the edit with that same tag.
    struct Editor {
        /// How many requests it has seen.
        step: Mutex<usize>,
    }

    /// Returns a reply that calls `name` once with `arguments`.
    fn calling(name: &str, arguments: serde_json::Value) -> Reply {
        Reply {
            tool_calls: vec![ToolCall {
                id: name.into(),
                name: name.into(),
                arguments,
            }],
            ..Reply::default()
        }
    }

    /// Returns the snapshot tag in the latest tool result of `messages`.
    fn latest_tag(messages: &[Message]) -> String {
        let Some(Message::ToolResults(results)) = messages.last() else {
            panic!("no tool results");
        };
        let start = results[0].text.find("tag ").unwrap() + 4;
        results[0].text[start..start + 4].to_string()
    }

    #[async_trait]
    impl ChatModel for Editor {
        /// Reads, edits two lines, then edits again with the tag the file no longer has.
        async fn complete(
            &self,
            request: Request<'_>,
            _: &mut dyn Sink,
        ) -> Result<Reply, ProviderError> {
            let mut step = self.step.lock().unwrap();
            *step += 1;
            let ops = json!([
                { "op": "replace", "line": 1, "text": "A" },
                { "op": "replace", "line": 3, "text": "C" }
            ]);
            Ok(match *step {
                1 => calling("read", json!({ "path": "f.txt" })),
                2 => calling(
                    "edit",
                    json!({ "files": [{ "path": "f.txt", "tag": latest_tag(request.messages), "ops": ops }] }),
                ),
                3 => calling(
                    "edit",
                    json!({ "files": [{ "path": "f.txt", "tag": "0000", "ops": ops }] }),
                ),
                _ => Reply {
                    text: "done".into(),
                    ..Reply::default()
                },
            })
        }
    }

    /// A model that answers every request, summary requests included, with the same text.
    struct Summarizer;

    #[async_trait]
    impl ChatModel for Summarizer {
        /// Answers with a summary.
        async fn complete(&self, _: Request<'_>, _: &mut dyn Sink) -> Result<Reply, ProviderError> {
            Ok(Reply {
                text: "what happened".into(),
                ..Reply::default()
            })
        }
    }

    /// Compacting on request summarizes the older turns, keeps every message in the history, and shows the client a finished `compact` step with the summary and a smaller context.
    #[tokio::test]
    async fn compacting_on_request_shows_a_compact_step() {
        let (out, mut events) = tokio::sync::mpsc::unbounded_channel();
        let shared = Shared::new(out);
        shared.open(Provider::Claude, vec!["m".into()], "m".into(), Mode::Yolo);
        let history = vec![
            Message::User("a".repeat(100_000)),
            Message::Assistant(Reply {
                text: "first".into(),
                ..Reply::default()
            }),
            Message::User("b".repeat(100_000)),
            Message::Assistant(Reply {
                text: "second".into(),
                ..Reply::default()
            }),
        ];
        let turn = Turn {
            provider: Provider::Claude,
            model: "m".into(),
            base: Arc::new(Registry::new()),
            denied: Arc::new(Vec::new()),
            discovered: Arc::new(Discovered::default()),
            locations: Arc::new(Locations {
                directory: std::path::PathBuf::from("."),
                root: None,
                home: None,
                claude: None,
                codex: None,
            }),
            shared,
            history: history.clone(),
            scribe: None,
        };
        let Ended::Done(compacted) = turn
            .run_with(Arc::new(Summarizer), Job::Compact(Some("names".into())))
            .await
        else {
            panic!("compaction failed");
        };
        assert_eq!(compacted[..4], history[..]);
        let Message::Compaction(compaction) = &compacted[4] else {
            panic!("no compaction recorded");
        };
        assert_eq!(compaction.first_kept, 2);
        let mut done = None;
        let mut used = None;
        while let Ok(event) = events.try_recv() {
            match event {
                Event::Tool(tool) if tool.state == Some(omc_wire::ToolState::Done) => {
                    done = Some(tool)
                }
                Event::Used(usage) => used = Some(usage.used),
                _ => {}
            }
        }
        let done = done.expect("no finished compact step");
        assert_eq!(done.id, "compact-4");
        assert_eq!(done.output.as_deref(), Some("what happened"));
        assert!(used.is_some_and(|used| used < compaction.tokens_before));
    }

    /// Reading then editing two lines changes the file and sends a diff; an edit with a stale tag is refused with a message to read again.
    #[tokio::test]
    async fn edits_report_a_diff_and_stale_tags_are_refused() {
        let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../build/tests/omc-session/turn-edit");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("f.txt"), "a\nb\nc\n").unwrap();
        let (out, mut events) = tokio::sync::mpsc::unbounded_channel();
        let shared = Shared::new(out);
        shared.open(Provider::Claude, vec!["m".into()], "m".into(), Mode::Yolo);
        let turn = Turn {
            provider: Provider::Claude,
            model: "m".into(),
            base: Arc::new(Registry::builtin(&dir).unwrap()),
            denied: Arc::new(Vec::new()),
            discovered: Arc::new(Discovered::default()),
            locations: Arc::new(Locations {
                directory: dir.clone(),
                root: None,
                home: None,
                claude: None,
                codex: None,
            }),
            shared,
            history: Vec::new(),
            scribe: None,
        };
        let model = Arc::new(Editor {
            step: Mutex::new(0),
        });
        let Ended::Done(history) = turn.run_with(model, Job::Prompt("edit".into())).await else {
            panic!("turn failed");
        };
        assert_eq!(
            std::fs::read_to_string(dir.join("f.txt")).unwrap(),
            "A\nb\nC\n"
        );
        let Some(Message::ToolResults(last)) = history.get(history.len() - 2) else {
            panic!("no results");
        };
        assert!(last[0].is_error && last[0].text.contains("read the file again"));
        let mut diffs = Vec::new();
        while let Ok(event) = events.try_recv() {
            if let Event::Tool(tool) = event
                && let Some(diff) = tool.diff
            {
                diffs.push(diff);
            }
        }
        assert_eq!(diffs.len(), 1);
        assert_eq!(diffs[0].old.as_deref(), Some("a\nb\nc\n"));
        assert_eq!(diffs[0].new, "A\nb\nC\n");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
