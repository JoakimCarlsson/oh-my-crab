//! The agent loop: call the model, run its tool calls, repeat until it answers.

mod agent;
mod approval;
mod journal;
mod task;

pub use agent::{AgentError, AgentEvent, Context, Live, run_turn, run_turn_recorded};
pub use approval::{ApprovalRequest, Approvals, Decision, Prompter};
pub use journal::{Journal, JournalError, Scribe};
pub use task::{EventSink, PromptBuilder, Task, TaskSettings};

#[cfg(test)]
mod testkit;
#[cfg(test)]
mod tool_tests;
