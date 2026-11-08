//! Extensions: programs crab starts and keeps running for a conversation, spoken to in JSON lines on their standard streams.
//!
//! Where a hook is a command run once per event, an extension stays up for the whole conversation. It says, when it starts, which events it wants and which tools it gives the model; crab then tells it of each such event and, where the event can be steered, waits for its answer: it can refuse or rewrite a tool call, replace a result, add context to a prompt, keep a turn going or keep a compaction from happening. The protocol is in `docs/extending/extensions.md`.
//!
//! [`config`] reads which extensions there are, [`Extensions`] runs them for one conversation, [`ExtensionHooks`] puts their answers to the agent loop, and their tools are [`Extensions::tools`].

pub mod config;
mod hooks;
mod host;
mod process;
mod tool;

pub use config::{
    EXTENSIONS_FILE_VARIABLE, EXTENSIONS_VARIABLE, Extension, Found, Origin, PROJECT_FILE,
};
pub use hooks::{ExtensionHooks, Prompted};
pub use host::{Extensions, Setting};
