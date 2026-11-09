//! The agent loop: call the model, run its tool calls, repeat until it answers.

mod agent;
mod approval;
mod backend;
mod compaction;
mod cow;
mod files;
mod hooks;
mod journal;
mod loop_guard;
mod nesting;
pub mod permissions;
mod picker;
mod plans;
mod pruning;
mod recall;
mod recovery;
mod shake;
mod steering;
mod submit;
mod task;
mod text_calls;
mod todos;
mod worktree;

pub use agent::{
    AgentError, AgentEvent, Context, Live, compact, run_turn, run_turn_recorded, run_turn_steered,
};
pub use approval::{ApprovalRequest, Approvals, Decision, Keeper, Place, Prompter, Standing};
pub use backend::{Backend, ISOLATION_VARIABLE, isolation_backend};
pub use compaction::{COMPACT, context, replayed};
pub use hooks::{
    Batched, Chained, CompactTrigger, Compacting, Followed, Gate, Hooks, MAX_STOP_IN_A_ROW,
    MAX_STOP_PER_TURN, Permit, Screened,
};
pub use journal::{
    Branch, FIRST_BRANCH, INTERRUPTED, Journal, JournalError, Lost, Mark, Marked, STOPPED, Saved,
    Scribe,
};
pub use nesting::{DEFAULT_DEPTH, DEPTH_VARIABLE, MOST_DEPTH, Nesting, Throttle, max_depth};
pub use picker::{ModelPicker, Models, Picked};
pub use plans::approved_plan;
pub use recall::{RECALL, spec as recall_spec};
pub use steering::Steering;
pub use task::{EventSink, MAX_CONCURRENT, MAX_TASKS, PromptBuilder, Task, TaskSettings};
pub use worktree::{Isolation, LOCK_REASON, Merge, ToolsFor, prune as prune_worktrees};

#[cfg(test)]
mod compaction_tests;
#[cfg(test)]
mod hooks_tests;
#[cfg(test)]
mod nesting_tests;
#[cfg(test)]
mod recovery_tests;
#[cfg(test)]
mod scheduling_tests;
#[cfg(test)]
mod script_tests;
#[cfg(test)]
mod steering_tests;
#[cfg(test)]
mod task_tests;
#[cfg(test)]
mod testkit;
#[cfg(test)]
mod todo_tests;
#[cfg(test)]
mod tool_tests;
