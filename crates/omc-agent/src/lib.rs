//! The agent loop: call the model, run its tool calls, repeat until it answers.

mod agent;
mod approval;
mod task;

pub use agent::{AgentError, AgentEvent, Context, Live, run_turn};
pub use approval::{ApprovalRequest, Approvals, Decision, Prompter};
pub use task::{EventSink, PromptBuilder, Task, TaskSettings};

#[cfg(test)]
mod testkit;
#[cfg(test)]
mod tool_tests;
