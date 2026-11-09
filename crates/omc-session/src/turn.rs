//! One turn of the conversation, owned so it can run on a task of its own.

use std::sync::Arc;

use omc_agent::{Chained, Context, EventSink, Hooks, Models, Scribe, compact, run_turn_steered};
use omc_auth::AuthError;
use omc_context::{Discovered, Environment, Locations};
use omc_extensions::{ExtensionHooks, Prompted};
use omc_hooks::{Engine, HookEvent, Submitted};
use omc_providers::Provider;
use omc_providers::{ChatModel, Message, RemoteModel};
use omc_tools::{Deferral, Host, Registry};

use crate::lifecycle::{context_of, start_notice};
use crate::picker::SessionPicker;
use crate::prefix::{Key, Planning, Prefix};
use crate::settings::Mode;
use crate::shared::{SessionHost, Shared};
use crate::toolset::{Inputs, Toolset};
use crate::translate::translate_all;
use crate::warmer::Recorder;

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
    /// How subagents get git worktrees of their own, when they can.
    pub(crate) isolation: Option<Arc<omc_agent::Isolation>>,
    /// The hooks that run, when there are any.
    pub(crate) hooks: Option<Arc<Engine>>,
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
            retention: self.shared.retention(),
        };
        self.run_with(Arc::new(model), job).await
    }

    /// The workstation as it is now; in tests, on the date the conversation pins, when it pins one.
    fn environment(&self) -> Environment {
        let environment = Environment::detect(&self.locations, &self.model);
        #[cfg(test)]
        let environment = match self
            .shared
            .today
            .lock()
            .unwrap_or_else(|held| held.into_inner())
            .clone()
        {
            Some(date) => Environment {
                date,
                ..environment
            },
            None => environment,
        };
        environment
    }

    /// Does `job` with `model` and says how it ended; a prompt takes in the prompts the client queued meanwhile at each safe point.
    ///
    /// Every request of the turn is sent with the conversation's system prompt and tool list, made by its first turn, so that the provider's prompt cache keeps them: a prompt starts with notices of what changed since, among them the context the `SessionStart` hooks gave when the conversation opened or was last compacted, and the tools the mode on now lacks are refused when called. A compaction starts the `SessionStart` hooks again. The last request of the main loop is kept warm.
    pub(crate) async fn run_with(mut self, model: Arc<dyn ChatModel>, job: Job) -> Ended {
        let window = omc_providers::context_window(self.provider, &self.model);
        let effort = self.shared.effort_now();
        let out = self.shared.out.clone();
        let lifecycle = self.shared.lifecycle.clone();
        let named = self.model.clone();
        let events: Arc<EventSink> = Arc::new(move |event| {
            if compacted(&event) {
                lifecycle.begin(START_COMPACT, Some(named.clone()));
            }
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
        let environment = Arc::new(self.environment());
        let unnamed = self.shared.title().is_none();
        let memories = self.shared.memories_now();
        let tree = self.shared.tree(&self.locations.directory).await;
        let extensions = self.shared.lifecycle.extensions();
        let mut links: Vec<Arc<dyn Hooks>> = Vec::new();
        if let Some(hooks) = &self.hooks {
            links.push(hooks.clone());
        }
        if let Some(extensions) = &extensions {
            links.push(Arc::new(extensions.clone()));
        }
        let checks = Chained::of(links);
        let toolset = Toolset::build(&Inputs {
            base: &self.base,
            denied: &self.denied,
            deferral: &Deferral::from_env(),
            host,
            model: model.clone(),
            models: Models {
                current: self.model.clone(),
                picker: Some(Arc::new(SessionPicker {
                    shared: self.shared.clone(),
                    provider: self.provider,
                    effort,
                })),
            },
            throttle: self.shared.throttle.clone(),
            approvals: self.shared.approvals.clone(),
            events: events.clone(),
            effort,
            discovered: self.discovered.clone(),
            environment: environment.clone(),
            tree: tree.clone(),
            notebook: self.shared.notebook.clone(),
            checkpoint: checkpoint.clone(),
            jobs: self.shared.jobs.clone(),
            unnamed,
            isolation: self.isolation.clone(),
            memories: memories.clone(),
            hooks: checks.clone(),
        });
        let key = Key {
            provider: self.provider,
            model: self.model.clone(),
            checkpoints: checkpoint.is_some(),
            memory: memories.is_some(),
        };
        let prefix = self.shared.prefix(&key, || {
            Prefix::freeze(
                key.clone(),
                &toolset,
                &environment,
                &tree,
                self.discovered.clone(),
                unnamed,
            )
        });
        let shared = self.shared.clone();
        let planning: Planning = Arc::new(move || shared.mode() == Mode::Plan);
        let registry = prefix.registry(&toolset, &planning);
        let _running = self.shared.warmer.running();
        let recorder = Recorder {
            inner: model.clone(),
            shared: self.shared.clone(),
            provider: self.provider,
            model: self.model.clone(),
            checkpoints: checkpoint.is_some(),
        };
        let context = Context {
            model: &recorder,
            registry: &registry,
            approvals: &self.shared.approvals,
            system: &prefix.system,
            effort,
            refresh: None,
            notebook: Some(&self.shared.notebook),
            window: Some(window),
            checkpoint: checkpoint.as_deref(),
            hooks: checks.as_deref(),
        };
        let result = match job {
            Job::Prompt(prompt) => {
                let added = match self.submitted(&prompt).await {
                    Ok(added) => added,
                    Err(blocked) => return blocked,
                };
                let (prompt, extended) = match self.extended(extensions.as_ref(), prompt).await {
                    Ok(extended) => extended,
                    Err(blocked) => return blocked,
                };
                let mut told = prefix.notices(
                    &self.history,
                    &environment,
                    &self.discovered,
                    planning(),
                    &toolset,
                );
                let starting = self.shared.lifecycle.pending();
                for notice in &starting {
                    if let Some(context) = context_of(notice.run.clone()).await {
                        told.push(start_notice(notice.tag, &context));
                    }
                }
                if let Some(memories) = memories.filter(|_| prefix.wants_memory(&self.history)) {
                    let digest = digest(memories, prompt.clone()).await;
                    told.push(prefix.memory_notice(&digest));
                }
                told.extend(added);
                told.extend(extended);
                told.push(prompt);
                let steering: Option<&dyn omc_agent::Steering> = if self
                    .hooks
                    .as_ref()
                    .is_some_and(|hooks| hooks.has(HookEvent::UserPromptSubmit))
                    || extensions
                        .as_ref()
                        .is_some_and(|extensions| extensions.wants("turn_start"))
                {
                    None
                } else {
                    Some(self.shared.as_ref())
                };
                let result = run_turn_steered(
                    &context,
                    &mut self.history,
                    told.join("\n\n"),
                    events.as_ref(),
                    self.scribe.as_ref(),
                    steering,
                )
                .await;
                if result.is_ok() {
                    self.shared.lifecycle.settle(&starting);
                }
                result
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
        if let Some(extensions) = &extensions {
            let failed = result.as_ref().err().map(ToString::to_string);
            extensions.turn_end(failed.as_deref()).await;
        }
        match result {
            Ok(()) => Ended::Done(self.history),
            Err(error) => {
                if let (Some(hooks), Some(kind)) = (&self.hooks, failure_kind(&error)) {
                    let text = error.to_string();
                    hooks.stop_failure(kind, &text, &text).await;
                }
                Ended::Failed(error.to_string())
            }
        }
    }
}

/// The kind of error `error` is, as `StopFailure` hooks are told and match it, when it is a model request that failed for good; nothing for a failure of another kind, such as a compaction a hook blocked.
fn failure_kind(error: &omc_agent::AgentError) -> Option<&'static str> {
    use omc_agent::AgentError;
    use omc_providers::ProviderError;
    Some(match error {
        AgentError::Provider(ProviderError::Status { status, .. }) => {
            match status.split_whitespace().next().unwrap_or_default() {
                "429" => "rate_limit",
                "529" | "503" => "overloaded",
                "401" | "403" => "authentication_failed",
                "402" => "billing_error",
                "404" => "model_not_found",
                code if code.starts_with('4') => "invalid_request",
                code if code.starts_with('5') => "server_error",
                _ => "unknown",
            }
        }
        AgentError::Provider(ProviderError::Credentials(_)) => "cloud_credential_error",
        AgentError::Provider(ProviderError::NoModels) => "model_not_found",
        AgentError::Provider(_) | AgentError::Empty => "unknown",
        AgentError::OutputLimit => "max_output_tokens",
        AgentError::Compaction(_) => return None,
    })
}

impl Turn {
    /// Asks `extensions` about `prompt`, which is about to start a turn, and returns the prompt to send, rewritten when one rewrote it, and the context they add before it; when one blocks it, the turn that ends for that reason, recorded in the saved history as lost.
    async fn extended(
        &self,
        extensions: Option<&ExtensionHooks>,
        prompt: String,
    ) -> Result<(String, Option<String>), Ended> {
        let Some(extensions) = extensions else {
            return Ok((prompt, None));
        };
        match extensions.turn_start(&prompt).await {
            Prompted::Go => Ok((prompt, None)),
            Prompted::Changed {
                prompt: rewritten,
                context,
            } => Ok((
                rewritten.unwrap_or(prompt),
                context.map(|context| {
                    format!(
                        "<extension-context>\n{}\n</extension-context>",
                        context.trim()
                    )
                }),
            )),
            Prompted::Blocked(reason) => {
                let reason = format!("an extension blocked the prompt: {}", reason.trim());
                if let Some(Err(error)) = self
                    .scribe
                    .as_ref()
                    .map(|scribe| scribe.cut_losing(self.history.len(), &reason))
                {
                    eprintln!("history not saved: {error}");
                }
                Err(Ended::Failed(reason))
            }
        }
    }

    /// Runs the hooks of the prompt `prompt` and returns the context they add before it; when they block it, the turn that ends for that reason, recorded in the saved history as lost so the reader still sees the prompt.
    async fn submitted(&self, prompt: &str) -> Result<Option<String>, Ended> {
        let Some(hooks) = &self.hooks else {
            return Ok(None);
        };
        match hooks.prompt(prompt).await {
            Submitted::Go => Ok(None),
            Submitted::WithContext(context) => Ok(Some(format!(
                "<user-prompt-submit-hook>\n{}\n</user-prompt-submit-hook>",
                context.trim()
            ))),
            Submitted::Blocked(reason) => {
                let reason = format!("a hook blocked the prompt: {}", reason.trim());
                if let Some(Err(error)) = self
                    .scribe
                    .as_ref()
                    .map(|scribe| scribe.cut_losing(self.history.len(), &reason))
                {
                    eprintln!("history not saved: {error}");
                }
                Err(Ended::Failed(reason))
            }
        }
    }
}

/// How a conversation started, as `SessionStart` hooks are told it, when it was just compacted.
const START_COMPACT: &str = "compact";

/// Whether `event` says the main agent's conversation was compacted.
fn compacted(event: &omc_agent::AgentEvent) -> bool {
    matches!(
        event,
        omc_agent::AgentEvent::ToolDone { name, is_error: false, .. } if name == omc_agent::COMPACT
    )
}

/// The digest of `memories` a conversation whose prompt is `prompt` starts with, read on a thread for blocking work; empty when there are none or they cannot be read, which the agent is then told as there being none.
async fn digest(memories: Arc<omc_memory::Memories>, prompt: String) -> String {
    tokio::task::spawn_blocking(move || match memories.all() {
        Ok(all) => omc_memory::digest(all, &prompt),
        Err(error) => {
            eprintln!("memories not loaded: {error}");
            String::new()
        }
    })
    .await
    .unwrap_or_default()
}

#[cfg(test)]
/// Tests for running a turn through the session's tools and modes.
mod tests {
    use std::sync::Mutex;

    use async_trait::async_trait;
    use omc_providers::{ProviderError, Reply, Request, Sink, ToolCall};
    use omc_tools::LOAD_TOOLS;
    use omc_wire::Event;
    use serde_json::json;

    use super::*;

    /// What one request carried: the system prompt and the tool list as the provider gets them, and the text of the last message when it is the reader's.
    #[derive(Clone, Debug, PartialEq)]
    struct Sent {
        /// The system prompt.
        system: String,
        /// Every tool's name, description and schema, in order.
        tools: Vec<serde_json::Value>,
        /// The last message, when it is the reader's.
        prompt: Option<String>,
    }

    impl Sent {
        /// What `request` carried.
        fn of(request: &Request<'_>) -> Self {
            Self {
                system: request.system.to_owned(),
                tools: request
                    .tools
                    .iter()
                    .map(|tool| json!([tool.name, tool.description, tool.schema]))
                    .collect(),
                prompt: match request.messages.last() {
                    Some(Message::User(text)) => Some(text.clone()),
                    _ => None,
                },
            }
        }
    }

    /// A model that writes a file in plan mode, then calls `exit_plan_mode`, then writes the file again, then answers, and keeps what it was sent.
    struct Planner {
        /// What each request carried, with the results it was given.
        seen: Mutex<Vec<(Sent, Option<Vec<omc_providers::ToolResult>>)>>,
    }

    #[async_trait]
    impl ChatModel for Planner {
        /// Writes, plans, writes, then says it is done.
        async fn complete(
            &self,
            request: Request<'_>,
            _: &mut dyn Sink,
        ) -> Result<Reply, ProviderError> {
            let results = match request.messages.last() {
                Some(Message::ToolResults(results)) => Some(results.clone()),
                _ => None,
            };
            let mut seen = self.seen.lock().unwrap();
            seen.push((Sent::of(&request), results));
            let write = || calling("write", json!({"path": "made.txt", "content": "x"}));
            Ok(match seen.len() {
                1 | 3 => write(),
                2 => calling("exit_plan_mode", json!({"plan": "# Plan\n1. do it"})),
                _ => Reply {
                    text: "implemented".into(),
                    ..Reply::default()
                },
            })
        }
    }

    /// A turn for `shared` in `dir` with the built-in tools and `history`.
    fn turn_in(dir: &std::path::Path, shared: &Arc<Shared>, history: Vec<Message>) -> Turn {
        Turn {
            provider: Provider::Claude,
            model: "m".into(),
            base: Arc::new(Registry::builtin(dir).unwrap()),
            denied: Arc::new(Vec::new()),
            discovered: Arc::new(Discovered::default()),
            locations: Arc::new(Locations {
                directory: dir.to_path_buf(),
                root: None,
                home: None,
                claude: None,
                codex: None,
                project: true,
            }),
            shared: shared.clone(),
            history,
            scribe: None,
            isolation: None,
            hooks: None,
        }
    }

    /// Plan mode keeps every tool on the list but refuses the ones that change things; approving the plan switches the mode back, announces it, and the same turn goes on writing, with the same system prompt and tools throughout.
    #[tokio::test]
    async fn approving_a_plan_ends_plan_mode_mid_turn() {
        let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../build/tests/omc-session/turn-plan");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let (out, mut events) = tokio::sync::mpsc::unbounded_channel();
        let shared = Shared::new(out);
        shared.open(Provider::Claude, vec!["m".into()], "m".into(), Mode::Yolo);
        shared.set_mode(Mode::Plan);
        assert!(!shared.approvals.yolo());
        let turn = turn_in(&dir, &shared, Vec::new());
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
        assert!(!dir.join("made.txt").exists());
        shared.questions.answer(ticket, Some("allow"));
        let Ended::Done(history) = running.await.unwrap() else {
            panic!("turn failed");
        };
        assert_eq!(history.len(), 8);
        assert!(dir.join("made.txt").exists());
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
        assert_eq!(seen.len(), 4);
        for (sent, _) in seen.iter() {
            assert_eq!(
                (&sent.system, &sent.tools),
                (&seen[0].0.system, &seen[0].0.tools)
            );
        }
        let names: Vec<&str> = seen[0]
            .0
            .tools
            .iter()
            .map(|tool| tool[0].as_str().unwrap())
            .collect();
        assert!(names.contains(&"write") && names.contains(&"exit_plan_mode"));
        assert!(!seen[0].0.system.contains("Plan mode active"));
        let prompt = seen[0].0.prompt.as_deref().unwrap();
        assert!(prompt.contains("Plan mode is now on") && prompt.ends_with("plan it"));
        let refused = &seen[1].1.as_ref().unwrap()[0];
        assert!(refused.is_error && refused.text.contains("not available in plan mode"));
        let written = &seen[3].1.as_ref().unwrap()[0];
        assert!(!written.is_error, "{}", written.text);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A model that runs one script, then answers, and keeps the results it was given.
    struct Scripting {
        /// The script it runs.
        code: &'static str,
        /// The results of each request that carried some.
        results: Mutex<Vec<omc_providers::ToolResult>>,
    }

    #[async_trait]
    impl ChatModel for Scripting {
        /// Runs the script first, then says it is done.
        async fn complete(
            &self,
            request: Request<'_>,
            _: &mut dyn Sink,
        ) -> Result<Reply, ProviderError> {
            if let Some(Message::ToolResults(results)) = request.messages.last() {
                self.results.lock().unwrap().extend(results.iter().cloned());
                return Ok(Reply {
                    text: "done".into(),
                    ..Reply::default()
                });
            }
            Ok(calling("script", json!({ "code": self.code })))
        }
    }

    /// In plan mode a script can only call the tools that look, through the conversation's own tool list; its calls reach the client as children of the script call.
    #[tokio::test]
    async fn scripts_in_plan_mode_only_look() {
        let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../build/tests/omc-session/turn-script-plan");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.txt"), "alpha\n").unwrap();
        let (out, mut events) = tokio::sync::mpsc::unbounded_channel();
        let shared = Shared::new(out);
        shared.open(Provider::Claude, vec!["m".into()], "m".into(), Mode::Yolo);
        shared.set_mode(Mode::Plan);
        let mut turn = turn_in(&dir, &shared, Vec::new());
        let mut base = Registry::builtin(&dir).unwrap();
        base.register(omc_script::Script::new(omc_tools::Spill::disabled()));
        turn.base = Arc::new(base);
        let model = Arc::new(Scripting {
            code: "const text = await tools.read({ path: 'a.txt' });
                   let refused;
                   try { await tools.write({ path: 'made.txt', content: 'x' }); } catch (error) { refused = error.message; }
                   return { read: text.includes('alpha'), refused };",
            results: Mutex::new(Vec::new()),
        });
        let Ended::Done(_) = turn
            .run_with(model.clone(), Job::Prompt("look".into()))
            .await
        else {
            panic!("turn failed");
        };
        let results = model.results.lock().unwrap();
        assert!(!results[0].is_error, "{}", results[0].text);
        assert!(
            results[0].text.contains("\"read\": true"),
            "{}",
            results[0].text
        );
        assert!(
            results[0]
                .text
                .contains("there is no tool called write in scripts"),
            "{}",
            results[0].text
        );
        assert!(!dir.join("made.txt").exists());
        let mut children = Vec::new();
        while let Ok(event) = events.try_recv() {
            if let Event::Tool(tool) = event
                && tool.parent.as_deref() == Some("script")
            {
                children.push(tool);
            }
        }
        assert!(
            children.iter().any(|tool| tool.id == "script:1"
                && tool.title.as_deref() == Some("read")
                && tool.agent.as_deref() == Some("script")),
            "{children:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A model that answers every request at once and keeps what each carried.
    struct Recording {
        /// What each request carried.
        seen: Mutex<Vec<Sent>>,
    }

    #[async_trait]
    impl ChatModel for Recording {
        /// Answers with a word.
        async fn complete(
            &self,
            request: Request<'_>,
            _: &mut dyn Sink,
        ) -> Result<Reply, ProviderError> {
            self.seen.lock().unwrap().push(Sent::of(&request));
            Ok(Reply {
                text: "ok".into(),
                ..Reply::default()
            })
        }
    }

    /// The system prompt and tool list sent stay byte for byte the same across plan mode turning on and off, the date changing and files appearing; each change of mode or date is told once, at the start of the next prompt, and the workspace outline stays as the first turn saw it.
    #[tokio::test]
    async fn the_prefix_survives_plan_mode_and_a_new_day() {
        let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../build/tests/omc-session/turn-prefix");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("early.txt"), "").unwrap();
        let (out, _events) = tokio::sync::mpsc::unbounded_channel();
        let shared = Shared::new(out);
        shared.open(Provider::Claude, vec!["m".into()], "m".into(), Mode::Asking);
        let model = Arc::new(Recording {
            seen: Mutex::new(Vec::new()),
        });
        let mut history = Vec::new();
        let steps = [
            ("2026-11-05", Mode::Asking, "one"),
            ("2026-11-06", Mode::Plan, "two"),
            ("2026-11-06", Mode::Plan, "three"),
            ("2026-11-06", Mode::Asking, "four"),
        ];
        for (date, mode, prompt) in steps {
            *shared.today.lock().unwrap() = Some(date.to_owned());
            shared.set_mode(mode);
            let turn = turn_in(&dir, &shared, history);
            let Ended::Done(after) = turn
                .run_with(model.clone(), Job::Prompt(prompt.into()))
                .await
            else {
                panic!("turn failed");
            };
            history = after;
            std::fs::write(dir.join("later.txt"), "").unwrap();
        }
        let seen = model.seen.lock().unwrap();
        assert_eq!(seen.len(), 4);
        assert!(seen[0].system.contains("<workspace-tree>"));
        assert!(seen[0].system.contains("\nearly.txt\n"));
        assert!(!seen[0].system.contains("later.txt"));
        for sent in seen.iter() {
            assert_eq!(sent.system.as_bytes(), seen[0].system.as_bytes());
            assert_eq!(
                serde_json::to_vec(&sent.tools).unwrap(),
                serde_json::to_vec(&seen[0].tools).unwrap()
            );
            assert!(!sent.system.contains("2026-11-0"));
        }
        let prompts: Vec<&str> = seen
            .iter()
            .map(|sent| sent.prompt.as_deref().unwrap())
            .collect();
        assert!(prompts[0].starts_with("<environment>\nToday's date: 2026-11-05."));
        assert!(!prompts[0].contains("<plan-mode>"));
        assert!(
            prompts[1].contains("Today's date: 2026-11-06.")
                && prompts[1].contains("Plan mode is now on")
        );
        assert_eq!(prompts[2], "three");
        assert!(
            prompts[3].starts_with("<plan-mode>\nPlan mode is now off")
                && prompts[3].ends_with("four")
        );
    }

    /// After a compaction has summarized away the reminder of the date, the next prompt carries it again.
    #[tokio::test]
    async fn the_date_is_told_again_after_compaction() {
        let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../build/tests/omc-session/turn-dated");
        std::fs::create_dir_all(&dir).unwrap();
        let (out, _events) = tokio::sync::mpsc::unbounded_channel();
        let shared = Shared::new(out);
        shared.open(Provider::Claude, vec!["m".into()], "m".into(), Mode::Asking);
        *shared.today.lock().unwrap() = Some("2026-11-06".to_owned());
        let model = Arc::new(Recording {
            seen: Mutex::new(Vec::new()),
        });
        let told = format!(
            "{}\n\nfirst",
            omc_context::environment_reminder(&Environment {
                directory: dir.clone(),
                os: std::env::consts::OS.to_owned(),
                date: "2026-11-06".into(),
                branch: None,
                model: None,
            })
        );
        let reply = Message::Assistant(Reply {
            text: "ok".into(),
            ..Reply::default()
        });
        let mut history = vec![Message::User(told), reply.clone()];
        let turn = turn_in(&dir, &shared, history.clone());
        let Ended::Done(_) = turn
            .run_with(model.clone(), Job::Prompt("second".into()))
            .await
        else {
            panic!("turn failed");
        };
        history.extend([
            Message::User("more".into()),
            reply,
            Message::Compaction(omc_providers::Compaction {
                summary: "s".into(),
                first_kept: 2,
                ..omc_providers::Compaction::default()
            }),
        ]);
        let turn = turn_in(&dir, &shared, history);
        let Ended::Done(_) = turn
            .run_with(model.clone(), Job::Prompt("third".into()))
            .await
        else {
            panic!("turn failed");
        };
        let seen = model.seen.lock().unwrap();
        assert_eq!(seen[0].prompt.as_deref(), Some("second"));
        let third = seen[1].prompt.as_deref().unwrap();
        assert!(third.starts_with("<environment>") && third.ends_with("third"));
    }

    /// A model that calls `skill` and `recall` together, then answers, and keeps what each request carried with the results it was given.
    struct Reacher {
        /// What each request carried, with the results it was given.
        seen: Mutex<Vec<(Sent, Option<Vec<omc_providers::ToolResult>>)>>,
    }

    #[async_trait]
    impl ChatModel for Reacher {
        /// Calls both tools the first time, then answers.
        async fn complete(
            &self,
            request: Request<'_>,
            _: &mut dyn Sink,
        ) -> Result<Reply, ProviderError> {
            let results = match request.messages.last() {
                Some(Message::ToolResults(results)) => Some(results.clone()),
                _ => None,
            };
            let mut seen = self.seen.lock().unwrap();
            seen.push((Sent::of(&request), results));
            Ok(match seen.len() {
                1 => Reply {
                    tool_calls: vec![
                        ToolCall {
                            id: "s".into(),
                            name: "skill".into(),
                            arguments: json!({"name": "pdf"}),
                        },
                        ToolCall {
                            id: "r".into(),
                            name: "recall".into(),
                            arguments: json!({"search": "x"}),
                        },
                    ],
                    ..Reply::default()
                },
                _ => Reply {
                    text: "ok".into(),
                    ..Reply::default()
                },
            })
        }
    }

    /// `skill` and `recall` are on the tool list from the first turn, before any skill exists and before anything was left out of the history, and say why they have nothing to give; the list stays the same once a pruning has made `recall` worth calling, and `recall` is then answered by the agent itself.
    #[tokio::test]
    async fn skill_and_recall_are_offered_from_the_start() {
        let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../build/tests/omc-session/turn-reach");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let (out, _events) = tokio::sync::mpsc::unbounded_channel();
        let shared = Shared::new(out);
        shared.open(Provider::Claude, vec!["m".into()], "m".into(), Mode::Yolo);
        let mut base = Registry::builtin(&dir).unwrap();
        crate::session::register_skills(&mut base, &[]);
        let base = Arc::new(base);
        let model = Arc::new(Reacher {
            seen: Mutex::new(Vec::new()),
        });
        let mut turn = turn_in(&dir, &shared, Vec::new());
        turn.base = base.clone();
        let Ended::Done(mut history) = turn.run_with(model.clone(), Job::Prompt("go".into())).await
        else {
            panic!("turn failed");
        };
        history.push(Message::Pruning(omc_providers::Pruning {
            elided: Vec::new(),
            excised: Vec::new(),
            tokens_after: 0,
        }));
        let model_after = Arc::new(Reacher {
            seen: Mutex::new(Vec::new()),
        });
        let mut turn = turn_in(&dir, &shared, history);
        turn.base = base;
        let Ended::Done(_) = turn
            .run_with(model_after.clone(), Job::Prompt("again".into()))
            .await
        else {
            panic!("turn failed");
        };
        let seen = model.seen.lock().unwrap();
        let names: Vec<&str> = seen[0]
            .0
            .tools
            .iter()
            .map(|tool| tool[0].as_str().unwrap())
            .collect();
        assert!(names.contains(&"skill") && names.contains(&"recall"));
        let results = seen[1].1.as_ref().unwrap();
        assert!(results[0].is_error && results[0].text.contains("no skills are available"));
        assert!(results[1].is_error && results[1].text.contains("nothing to read yet"));
        let after = model_after.seen.lock().unwrap();
        assert_eq!(
            serde_json::to_vec(&after[0].0.tools).unwrap(),
            serde_json::to_vec(&seen[0].0.tools).unwrap()
        );
        let recalled = &after[1].1.as_ref().unwrap()[1];
        assert!(
            !recalled.is_error && recalled.text.contains("#1: calls recall"),
            "{}",
            recalled.text
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A model that deletes `f.txt` with `bash` on its first request, then answers, keeping the results it was given.
    #[cfg(unix)]
    struct Deleter {
        /// The results each request carried.
        results: Mutex<Vec<Vec<omc_providers::ToolResult>>>,
    }

    #[cfg(unix)]
    #[async_trait]
    impl ChatModel for Deleter {
        /// Calls `bash` with `rm f.txt`, then answers.
        async fn complete(
            &self,
            request: Request<'_>,
            _: &mut dyn Sink,
        ) -> Result<Reply, ProviderError> {
            let mut results = self.results.lock().unwrap();
            if let Some(Message::ToolResults(given)) = request.messages.last() {
                results.push(given.clone());
                return Ok(Reply {
                    text: "done".into(),
                    ..Reply::default()
                });
            }
            Ok(calling("bash", json!({"command": "rm f.txt"})))
        }
    }

    /// A project's hook that refuses `rm` in `bash` is skipped while the project is untrusted, so the file is deleted, and enforced once it is trusted, so the model is told why and the file stays; the user's note after each call reaches the model either way.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_project_hook_runs_only_once_trusted() {
        let base = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../build/tests/omc-session/turn-hooks");
        let _ = std::fs::remove_dir_all(&base);
        let dir = base.join("repo");
        std::fs::create_dir_all(dir.join(".claude")).unwrap();
        let status = std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(&dir)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .status()
            .unwrap();
        assert!(status.success());
        let dir = dir.canonicalize().unwrap();
        std::fs::write(
            dir.join(".claude/settings.json"),
            r#"{"hooks":{"PreToolUse":[{"matcher":"Bash","hooks":[{"type":"command","command":"grep -q '\"command\":\"rm ' && { echo 'deleting files is not allowed here' >&2; exit 2; }; exit 0"}]}]}}"#,
        )
        .unwrap();
        std::fs::write(
            base.join("user-hooks.json"),
            r#"{"hooks":{"PostToolUse":[{"matcher":"bash","hooks":[{"type":"command","command":"echo '{\"hookSpecificOutput\":{\"additionalContext\":\"checked by the user hook\"}}'"}]}]}}"#,
        )
        .unwrap();
        let store = omc_hooks::TrustStore::new(base.join("trust.json"));
        let locations = Locations {
            directory: dir.clone(),
            root: Some(dir.clone()),
            home: None,
            claude: None,
            codex: None,
            project: true,
        };
        let own = base.join("user-hooks.json");
        let untrusted = omc_hooks::assess(&locations, Some(&own), Some(&store), false, &[]);
        assert_eq!(untrusted.skipped.len(), 1);
        store
            .set(&untrusted.project, true, untrusted.project_hooks.clone())
            .unwrap();
        let trusted = omc_hooks::assess(&locations, Some(&own), Some(&store), false, &[]);
        assert!(trusted.skipped.is_empty());
        let mut seen = Vec::new();
        for assessment in [untrusted, trusted] {
            std::fs::write(dir.join("f.txt"), "keep me").unwrap();
            let (out, _events) = tokio::sync::mpsc::unbounded_channel();
            let shared = Shared::new(out);
            shared.open(Provider::Claude, vec!["m".into()], "m".into(), Mode::Yolo);
            let mut turn = turn_in(&dir, &shared, Vec::new());
            turn.hooks = Some(Arc::new(Engine::new(
                assessment.hooks.clone(),
                omc_hooks::Setting {
                    session: None,
                    folder: dir.clone(),
                    mode: Arc::new(|| "bypassPermissions"),
                },
            )));
            let model = Arc::new(Deleter {
                results: Mutex::new(Vec::new()),
            });
            let Ended::Done(_) = turn
                .run_with(model.clone(), Job::Prompt("delete f.txt".into()))
                .await
            else {
                panic!("turn failed");
            };
            let result = model.results.lock().unwrap()[0][0].clone();
            seen.push((dir.join("f.txt").exists(), result));
        }
        let (kept, result) = &seen[0];
        assert!(!*kept && !result.is_error, "{}", result.text);
        assert!(result.text.contains("checked by the user hook"));
        let (kept, result) = &seen[1];
        assert!(*kept && result.is_error);
        assert!(
            result.text.contains("deleting files is not allowed here"),
            "{}",
            result.text
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    /// A model that refuses every request as malformed.
    #[cfg(unix)]
    struct Limited;

    #[cfg(unix)]
    #[async_trait]
    impl ChatModel for Limited {
        /// Fails with status 400.
        async fn complete(&self, _: Request<'_>, _: &mut dyn Sink) -> Result<Reply, ProviderError> {
            Err(ProviderError::Status {
                status: "400 Bad Request".into(),
                body: "bad field".into(),
                retry_after: None,
            })
        }
    }

    /// A turn that ends because the model request failed for good runs the `StopFailure` hooks matching the kind of error, told what failed.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_failed_request_runs_stop_failure_hooks() {
        let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../build/tests/omc-session/turn-stop-failure");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let (out, _events) = tokio::sync::mpsc::unbounded_channel();
        let shared = Shared::new(out);
        shared.open(Provider::Claude, vec!["m".into()], "m".into(), Mode::Asking);
        let mut turn = turn_in(&dir, &shared, Vec::new());
        turn.hooks = Some(Arc::new(Engine::new(
            vec![omc_hooks::Hook {
                event: HookEvent::StopFailure,
                matcher: "invalid_request".into(),
                command: "cat > failure.json".into(),
                timeout: None,
                file: dir.join("hooks.json"),
                origin: omc_hooks::HookOrigin::User,
                claude: false,
                plugin: None,
            }],
            omc_hooks::Setting {
                session: None,
                folder: dir.clone(),
                mode: Arc::new(|| "default"),
            },
        )));
        let Ended::Failed(reason) = turn
            .run_with(Arc::new(Limited), Job::Prompt("hello".into()))
            .await
        else {
            panic!("the turn did not fail");
        };
        assert!(reason.contains("400"), "{reason}");
        let failure: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(dir.join("failure.json")).unwrap())
                .unwrap();
        assert_eq!(failure["hook_event_name"], "StopFailure");
        assert_eq!(failure["error"], "invalid_request");
        assert!(
            failure["error_details"]
                .as_str()
                .unwrap()
                .contains("bad field")
        );
    }

    /// A prompt hook that blocks ends the turn before the model is asked, with its reason; one that adds context has it told before the prompt.
    #[cfg(unix)]
    #[tokio::test]
    async fn prompt_hooks_block_or_add_context() {
        let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../build/tests/omc-session/turn-prompt-hooks");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let (out, _events) = tokio::sync::mpsc::unbounded_channel();
        let shared = Shared::new(out);
        shared.open(Provider::Claude, vec!["m".into()], "m".into(), Mode::Asking);
        let hook = |command: &str| omc_hooks::Hook {
            event: HookEvent::UserPromptSubmit,
            matcher: String::new(),
            command: command.to_owned(),
            timeout: None,
            file: dir.join("hooks.json"),
            origin: omc_hooks::HookOrigin::User,
            claude: false,
            plugin: None,
        };
        let engine = |command: &str| {
            Arc::new(Engine::new(
                vec![hook(command)],
                omc_hooks::Setting {
                    session: None,
                    folder: dir.clone(),
                    mode: Arc::new(|| "default"),
                },
            ))
        };
        let model = Arc::new(Recording {
            seen: Mutex::new(Vec::new()),
        });
        let mut turn = turn_in(&dir, &shared, Vec::new());
        turn.hooks = Some(engine(
            "grep -q secret && { echo 'no secrets in prompts' >&2; exit 2; }; exit 0",
        ));
        let Ended::Failed(reason) = turn
            .run_with(model.clone(), Job::Prompt("my secret is 42".into()))
            .await
        else {
            panic!("the prompt was not blocked");
        };
        assert_eq!(reason, "a hook blocked the prompt: no secrets in prompts");
        assert!(model.seen.lock().unwrap().is_empty());
        let mut turn = turn_in(&dir, &shared, Vec::new());
        turn.hooks = Some(engine("echo 'the build is red'"));
        let Ended::Done(_) = turn
            .run_with(model.clone(), Job::Prompt("hello".into()))
            .await
        else {
            panic!("turn failed");
        };
        let prompt = model.seen.lock().unwrap()[0].prompt.clone().unwrap();
        assert!(
            prompt.contains(
                "<user-prompt-submit-hook>\nthe build is red\n</user-prompt-submit-hook>\n\nhello"
            ),
            "{prompt}"
        );
    }

    /// The context `SessionStart` hooks give is told once, as a notice at the start of the first prompt, without changing the system prompt or tool list; a compaction runs the hooks again for `compact` and the next prompt carries their new context.
    #[cfg(unix)]
    #[tokio::test]
    async fn session_start_context_is_a_notice() {
        let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../build/tests/omc-session/turn-session-start");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let (out, _events) = tokio::sync::mpsc::unbounded_channel();
        let shared = Shared::new(out);
        shared.open(Provider::Claude, vec!["m".into()], "m".into(), Mode::Asking);
        shared.lifecycle.set_engine(Some(Arc::new(Engine::new(
            vec![omc_hooks::Hook {
                event: HookEvent::SessionStart,
                matcher: String::new(),
                command: r#"echo "started by $(grep -o '"source":"[a-z]*"' | cut -d'"' -f4)""#
                    .to_owned(),
                timeout: None,
                file: dir.join("hooks.json"),
                origin: omc_hooks::HookOrigin::User,
                claude: false,
                plugin: None,
            }],
            omc_hooks::Setting {
                session: None,
                folder: dir.clone(),
                mode: Arc::new(|| "default"),
            },
        ))));
        shared.lifecycle.begin("startup", Some("m".into()));
        let model = Arc::new(Recording {
            seen: Mutex::new(Vec::new()),
        });
        let reply = Message::Assistant(omc_providers::Reply {
            text: "ok".into(),
            ..omc_providers::Reply::default()
        });
        let mut history = vec![
            Message::User("x".repeat(200_000)),
            reply.clone(),
            Message::User("y".repeat(200_000)),
            reply,
        ];
        let jobs = [
            Job::Prompt("one".into()),
            Job::Prompt("two".into()),
            Job::Compact(None),
            Job::Prompt("three".into()),
        ];
        for job in jobs {
            let turn = turn_in(&dir, &shared, history);
            history = match turn.run_with(model.clone(), job).await {
                Ended::Done(after) => after,
                Ended::Failed(reason) | Ended::SignedOut(reason) => panic!("turn failed: {reason}"),
            };
        }
        assert!(shared.lifecycle.pending().is_empty());
        let seen = model.seen.lock().unwrap();
        let turns: Vec<&Sent> = seen
            .iter()
            .filter(|sent| !sent.system.contains("checkpoint summaries"))
            .collect();
        assert_eq!(turns.len(), 3);
        for sent in &turns {
            assert_eq!(sent.system.as_bytes(), turns[0].system.as_bytes());
            assert_eq!(sent.tools, turns[0].tools);
            assert!(!sent.system.contains("started by"));
        }
        let prompts: Vec<&str> = turns
            .iter()
            .map(|sent| sent.prompt.as_deref().unwrap())
            .collect();
        assert!(
            prompts[0].contains("<session-start-hook>\nstarted by startup\n</session-start-hook>")
                && prompts[0].ends_with("one"),
            "{}",
            prompts[0]
        );
        assert_eq!(prompts[1], "two");
        assert!(
            prompts[2].contains("<session-start-hook>\nstarted by compact\n</session-start-hook>")
                && prompts[2].ends_with("three"),
            "{}",
            prompts[2]
        );
        let _ = std::fs::remove_dir_all(&dir);
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
                project: true,
            }),
            shared,
            history: history.clone(),
            scribe: None,
            isolation: None,
            hooks: None,
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

    /// A model that, on its first request, has the client queue a prompt and reads a file; afterwards it answers, and keeps the last message of each request.
    struct Interrupted {
        /// What the conversation shares, whose queue the prompt goes into.
        shared: Arc<Shared>,
        /// The last message of each request.
        last: Mutex<Vec<Message>>,
    }

    #[async_trait]
    impl ChatModel for Interrupted {
        /// Queues a prompt and reads on the first request, then answers.
        async fn complete(
            &self,
            request: Request<'_>,
            _: &mut dyn Sink,
        ) -> Result<Reply, ProviderError> {
            let mut last = self.last.lock().unwrap();
            last.push(request.messages.last().cloned().unwrap());
            if last.len() > 1 {
                return Ok(Reply {
                    text: "both done".into(),
                    ..Reply::default()
                });
            }
            self.shared.queue.push(
                "and the second".into(),
                Some("t2".into()),
                "and the second".into(),
                vec!["b.txt".into()],
            );
            Ok(calling("read", json!({ "path": "f.txt" })))
        }
    }

    /// A prompt queued while the tools run is put into the turn before the next request, the client is told so, and it is saved as a turn of its own that comes back when the conversation is opened again.
    #[tokio::test]
    async fn a_queued_prompt_is_steered_into_the_turn_and_saved() {
        let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../build/tests/omc-session/turn-steer");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("f.txt"), "a\n").unwrap();
        let (journal, _) = omc_agent::Journal::open(&dir.join("c.jsonl"), None).unwrap();
        journal
            .mark(
                0,
                &omc_agent::Mark {
                    id: "t1".into(),
                    text: "read f".into(),
                    files: Vec::new(),
                },
            )
            .unwrap();
        let (out, mut events) = tokio::sync::mpsc::unbounded_channel();
        let shared = Shared::new(out);
        shared.open(Provider::Claude, vec!["m".into()], "m".into(), Mode::Yolo);
        shared.attach(Some(journal.clone()));
        let registry = Arc::new(Registry::builtin(&dir).unwrap());
        let turn = Turn {
            provider: Provider::Claude,
            model: "m".into(),
            base: registry.clone(),
            denied: Arc::new(Vec::new()),
            discovered: Arc::new(Discovered::default()),
            locations: Arc::new(Locations {
                directory: dir.clone(),
                root: None,
                home: None,
                claude: None,
                codex: None,
                project: true,
            }),
            shared: shared.clone(),
            history: Vec::new(),
            scribe: Some(journal.scribe()),
            isolation: None,
            hooks: None,
        };
        let model = Arc::new(Interrupted {
            shared: shared.clone(),
            last: Mutex::new(Vec::new()),
        });
        let Ended::Done(history) = turn
            .run_with(model.clone(), Job::Prompt("read f".into()))
            .await
        else {
            panic!("turn failed");
        };
        assert_eq!(
            model.last.lock().unwrap()[1],
            Message::User("and the second".into())
        );
        assert_eq!(history.len(), 5);
        let steered: Vec<omc_wire::Queued> = std::iter::from_fn(|| events.try_recv().ok())
            .filter_map(|event| match event {
                Event::Queued(queued) => Some(queued),
                _ => None,
            })
            .collect();
        assert_eq!(steered.len(), 1);
        assert_eq!(steered[0].state, omc_wire::QueueState::Steered);
        assert_eq!(steered[0].turn.as_deref(), Some("t2"));
        assert!(shared.queue.pop().is_none());
        drop(journal);
        let saved = omc_agent::Journal::read(&dir.join("c.jsonl")).unwrap();
        let turns = crate::translate::replay(
            &saved.history,
            &saved.marks,
            &saved.lost,
            &saved.changes,
            &registry,
            None,
        );
        let told: Vec<(Option<&str>, &str, &[String])> = turns
            .iter()
            .map(|turn| (turn.id.as_deref(), turn.prompt.as_str(), &turn.files[..]))
            .collect();
        assert_eq!(
            told,
            vec![
                (Some("t1"), "read f", &[][..]),
                (Some("t2"), "and the second", &["b.txt".to_owned()][..]),
            ]
        );
        assert!(turns[1].events.contains(&Event::Said("both done".into())));
        let _ = std::fs::remove_dir_all(&dir);
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
                project: true,
            }),
            shared,
            history: Vec::new(),
            scribe: None,
            isolation: None,
            hooks: None,
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

    /// A model that saves a memory on its first request and answers every other one, keeping what each request carried.
    struct Rememberer {
        /// What each request carried.
        seen: Mutex<Vec<Sent>>,
    }

    #[async_trait]
    impl ChatModel for Rememberer {
        /// Calls `remember` the first time, then answers.
        async fn complete(
            &self,
            request: Request<'_>,
            _: &mut dyn Sink,
        ) -> Result<Reply, ProviderError> {
            let mut seen = self.seen.lock().unwrap();
            seen.push(Sent::of(&request));
            Ok(if seen.len() == 1 {
                calling(
                    "remember",
                    json!({"text": "Release builds need the `ci` feature.", "tags": ["build"]}),
                )
            } else {
                Reply {
                    text: "ok".into(),
                    ..Reply::default()
                }
            })
        }
    }

    /// A fresh folder named `name` for a turn to work in, and memories kept in a store inside it, holding one memory about integration tests.
    fn with_memories(name: &str) -> (std::path::PathBuf, Arc<omc_memory::Memories>) {
        let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../build/tests/omc-session")
            .join(name);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let memories = Arc::new(omc_memory::Memories::new(
            &omc_memory::Store::new(dir.join("store")),
            omc_memory::Project::of(&dir),
            None,
        ));
        memories
            .remember(
                "Integration tests need Docker running.",
                &[],
                omc_memory::Scope::Project,
            )
            .unwrap();
        (dir, memories)
    }

    /// The names of the tools `sent` offered.
    fn tool_names(sent: &Sent) -> Vec<&str> {
        sent.tools
            .iter()
            .map(|tool| tool[0].as_str().unwrap())
            .collect()
    }

    /// The first prompt starts with the digest of the memories; a memory saved during the turn is kept on disk and never makes the system prompt, the tool list or the next prompt change.
    #[tokio::test]
    async fn the_memory_digest_comes_once_and_the_prefix_holds() {
        let (dir, memories) = with_memories("turn-memory");
        let (out, _events) = tokio::sync::mpsc::unbounded_channel();
        let shared = Shared::new(out);
        shared.set_memories(Some(memories.clone()));
        shared.open(Provider::Claude, vec!["m".into()], "m".into(), Mode::Yolo);
        let model = Arc::new(Rememberer {
            seen: Mutex::new(Vec::new()),
        });
        let mut history = Vec::new();
        for prompt in ["how do I run the integration tests", "next"] {
            let turn = turn_in(&dir, &shared, history);
            let Ended::Done(after) = turn
                .run_with(model.clone(), Job::Prompt(prompt.into()))
                .await
            else {
                panic!("turn failed");
            };
            history = after;
        }
        let seen = model.seen.lock().unwrap();
        assert_eq!(seen.len(), 3);
        for sent in seen.iter() {
            assert_eq!(sent.system.as_bytes(), seen[0].system.as_bytes());
            assert_eq!(
                serde_json::to_vec(&sent.tools).unwrap(),
                serde_json::to_vec(&seen[0].tools).unwrap()
            );
        }
        let names = tool_names(&seen[0]);
        for name in ["remember", "memory_search"] {
            assert!(names.contains(&name), "{name} missing");
        }
        if Deferral::from_env().defers("memory_edit") {
            assert!(!names.contains(&"memory_edit") && names.contains(&LOAD_TOOLS));
        } else {
            assert!(names.contains(&"memory_edit"));
        }
        assert!(seen[0].system.contains("# Memory"));
        let first = seen[0].prompt.as_deref().unwrap();
        assert!(
            first.contains("<memory>") && first.contains("Integration tests need Docker running.")
        );
        assert!(first.ends_with("how do I run the integration tests"));
        assert_eq!(seen[2].prompt.as_deref(), Some("next"));
        let saved = memories.all().unwrap();
        assert_eq!(saved.len(), 2);
        assert_eq!(saved[1].text, "Release builds need the `ci` feature.");
        assert_eq!(saved[1].tags, ["build"]);
    }

    /// With the setting off the agent has no memory tools and no digest; turned on again, the next prompt brings the digest, and a compaction that summarized it away brings it once more.
    #[tokio::test]
    async fn the_memory_setting_and_compaction() {
        let (dir, memories) = with_memories("turn-memory-off");
        let (out, _events) = tokio::sync::mpsc::unbounded_channel();
        let shared = Shared::new(out);
        shared.set_memories(Some(memories));
        shared.open(Provider::Claude, vec!["m".into()], "m".into(), Mode::Asking);
        shared.set_memory(false);
        let model = Arc::new(Recording {
            seen: Mutex::new(Vec::new()),
        });
        let run = |history: Vec<Message>, prompt: &'static str| {
            let turn = turn_in(&dir, &shared, history);
            let model = model.clone();
            async move {
                match turn.run_with(model, Job::Prompt(prompt.into())).await {
                    Ended::Done(after) => after,
                    _ => panic!("turn failed"),
                }
            }
        };
        let mut history = run(Vec::new(), "one").await;
        shared.set_memory(true);
        history = run(history, "two").await;
        history.push(Message::Compaction(omc_providers::Compaction {
            summary: "s".into(),
            first_kept: history.len(),
            ..omc_providers::Compaction::default()
        }));
        run(history, "three").await;
        let seen = model.seen.lock().unwrap();
        let off = &seen[0];
        assert!(
            !tool_names(off)
                .iter()
                .any(|name| name.contains("memory") || *name == "remember")
        );
        assert!(!off.system.contains("# Memory"));
        assert!(!off.prompt.as_deref().unwrap().contains("<memory>"));
        let on = &seen[1];
        assert!(tool_names(on).contains(&"remember"));
        assert!(
            on.prompt
                .as_deref()
                .unwrap()
                .contains("Integration tests need Docker")
        );
        let after = seen[2].prompt.as_deref().unwrap();
        assert!(after.contains("<memory>") && after.ends_with("three"));
    }
}
