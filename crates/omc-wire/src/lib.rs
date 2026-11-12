//! The protocol between a harness and the application that shows it.
//!
//! The application sends [`Command`]s and reads [`Event`]s, one JSON message
//! per line. This crate holds only the shapes of those messages and how they
//! are framed, so both ends can depend on it without depending on each other.

mod branch;
mod command;
mod event;
mod frame;
mod import;
mod inquiry;
mod language;
mod limits;
mod memory;
mod permission;
mod plan;
mod provider;
mod queue;
mod setting;
mod tool;
mod toolserver;
mod trust;
mod usage;

pub use branch::{Branch, BranchTurn};
pub use command::{Command, Opening};
pub use event::{Conversation, Event, Part, PastTurn};
pub use frame::{decode, encode};
pub use import::{Imported, ImportedTurn};
pub use inquiry::{Alternative, Inquiry, Question, Reply, summary};
pub use language::{ActionInfo, FileEdits, Place, ServerInfo, Stretch, SymbolInfo, TextEdit};
pub use language::{Diagnostic, LanguageAnswer, LanguageAsk, LanguageRequest, Severity};
pub use limits::{Limit, LimitUnit, Limits};
pub use memory::{MemoryEntry, MemoryListing};
pub use permission::{Choice, Permission, Verdict};
pub use plan::{PlanStep, Progress};
pub use provider::{KeySource, ProviderInfo, SignIn};
pub use queue::{QueueState, Queued};
pub use setting::{Pick, Setting, Slash};
pub use tool::{
    Background, Diff, Kind, Merge, Subagent, SubagentState, Tool, ToolInfo, ToolState, Worktree,
};
pub use toolserver::{Reach, ToolServer};
pub use trust::{Input, Skipped, Standing, TrustRequest};
pub use usage::Usage;
