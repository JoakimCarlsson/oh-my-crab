//! The agent loop: call the model, run its tool calls, repeat until it answers.

mod agent;
mod approval;

pub use agent::{AgentError, AgentEvent, MAX_ITERATIONS, run_turn};
pub use approval::{ApprovalRequest, Approvals, Decision, Prompter};
