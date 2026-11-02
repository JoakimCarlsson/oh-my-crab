//! The tool-calling loop.

use futures_util::future::join_all;
use omc_providers::{ChatModel, Message, ProviderError, ToolCall, ToolResult, ToolSpec};
use omc_tools::Registry;
use thiserror::Error;

use crate::{ApprovalRequest, Approvals};

/// Most model requests one user turn may make before the loop gives up.
pub const MAX_ITERATIONS: usize = 25;

/// Something the UI should show while a turn runs.
#[derive(Debug, Clone, PartialEq)]
pub enum AgentEvent {
    /// A chunk of streamed assistant text.
    Delta(String),
    /// The model asked for a tool call.
    ToolStart {
        /// Id of the call.
        id: String,
        /// Tool name.
        name: String,
        /// One-line description of the call.
        summary: String,
    },
    /// A tool call finished, successfully or not.
    ToolDone {
        /// Id of the call.
        id: String,
        /// Tool name.
        name: String,
        /// Result text or error description.
        text: String,
        /// Whether the call failed.
        is_error: bool,
    },
}

/// Why a turn ended without a final answer.
#[derive(Debug, Error)]
pub enum AgentError {
    /// The model request failed.
    #[error(transparent)]
    Provider(#[from] ProviderError),
    /// The model kept calling tools past [`MAX_ITERATIONS`].
    #[error("stopped after {0} model requests without a final answer")]
    IterationLimit(usize),
}

/// Runs one user turn: sends `prompt`, executes tool calls and loops until the
/// model answers without calling a tool.
///
/// Tools that need approval run only when `approvals` allows the call; a denied
/// call is answered to the model with an error result. New messages are appended to `history`. When the turn fails, `history` is
/// restored to what it was before the call, so it never holds a tool call
/// without its result. Tool failures are not errors here: they are sent to the
/// model as results. `events` observes streamed text and tool activity.
///
/// # Errors
///
/// Returns [`AgentError::Provider`] when a model request fails and
/// [`AgentError::IterationLimit`] after [`MAX_ITERATIONS`] requests.
pub async fn run_turn(
    model: &dyn ChatModel,
    registry: &Registry,
    approvals: &Approvals,
    history: &mut Vec<Message>,
    prompt: String,
    events: &(dyn Fn(AgentEvent) + Send + Sync),
) -> Result<(), AgentError> {
    let start = history.len();
    history.push(Message::User(prompt));
    let result = drive(model, registry, approvals, history, events).await;
    if result.is_err() {
        history.truncate(start);
    }
    result
}

/// Repeats model request and tool execution until no tool is requested.
///
/// # Errors
///
/// Same as [`run_turn`].
async fn drive(
    model: &dyn ChatModel,
    registry: &Registry,
    approvals: &Approvals,
    history: &mut Vec<Message>,
    events: &(dyn Fn(AgentEvent) + Send + Sync),
) -> Result<(), AgentError> {
    let specs = tool_specs(registry);
    for _ in 0..MAX_ITERATIONS {
        let mut on_delta = |delta: &str| events(AgentEvent::Delta(delta.to_string()));
        let reply = model.complete(history, &specs, &mut on_delta).await?;
        let calls = reply.tool_calls.clone();
        history.push(Message::Assistant(reply));
        if calls.is_empty() {
            return Ok(());
        }
        let results = run_calls(registry, approvals, &calls, events).await;
        history.push(Message::ToolResults(results));
    }
    Err(AgentError::IterationLimit(MAX_ITERATIONS))
}

/// Describes every registered tool in the provider-neutral form.
fn tool_specs(registry: &Registry) -> Vec<ToolSpec> {
    registry
        .iter()
        .map(|tool| ToolSpec {
            name: tool.name().to_string(),
            description: tool.description().to_string(),
            schema: tool.schema(),
        })
        .collect()
}

/// Runs the calls of one model reply and returns their results in call order.
///
/// Every call is announced first. Calls that need approval are asked about one
/// at a time, in order. Approved calls then run one after another so edits
/// cannot race, while calls that need no approval run concurrently.
async fn run_calls(
    registry: &Registry,
    approvals: &Approvals,
    calls: &[ToolCall],
    events: &(dyn Fn(AgentEvent) + Send + Sync),
) -> Vec<ToolResult> {
    let mut slots: Vec<Option<ToolResult>> = vec![None; calls.len()];
    let mut concurrent = Vec::new();
    let mut sequential = Vec::new();
    for (index, call) in calls.iter().enumerate() {
        let tool = registry.get(&call.name);
        let summary = tool.map_or_else(
            || call.arguments.to_string(),
            |t| t.describe(&call.arguments),
        );
        events(AgentEvent::ToolStart {
            id: call.id.clone(),
            name: call.name.clone(),
            summary: summary.clone(),
        });
        match tool {
            None => {
                let message = format!("unknown tool: {}", call.name);
                slots[index] = Some(finish(call, message, true, events));
            }
            Some(tool) if tool.needs_approval() => {
                let request = ApprovalRequest {
                    tool: call.name.clone(),
                    summary,
                };
                if approvals.allows(&request).await {
                    sequential.push(index);
                } else {
                    let message = "the user denied this tool call".to_string();
                    slots[index] = Some(finish(call, message, true, events));
                }
            }
            Some(_) => concurrent.push(index),
        }
    }
    let done = join_all(
        concurrent
            .iter()
            .map(|&index| run_call(registry, &calls[index], events)),
    )
    .await;
    for (index, result) in concurrent.into_iter().zip(done) {
        slots[index] = Some(result);
    }
    for index in sequential {
        slots[index] = Some(run_call(registry, &calls[index], events).await);
    }
    slots.into_iter().flatten().collect()
}

/// Runs one approved tool call and reports its outcome.
///
/// A failing tool yields a result with `is_error` set.
async fn run_call(
    registry: &Registry,
    call: &ToolCall,
    events: &(dyn Fn(AgentEvent) + Send + Sync),
) -> ToolResult {
    let (text, is_error) = match registry.call(&call.name, call.arguments.clone()).await {
        Ok(output) => (output.text, false),
        Err(error) => (error.to_string(), true),
    };
    finish(call, text, is_error, events)
}

/// Reports the outcome of `call` through `events` and builds its result.
fn finish(
    call: &ToolCall,
    text: String,
    is_error: bool,
    events: &(dyn Fn(AgentEvent) + Send + Sync),
) -> ToolResult {
    events(AgentEvent::ToolDone {
        id: call.id.clone(),
        name: call.name.clone(),
        text: text.clone(),
        is_error,
    });
    ToolResult {
        call_id: call.id.clone(),
        name: call.name.clone(),
        text,
        is_error,
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};

    use async_trait::async_trait;
    use omc_providers::{DeltaSink, Reply};
    use serde_json::json;

    use super::*;
    use crate::{Decision, Prompter};

    /// A model that plays back a fixed list of replies.
    struct Scripted {
        /// Replies still to give, in order; the last one repeats.
        replies: Mutex<Vec<Reply>>,
        /// Number of messages seen at each request.
        seen: Mutex<Vec<usize>>,
    }

    impl Scripted {
        /// Creates a model that answers with `replies` in order.
        fn new(replies: Vec<Reply>) -> Self {
            Self {
                replies: Mutex::new(replies),
                seen: Mutex::new(Vec::new()),
            }
        }
    }

    #[async_trait]
    impl ChatModel for Scripted {
        /// Records the history length and returns the next scripted reply.
        async fn complete(
            &self,
            messages: &[Message],
            _tools: &[ToolSpec],
            on_delta: DeltaSink<'_>,
        ) -> Result<Reply, ProviderError> {
            self.seen.lock().unwrap().push(messages.len());
            let mut replies = self.replies.lock().unwrap();
            let reply = if replies.len() > 1 {
                replies.remove(0)
            } else {
                replies[0].clone()
            };
            if !reply.text.is_empty() {
                on_delta(&reply.text);
            }
            Ok(reply)
        }
    }

    /// Builds a reply that calls `tool` with `arguments`.
    fn call_reply(id: &str, tool: &str, arguments: serde_json::Value) -> Reply {
        Reply {
            text: String::new(),
            tool_calls: vec![ToolCall {
                id: id.into(),
                name: tool.into(),
                arguments,
            }],
        }
    }

    /// A prompter that denies every request.
    struct DenyAll;

    #[async_trait]
    impl Prompter for DenyAll {
        /// Always answers [`Decision::Deny`].
        async fn ask(&self, _request: &ApprovalRequest) -> Decision {
            Decision::Deny
        }
    }

    /// Builds a policy that denies every tool needing approval.
    fn deny_all() -> Approvals {
        Approvals::new(Arc::new(DenyAll), false)
    }

    /// Creates an empty scratch directory under the workspace's `build/`.
    fn scratch(name: &str) -> PathBuf {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../build/tests/omc-agent")
            .join(name);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// The model asks for `read`, gets the file, then answers.
    #[tokio::test]
    async fn runs_a_tool_then_answers() {
        let dir = scratch("loop-read");
        std::fs::write(dir.join("hello.txt"), "hello from disk\n").unwrap();
        let registry = Registry::builtin(&dir).unwrap();
        let model = Scripted::new(vec![
            call_reply("t1", "read", json!({ "path": "hello.txt" })),
            Reply {
                text: "it says hello".into(),
                tool_calls: Vec::new(),
            },
        ]);
        let log = Mutex::new(Vec::new());
        let events = |event: AgentEvent| log.lock().unwrap().push(event);
        let mut history = Vec::new();
        run_turn(
            &model,
            &registry,
            &deny_all(),
            &mut history,
            "read it".into(),
            &events,
        )
        .await
        .unwrap();
        assert_eq!(history.len(), 4);
        let Message::ToolResults(results) = &history[2] else {
            panic!("expected tool results, got {:?}", history[2]);
        };
        assert!(!results[0].is_error);
        assert!(results[0].text.contains("hello from disk"));
        assert_eq!(*model.seen.lock().unwrap(), vec![1, 3]);
        let log = log.lock().unwrap();
        assert!(matches!(log[0], AgentEvent::ToolStart { .. }));
        assert!(matches!(
            log[1],
            AgentEvent::ToolDone {
                is_error: false,
                ..
            }
        ));
        assert_eq!(log[2], AgentEvent::Delta("it says hello".into()));
    }

    /// Several calls in one reply all run and all get a result, tool errors included.
    #[tokio::test]
    async fn runs_parallel_calls_and_reports_failures() {
        let dir = scratch("loop-parallel");
        std::fs::write(dir.join("a.txt"), "a\n").unwrap();
        let registry = Registry::builtin(&dir).unwrap();
        let first = Reply {
            text: String::new(),
            tool_calls: vec![
                ToolCall {
                    id: "1".into(),
                    name: "read".into(),
                    arguments: json!({ "path": "a.txt" }),
                },
                ToolCall {
                    id: "2".into(),
                    name: "read".into(),
                    arguments: json!({ "path": "missing.txt" }),
                },
                ToolCall {
                    id: "3".into(),
                    name: "nope".into(),
                    arguments: json!({}),
                },
            ],
        };
        let model = Scripted::new(vec![first, Reply::default()]);
        let mut history = Vec::new();
        run_turn(
            &model,
            &registry,
            &deny_all(),
            &mut history,
            "go".into(),
            &|_| {},
        )
        .await
        .unwrap();
        let Message::ToolResults(results) = &history[2] else {
            panic!("expected tool results");
        };
        let flags: Vec<bool> = results.iter().map(|r| r.is_error).collect();
        assert_eq!(flags, vec![false, true, true]);
        assert_eq!(results[2].call_id, "3");
    }

    /// A model that never stops calling tools is cut off and the history is restored.
    #[tokio::test]
    async fn stops_at_the_iteration_limit() {
        let dir = scratch("loop-limit");
        std::fs::write(dir.join("a.txt"), "a\n").unwrap();
        let registry = Registry::builtin(&dir).unwrap();
        let model = Scripted::new(vec![call_reply("t", "read", json!({ "path": "a.txt" }))]);
        let mut history = vec![Message::User("earlier".into())];
        let result = run_turn(
            &model,
            &registry,
            &deny_all(),
            &mut history,
            "loop".into(),
            &|_| {},
        )
        .await;
        assert!(matches!(result, Err(AgentError::IterationLimit(_))));
        assert_eq!(model.seen.lock().unwrap().len(), MAX_ITERATIONS);
        assert_eq!(history, vec![Message::User("earlier".into())]);
    }

    /// A denied edit does not touch the file and the model is told why.
    #[tokio::test]
    async fn denied_tools_do_not_run() {
        let dir = scratch("loop-denied");
        std::fs::write(dir.join("f.txt"), "before\n").unwrap();
        let registry = Registry::builtin(&dir).unwrap();
        let edit = json!({ "path": "f.txt", "old_string": "before", "new_string": "after" });
        let model = Scripted::new(vec![call_reply("e", "edit", edit), Reply::default()]);
        let mut history = Vec::new();
        run_turn(
            &model,
            &registry,
            &deny_all(),
            &mut history,
            "edit".into(),
            &|_| {},
        )
        .await
        .unwrap();
        let Message::ToolResults(results) = &history[2] else {
            panic!("expected tool results");
        };
        assert!(results[0].is_error);
        assert!(results[0].text.contains("denied"));
        assert_eq!(
            std::fs::read_to_string(dir.join("f.txt")).unwrap(),
            "before\n"
        );
    }

    /// In yolo mode the same edit runs without asking.
    #[tokio::test]
    async fn yolo_runs_tools_without_asking() {
        let dir = scratch("loop-yolo");
        std::fs::write(dir.join("f.txt"), "before\n").unwrap();
        let registry = Registry::builtin(&dir).unwrap();
        let edit = json!({ "path": "f.txt", "old_string": "before", "new_string": "after" });
        let model = Scripted::new(vec![call_reply("e", "edit", edit), Reply::default()]);
        let approvals = Approvals::new(Arc::new(DenyAll), true);
        let mut history = Vec::new();
        run_turn(
            &model,
            &registry,
            &approvals,
            &mut history,
            "edit".into(),
            &|_| {},
        )
        .await
        .unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.join("f.txt")).unwrap(),
            "after\n"
        );
    }
}
