//! The agent loop: call the model, run its tool calls, repeat until it answers.

mod agent;

pub use agent::{AgentError, AgentEvent, MAX_ITERATIONS, run_turn};
