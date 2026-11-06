//! What the agent remembers from one conversation to the next.
//!
//! Memories are short facts and lessons the agent saved, kept as JSON lines in
//! plain files under the harness's data folder: one file for each project,
//! which every conversation and every linked worktree of that project shares,
//! and one for the user across projects. [`Memories`] is what one
//! conversation reads and writes; [`search`] finds memories by keyword and
//! [`digest`] picks the ones a conversation starts with.

mod digest;
mod project;
mod search;
mod store;

pub use digest::{DIGEST_LIMIT, ENTRY_LIMIT, digest};
pub use project::Project;
pub use search::{score, search};
pub use store::{
    Added, MEMORY_LIMIT, Memories, Memory, MemoryError, ROOT_VARIABLE, SWITCH_VARIABLE, Scope,
    Shelf, Store, TAG_LIMIT, disabled_by_environment,
};

#[cfg(test)]
mod testutil;
