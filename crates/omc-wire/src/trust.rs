//! Whether the harness may use what a project folder brings, and what it left out until it may.

use std::path::PathBuf;

/// What kind of project input was left out.
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum Input {
    /// A hook: a command run around tool calls, prompts or the end of a turn.
    Hook,
    /// An instruction file, such as `AGENTS.md` or `CLAUDE.md`.
    Instruction,
    /// A skill.
    Skill,
    /// An agent definition for subagents.
    Agent,
}

/// One project input the harness left out.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Skipped {
    /// What kind of input it is.
    pub kind: Input,
    /// What it is called: the name of a skill or agent, the path of an instruction file, or a hook's event, matcher and command.
    pub name: String,
    /// The file it is defined in.
    pub path: PathBuf,
}

/// How the trust of a project stands.
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum Standing {
    /// Nobody decided yet.
    Unknown,
    /// The project was trusted, but it now has a hook the reader did not trust, added or changed since, so its hooks wait for trust again; its other inputs are still used.
    Changed,
    /// The reader decided not to trust it.
    Declined,
}

/// The harness left out what a project folder brings until the reader trusts it, and waits for a [`crate::Command::Trust`] answer under `ticket`; the conversation goes on without them meanwhile.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct TrustRequest {
    /// Names this request in the answer.
    pub ticket: u64,
    /// The folder trust is recorded for: the repository's main checkout, which every linked worktree of it shares, or the folder outside a repository.
    pub root: PathBuf,
    /// How its trust stands.
    pub standing: Standing,
    /// What was left out.
    pub skipped: Vec<Skipped>,
}
