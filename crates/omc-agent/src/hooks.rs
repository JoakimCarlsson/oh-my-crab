//! Checks the user defines around the loop: before and after each tool call, and when a turn is about to end.
//!
//! The loop only asks; what is checked and how is up to the [`Hooks`] it is given, such as commands the user configured.

use std::sync::Arc;

use async_trait::async_trait;
use omc_providers::ToolCall;
use serde_json::Value;

/// What the checks before a tool call decided about it.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum Gate {
    /// No opinion: the call goes on and is asked about as usual.
    #[default]
    Pass,
    /// The call goes on without the reader being asked.
    Allow,
    /// The call is not run; the reason is what the model is told instead of a result.
    Deny(String),
}

/// What the checks before a tool call came to.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Screened {
    /// Whether the call may run.
    pub gate: Gate,
    /// The arguments to run it with instead of the model's, when a check rewrote them.
    pub arguments: Option<Value>,
    /// A note for the model, added to the call's result.
    pub note: Option<String>,
}

/// Checks run around the agent loop.
#[async_trait]
pub trait Hooks: Send + Sync {
    /// Checks `call`, whose arguments were read and put right against its tool's schema as `arguments`, before it is announced, asked about or run.
    async fn before_tool(&self, call: &ToolCall, arguments: &Value) -> Screened;

    /// A note for the model to read with the result of `call`, which ran with `arguments` and succeeded with `output`; nothing when there is none.
    async fn after_tool(&self, call: &ToolCall, arguments: &Value, output: &str) -> Option<String>;

    /// Why the agent should go on instead of ending its turn with `answer`, or nothing to let it end; `again` says the turn already went on because of such a check since the model last called a tool.
    async fn stop(&self, answer: &str, again: bool) -> Option<String>;

    /// The same checks, for the subagent `id` of the kind `kind`.
    fn for_subagent(&self, id: &str, kind: &str) -> Arc<dyn Hooks>;
}

/// How many times in a row the checks at the end of a turn may make it go on before the model last called a tool; past that the turn ends anyway.
pub const MAX_STOP_IN_A_ROW: usize = 8;

/// How many times in all the checks at the end of a turn may make one turn go on.
pub const MAX_STOP_PER_TURN: usize = 24;

/// Counts how often the checks at the end of a turn made it go on, so that a check that never lets go cannot hold the turn forever.
#[derive(Debug, Default)]
pub(crate) struct StopBound {
    /// Since the model last called a tool.
    in_a_row: usize,
    /// In the whole turn.
    total: usize,
}

impl StopBound {
    /// Notes that the model called tools, which starts the count in a row again.
    pub(crate) fn tools_called(&mut self) {
        self.in_a_row = 0;
    }

    /// The notice that makes the turn go on, when `hooks` ask it to instead of ending with `answer` and the bounds allow it once more.
    pub(crate) async fn next(&mut self, hooks: Option<&dyn Hooks>, answer: &str) -> Option<String> {
        let hooks = hooks?;
        if self.in_a_row >= MAX_STOP_IN_A_ROW || self.total >= MAX_STOP_PER_TURN {
            return None;
        }
        let reason = hooks.stop(answer, self.in_a_row > 0).await?;
        self.in_a_row += 1;
        self.total += 1;
        Some(format!(
            "<stop-hook>\nA stop hook the user set up asked you to go on instead of ending your turn:\n{}\n</stop-hook>",
            reason.trim()
        ))
    }
}

/// `text` with `note` from a hook added at its end, where the model reads it with the result.
pub(crate) fn with_note(mut text: String, note: &str) -> String {
    if !text.is_empty() {
        text.push_str("\n\n");
    }
    text.push_str("<hook-note>\n");
    text.push_str(note.trim());
    text.push_str("\n</hook-note>");
    text
}
