//! The protocol between a harness and the application that shows it.
//!
//! The application sends [`Command`]s and reads [`Event`]s, one JSON message
//! per line. This crate holds only the shapes of those messages and how they
//! are framed, so both ends can depend on it without depending on each other.

mod command;
mod event;
mod frame;
mod import;
mod inquiry;
mod permission;
mod plan;
mod setting;
mod tool;
mod toolserver;
mod usage;

pub use command::{Command, Opening};
pub use event::{Conversation, Event, PastTurn};
pub use frame::{decode, encode};
pub use import::{Imported, ImportedTurn};
pub use inquiry::{Alternative, Inquiry, Question, Reply, summary};
pub use permission::{Choice, Permission, Verdict};
pub use plan::{PlanStep, Progress};
pub use setting::{Pick, Setting, Slash};
pub use tool::{Background, Diff, Kind, Tool, ToolInfo, ToolState};
pub use toolserver::{Reach, ToolServer};
pub use usage::Usage;
