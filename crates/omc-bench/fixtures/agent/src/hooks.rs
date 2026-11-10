//! Checks the user defines around the loop: before and after each tool call, when a call needs the reader's permission, after each batch of calls, when a reply comes in, when a subagent starts, before and after the conversation is compacted, and when a turn is about to end.
//!
//! The loop only asks; what is checked and how is up to the [`Hooks`] it is given, such as commands the user configured.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use omc_providers::{Reply, ToolCall, ToolResult};
use serde_json::Value;

/// What the checks before a tool call decided about it.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum Gate {
    /// No opinion: the call goes on and is asked about as usual.
    #[default]
    Pass,
    /// The call goes on without the reader being asked.
    Allow,
    /// The reader is asked about the call, as when an ask rule matches it, even when it needs no approval.
    Ask,
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
    /// Why the turn stops once the batch of calls this one is in has ended, when a check asked for that.
    pub halt: Option<String>,
}

/// What the checks after a tool call came to.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Followed {
    /// A note for the model, added to the call's result.
    pub note: Option<String>,
    /// The text the model reads as the call's result instead of what the tool gave, when a check replaced it.
    pub output: Option<String>,
    /// Why the turn stops once the batch of calls this one is in has ended, when a check asked for that.
    pub halt: Option<String>,
}

/// What the checks said about a call the reader is about to be asked about.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum Permit {
    /// No opinion: the reader is asked.
    #[default]
    Pass,
    /// The call runs without the reader being asked.
    Allow {
        /// The arguments to run it with instead, when a check rewrote them.
        arguments: Option<Value>,
        /// Allow rules, in Claude Code's syntax, to add for the rest of the conversation.
        rules: Vec<String>,
    },
    /// The call is not run.
    Deny {
        /// What the model is told instead of a result.
        reason: String,
        /// Whether the turn stops once the batch of calls has ended.
        halt: bool,
    },
}

/// What the checks after a whole batch of tool calls came to.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Batched {
    /// A note for the model, added after the batch's results.
    pub note: Option<String>,
    /// Why the turn stops now instead of sending the results to the model, when a check asked for that.
    pub halt: Option<String>,
}

/// What started a compaction.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CompactTrigger {
    /// The reader asked for it, with `/compact`.
    Manual,
    /// The context neared the model's window, or the provider refused a request as too long.
    Auto,
}

impl CompactTrigger {
    /// The trigger's name as Claude Code's hooks are told it and match it: `manual` or `auto`.
    pub fn name(self) -> &'static str {
        match self {
            Self::Manual => "manual",
            Self::Auto => "auto",
        }
    }
}

/// What the checks before a compaction came to.
#[derive(Clone, Debug, PartialEq)]
pub enum Compacting {
    /// The compaction goes on, giving the summary these instructions besides the reader's focus, when there are any.
    Go(Option<String>),
    /// The compaction does not happen, for this reason.
    Blocked(String),
}

impl Default for Compacting {
    /// The compaction goes on with no instructions added.
    fn default() -> Self {
        Self::Go(None)
    }
}

/// Checks run around the agent loop.
#[async_trait]
pub trait Hooks: Send + Sync {
    /// Checks `call`, whose arguments were read and put right against its tool's schema as `arguments`, before it is announced, asked about or run.
    async fn before_tool(&self, call: &ToolCall, arguments: &Value) -> Screened;

    /// What the checks make of the result of `call`, which ran with `arguments` and succeeded with `output`: a note for the model to read with it, a replacement for it, and whether the turn stops.
    async fn after_tool(&self, call: &ToolCall, arguments: &Value, output: &str) -> Followed;

    /// What the checks make of the failure of `call`, which ran with `arguments` for `took` and failed with `error`; nothing, as by default.
    async fn after_tool_failed(
        &self,
        _call: &ToolCall,
        _arguments: &Value,
        _error: &str,
        _took: Duration,
    ) -> Followed {
        Followed::default()
    }

    /// Whether `call`, made with `arguments`, runs, is refused or is put to the reader, asked when no rule settled it and the reader is about to be asked; by default the reader is asked.
    async fn permission_request(&self, _call: &ToolCall, _arguments: &Value) -> Permit {
        Permit::Pass
    }

    /// What the checks make of a whole batch of `calls`, once each has its result in `results`, in the same order, and before the results go to the model; nothing, as by default.
    async fn after_batch(&self, _calls: &[ToolCall], _results: &[ToolResult]) -> Batched {
        Batched::default()
    }

    /// Tells the checks the model replied with `reply`, before its calls are run; by default nothing is told.
    async fn replied(&self, _reply: &Reply) {}

    /// Tells the checks the conversation was compacted, as `trigger` started it, into `summary`; by default nothing is told.
    async fn after_compact(&self, _trigger: CompactTrigger, _summary: &str) {}

    /// Why the agent should go on instead of ending its turn with `answer`, or nothing to let it end; `again` says the turn already went on because of such a check since the model last called a tool.
    async fn stop(&self, answer: &str, again: bool) -> Option<String>;

    /// Whether a compaction started by `trigger`, whose summary the reader asked to give particular attention to `focus`, goes on and with what instructions; by default it goes on as it is.
    async fn before_compact(&self, _trigger: CompactTrigger, _focus: Option<&str>) -> Compacting {
        Compacting::default()
    }

    /// Context for a subagent that is about to start, which its assignment begins with; nothing when there is none, as by default and for the main agent.
    async fn subagent_start(&self) -> Option<String> {
        None
    }

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

/// Several sets of checks run as one, in order: the shell hooks and the extensions of a conversation, for instance.
///
/// Before a call, each set sees the arguments the sets before it left; a denial wins over asking and asking over allowing. After a call, each sees the result the sets before it left. Every note is kept, in order, and the first reason to stop the turn wins.
pub struct Chained {
    /// The sets of checks, in the order they run.
    links: Vec<Arc<dyn Hooks>>,
}

impl Chained {
    /// The checks of `links`, run in that order; nothing when there are none, and the one set itself when there is one.
    pub fn of(links: Vec<Arc<dyn Hooks>>) -> Option<Arc<dyn Hooks>> {
        match links.len() {
            0 => None,
            1 => links.into_iter().next(),
            _ => Some(Arc::new(Self { links })),
        }
    }
}

/// `parts` joined by blank lines, or nothing when there are none.
fn joined(parts: Vec<String>) -> Option<String> {
    (!parts.is_empty()).then(|| parts.join("\n\n"))
}

#[async_trait]
impl Hooks for Chained {
    /// Runs each set in turn on the arguments the ones before it left, stopping at the first denial.
    async fn before_tool(&self, call: &ToolCall, arguments: &Value) -> Screened {
        let mut current = arguments.clone();
        let mut rewritten = None;
        let mut notes = Vec::new();
        let mut halt = None;
        let mut gate = Gate::Pass;
        for link in &self.links {
            let screened = link.before_tool(call, &current).await;
            if let Some(arguments) = screened.arguments {
                current = arguments.clone();
                rewritten = Some(arguments);
            }
            notes.extend(screened.note);
            halt = halt.or(screened.halt);
            gate = match (gate, screened.gate) {
                (_, Gate::Deny(reason)) => {
                    gate = Gate::Deny(reason);
                    break;
                }
                (Gate::Ask, _) | (_, Gate::Ask) => Gate::Ask,
                (Gate::Allow, _) | (_, Gate::Allow) => Gate::Allow,
                _ => Gate::Pass,
            };
        }
        Screened {
            gate,
            arguments: rewritten,
            note: joined(notes),
            halt,
        }
    }

    /// Runs each set in turn on the result the ones before it left.
    async fn after_tool(&self, call: &ToolCall, arguments: &Value, output: &str) -> Followed {
        let mut current = output.to_owned();
        let mut replaced = None;
        let mut notes = Vec::new();
        let mut halt = None;
        for link in &self.links {
            let followed = link.after_tool(call, arguments, &current).await;
            if let Some(output) = followed.output {
                current = output.clone();
                replaced = Some(output);
            }
            notes.extend(followed.note);
            halt = halt.or(followed.halt);
        }
        Followed {
            note: joined(notes),
            output: replaced,
            halt,
        }
    }

    /// Runs each set on the failure, keeping every note.
    async fn after_tool_failed(
        &self,
        call: &ToolCall,
        arguments: &Value,
        error: &str,
        took: Duration,
    ) -> Followed {
        let mut notes = Vec::new();
        let mut halt = None;
        for link in &self.links {
            let followed = link.after_tool_failed(call, arguments, error, took).await;
            notes.extend(followed.note);
            halt = halt.or(followed.halt);
        }
        Followed {
            note: joined(notes),
            output: None,
            halt,
        }
    }

    /// Asks each set in turn: the first denial refuses the call; otherwise the last allowance runs it, with the rules of every allowance.
    async fn permission_request(&self, call: &ToolCall, arguments: &Value) -> Permit {
        let mut allowed: Option<(Option<Value>, Vec<String>)> = None;
        for link in &self.links {
            match link.permission_request(call, arguments).await {
                Permit::Pass => {}
                Permit::Deny { reason, halt } => return Permit::Deny { reason, halt },
                Permit::Allow { arguments, rules } => {
                    let (kept, mut all) = allowed.take().unwrap_or_default();
                    all.extend(rules);
                    allowed = Some((arguments.or(kept), all));
                }
            }
        }
        match allowed {
            Some((arguments, rules)) => Permit::Allow { arguments, rules },
            None => Permit::Pass,
        }
    }

    /// Runs each set on the batch, keeping every note.
    async fn after_batch(&self, calls: &[ToolCall], results: &[ToolResult]) -> Batched {
        let mut notes = Vec::new();
        let mut halt = None;
        for link in &self.links {
            let batched = link.after_batch(calls, results).await;
            notes.extend(batched.note);
            halt = halt.or(batched.halt);
        }
        Batched {
            note: joined(notes),
            halt,
        }
    }

    /// Tells every set of the reply.
    async fn replied(&self, reply: &Reply) {
        for link in &self.links {
            link.replied(reply).await;
        }
    }

    /// Tells every set of the compaction.
    async fn after_compact(&self, trigger: CompactTrigger, summary: &str) {
        for link in &self.links {
            link.after_compact(trigger, summary).await;
        }
    }

    /// The reasons every set gives to go on, joined; nothing when none gives one.
    async fn stop(&self, answer: &str, again: bool) -> Option<String> {
        let mut reasons = Vec::new();
        for link in &self.links {
            reasons.extend(link.stop(answer, again).await);
        }
        joined(reasons)
    }

    /// Blocked when any set blocks; otherwise going on with the instructions of every set.
    async fn before_compact(&self, trigger: CompactTrigger, focus: Option<&str>) -> Compacting {
        let mut added = Vec::new();
        for link in &self.links {
            match link.before_compact(trigger, focus).await {
                Compacting::Blocked(reason) => return Compacting::Blocked(reason),
                Compacting::Go(instructions) => added.extend(instructions),
            }
        }
        Compacting::Go(joined(added))
    }

    /// The context of every set, joined.
    async fn subagent_start(&self) -> Option<String> {
        let mut added = Vec::new();
        for link in &self.links {
            added.extend(link.subagent_start().await);
        }
        joined(added)
    }

    /// Every set, run for the subagent.
    fn for_subagent(&self, id: &str, kind: &str) -> Arc<dyn Hooks> {
        Arc::new(Self {
            links: self
                .links
                .iter()
                .map(|link| link.for_subagent(id, kind))
                .collect(),
        })
    }
}
