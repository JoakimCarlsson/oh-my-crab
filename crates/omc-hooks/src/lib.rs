//! Hooks and project trust.
//!
//! Hooks are commands the user, a plugin or a trusted project sets up to run
//! at points of a conversation: before and after a tool call, when a prompt is
//! submitted and when a turn is about to end. Each reads a JSON payload on
//! standard input and answers on standard output and with its exit code, in the
//! format of Claude Code's hooks, so the hooks a user already has keep working.
//! [`load`] reads their definitions, [`Engine`] runs them for the agent loop.
//!
//! What a project folder brings, hooks, instruction files, skills and agent
//! definitions, is used only once the project is trusted; [`assess`] says what
//! a conversation may use and what it leaves out, and [`TrustStore`] records
//! the reader's decisions per repository.

mod answer;
mod config;
mod engine;
mod matcher;
mod run;
mod trust;

#[cfg(test)]
mod testutil;

pub use answer::Submitted;
pub use config::{
    HOOKS_FILE_VARIABLE, Hook, HookEvent, HookOrigin, Loaded, PROJECT_FILES, load, own_file, parse,
};
pub use engine::{Engine, ModeNow, Setting};
pub use matcher::{Matcher, claude_names};
pub use run::{Outcome, run};
pub use trust::{
    Assessment, Record, Standing, TRUST_ALL_VARIABLE, TRUST_FILE_VARIABLE, TrustStore, assess,
    project_hooks, trusted_by_environment,
};
