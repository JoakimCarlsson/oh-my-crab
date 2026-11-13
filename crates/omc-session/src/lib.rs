//! A conversation with an agent, driven by commands and reported as events.
//!
//! [`run`] holds one conversation: it takes [`omc_wire::Command`]s, runs the
//! agent loop for each prompt, and reports what happens as
//! [`omc_wire::Event`]s. [`serve`] carries that over a pair of byte streams
//! so another program can hold the conversation.

mod bridge;
mod edits;
mod goal;
mod hooks;
mod languages;
mod lifecycle;
mod limits;
mod listing;
mod memory;
mod permissions;
mod picker;
mod plans;
mod prefix;
mod prompt;
mod providers;
mod queue;
mod serve;
mod session;
mod settings;
mod shared;
mod snapshots;
mod spend;
mod toolset;
mod translate;
mod turn;
mod warmer;

pub use hooks::{hooks, trust};
pub use limits::render as render_limits;
pub use listing::tools;
pub use memory::memory;
pub use permissions::{
    FILE_VARIABLE as PERMISSIONS_FILE_VARIABLE, add_permission, permissions, remove_permission,
};
pub use providers::{
    account, key_source, limits, limits_providers, providers, save_key, signed_in,
};
pub use serve::serve;
pub use session::{Options, run, run_with};
pub use session::{fork, history, import, rename};
