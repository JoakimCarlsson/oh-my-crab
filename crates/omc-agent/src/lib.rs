//! The agent loop: call the model, run its tool calls, repeat until it answers.

mod agent;
mod approval;
mod aside;
mod backend;
mod board;
mod compaction;
mod cow;
mod crew;
mod fallback;
mod files;
mod hooks;
mod journal;
mod loop_guard;
mod mailbox;
mod meter;
mod nesting;
mod open_calls;
pub mod permissions;
mod picker;
mod plans;
mod pruning;
mod recall;
mod recovery;
mod review;
mod roster;
mod shake;
mod steering;
mod submit;
mod task;
mod todos;
mod worktree;

pub use agent::{
    AgentError, AgentEvent, Context, Live, compact, resume_turn, run_turn, run_turn_recorded,
    run_turn_steered,
};
pub use approval::{ApprovalRequest, Approvals, Decision, Keeper, Place, Prompter, Standing};
pub use aside::{answer_aside, write_handoff};
pub use backend::{Backend, ISOLATION_VARIABLE, isolation_backend};
pub use compaction::{COMPACT, context, replayed};
pub use crew::{CREW_MOST, Crew, crew_tools};
pub use fallback::{Fallback, Routing, worth_switching};
pub use hooks::{
    Batched, Chained, CompactTrigger, Compacting, Followed, Gate, Hooks, MAX_STOP_IN_A_ROW,
    MAX_STOP_PER_TURN, Permit, Screened,
};
pub use journal::{
    Branch, FIRST_BRANCH, INTERRUPTED, INTERRUPTED_CALL, Journal, JournalError, Lost, Mark, Marked,
    STOPPED, STOPPED_NOTE, Saved, Scribe,
};
pub use meter::charge;
pub use nesting::{DEFAULT_DEPTH, DEPTH_VARIABLE, MOST_DEPTH, Nesting, Throttle, max_depth};
pub use picker::{Category, ModelPicker, Models, Picked};
pub use plans::approved_plan;
pub use recall::{RECALL, spec as recall_spec};
pub use review::{Verdict, completion_refused, plan_returned, review_completion, review_plan};
pub use roster::{KEPT_FOR, KEPT_MOST, Roster, SubagentNotice, SubagentState};
pub use steering::Steering;
pub use task::{EventSink, MAX_CONCURRENT, MAX_TASKS, PromptBuilder, Task, TaskSettings};
pub use worktree::{Isolation, LOCK_REASON, Merge, ToolsFor, prune as prune_worktrees};

#[cfg(test)]
mod category_tests;
#[cfg(test)]
mod compaction_tests;
#[cfg(test)]
mod crew_tests;
#[cfg(test)]
mod follow_up_tests;
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
mod spend_tests;
#[cfg(test)]
mod steering_tests;
#[cfg(test)]
mod stop_tests;
#[cfg(test)]
mod task_tests;
#[cfg(test)]
mod testkit;
#[cfg(test)]
mod todo_tests;
#[cfg(test)]
mod tool_tests;
