//! The tool abstraction, its registry and the built-in tools.

mod checkpoint;
mod coerce;
mod deferral;
mod error;
mod host;
mod jobs;
mod language;
mod navigation;
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
pub use deferral::{DEFAULT_DEFERRED, DEFERRED_VARIABLE, Deferral};
pub use error::ToolError;
pub use host::{Host, PlanVerdict};
pub use jobs::{Finished, Jobs};
pub use language::{
    DIAGNOSTICS_WAIT, Diagnostic, FORMAT_WAIT, LanguageService, Languages, Severity,
};
pub use navigation::{
    Applied, Chosen, CodeAction, LanguageError, Location, Lookup, Renamed, ServerStatus, Spot,
    Symbol,
};
pub use notebook::{NOTEBOOK_LIMIT, Notebook};
pub use program::find_program;
pub use registry::Registry;
pub use sandbox::Sandbox;
pub use spill::Spill;
pub use tool::{
    Caller, Concurrency, FileChange, Image, NoCaller, Subject, Tool, ToolOutput, marked_lines,
    parse_args, sanitize,
};
pub use tools::{
    Ask, AstEdit, AstGrep, Bash, CheckpointTool, ContextNotes, Edit, ExitPlanMode, Glob, Grep,
    LOAD_TOOLS, LoadTools, Lsp, MemoryEdit, MemorySearch, NewContext, Read, Remember, Rewind,
    SetTitle, Skill, Todo, Wait, WebSearch, Write, attached_image, one_line_summary, snapshot_tag,
};
