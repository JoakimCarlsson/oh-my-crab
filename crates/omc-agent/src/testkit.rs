//! Helpers shared by the tests that drive the agent loop with scripted models.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use omc_providers::{ChatModel, ProviderError, Reply, Request, Sink, ToolCall};
use omc_tools::Host;
use omc_wire::{PlanStep, Question, Reply as Answer};
use serde_json::{Value, json};

use crate::{ApprovalRequest, Approvals, Decision, Prompter};

/// What a [`Router`] answers a request with.
pub type Answerer = dyn Fn(&RequestSeen) -> Reply + Send + Sync;

/// What a request carried, owned.
pub struct RequestSeen {
    /// The system prompt.
    pub system: String,
    /// The number of messages in the history.
    pub messages: usize,
    /// The names of the tools offered.
    pub tools: Vec<String>,
    /// The last user message, when there is one.
    pub last_user: Option<String>,
}

/// A model that answers each request by asking a function, and keeps what it was asked.
pub struct Router {
    /// Decides the reply.
    pub answer: Box<Answerer>,
    /// Every request seen, in order.
    pub seen: Mutex<Vec<RequestSeen>>,
}

impl Router {
    /// A model answering with `answer`.
    pub fn new(answer: impl Fn(&RequestSeen) -> Reply + Send + Sync + 'static) -> Arc<Self> {
        Arc::new(Self {
            answer: Box::new(answer),
            seen: Mutex::new(Vec::new()),
        })
    }
}

#[async_trait]
impl ChatModel for Router {
    /// Records the request and returns what the function says.
    async fn complete(
        &self,
        request: Request<'_>,
        sink: &mut dyn Sink,
    ) -> Result<Reply, ProviderError> {
        let seen = RequestSeen {
            system: request.system.to_owned(),
            messages: request.messages.len(),
            tools: request.tools.iter().map(|tool| tool.name.clone()).collect(),
            last_user: request
                .messages
                .iter()
                .rev()
                .find_map(|message| match message {
                    omc_providers::Message::User(text) => Some(text.clone()),
                    _ => None,
                }),
        };
        let reply = (self.answer)(&seen);
        self.seen.lock().unwrap().push(seen);
        if !reply.text.is_empty() {
            sink.text(&reply.text);
        }
        Ok(reply)
    }
}

/// A reply that calls `tool` with `arguments` under the id `id`.
pub fn call(id: &str, tool: &str, arguments: Value) -> Reply {
    Reply {
        tool_calls: vec![ToolCall {
            id: id.into(),
            name: tool.into(),
            arguments,
        }],
        ..Reply::default()
    }
}

/// A reply that says `text` and calls no tool.
pub fn say(text: &str) -> Reply {
    Reply {
        text: text.into(),
        ..Reply::default()
    }
}

/// A prompter that denies every request.
pub struct DenyAll;

#[async_trait]
impl Prompter for DenyAll {
    /// Always answers [`Decision::Deny`].
    async fn ask(&self, _request: &ApprovalRequest) -> Decision {
        Decision::Deny
    }
}

/// A policy that denies every tool needing approval.
pub fn deny_all() -> Arc<Approvals> {
    Arc::new(Approvals::new(Arc::new(DenyAll), false))
}

/// Creates an empty scratch directory under the workspace's `build/`.
pub fn scratch(name: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../build/tests/omc-agent")
        .join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A reader that records what it is shown and answers as it is told.
#[derive(Default)]
pub struct Reader {
    /// The plans shown.
    pub plans: Mutex<Vec<Vec<PlanStep>>>,
    /// The forms put.
    pub forms: Mutex<Vec<Vec<Question>>>,
    /// What the reader answers to forms.
    pub replies: Mutex<Option<Vec<Answer>>>,
    /// Whether the reader approves plans.
    pub approve: Mutex<bool>,
}

#[async_trait]
impl Host for Reader {
    /// Records `steps`.
    fn plan(&self, steps: Vec<PlanStep>) {
        self.plans.lock().unwrap().push(steps);
    }

    /// Records the form and answers with the scripted replies.
    async fn inquire(&self, questions: Vec<Question>) -> Option<Vec<Answer>> {
        self.forms.lock().unwrap().push(questions);
        self.replies.lock().unwrap().clone()
    }

    /// Takes any name, since the tests here never look at it.
    fn name(&self, _title: &str) -> bool {
        true
    }

    /// Answers with the scripted verdict: approved, in the window the plan was made in, or sent back.
    async fn approve_plan(&self, _plan: &str) -> omc_tools::PlanVerdict {
        if *self.approve.lock().unwrap() {
            omc_tools::PlanVerdict::Approved {
                fresh: false,
                note: None,
            }
        } else {
            omc_tools::PlanVerdict::Refine(None)
        }
    }
}

/// The arguments of a read of `path`.
pub fn read_args(path: &str) -> Value {
    json!({ "path": path })
}

/// What a subagent said in `result`, the text of its `task` result: the text without the line naming the handle it is kept under.
pub fn said(result: &str) -> &str {
    result
        .split("\n\n(Subagent kept as ")
        .next()
        .unwrap_or(result)
}
