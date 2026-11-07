//! The tool abstraction, its registry and the built-in tools.

mod checkpoint;
mod coerce;
mod error;
mod host;
mod jobs;
mod language;
mod notebook;
mod program;
mod registry;
mod sandbox;
pub mod schema;
mod spill;
mod tool;
mod tools;

#[cfg(test)]
mod testutil;

pub use checkpoint::Checkpoint;
pub use error::ToolError;
pub use host::Host;
pub use jobs::{Finished, Jobs};
pub use language::{
    DIAGNOSTICS_WAIT, Diagnostic, FORMAT_WAIT, LanguageService, Languages, Severity,
};
pub use notebook::{NOTEBOOK_LIMIT, Notebook};
pub use program::find_program;
pub use registry::Registry;
pub use sandbox::Sandbox;
pub use spill::Spill;
pub use tool::{
    Caller, FileChange, Image, NoCaller, Tool, ToolOutput, marked_lines, parse_args, sanitize,
};
pub use tools::{
    Ask, AstEdit, AstGrep, Bash, CheckpointTool, ContextNotes, Edit, ExitPlanMode, Glob, Grep,
    MemoryEdit, MemorySearch, NewContext, Read, Remember, Rewind, SetTitle, Skill, Todo, Wait,
    WebSearch, Write, snapshot_tag,
};
