//! What the client asks of the harness.

use std::path::PathBuf;

use crate::inquiry::Reply;
use crate::toolserver::ToolServer;

/// How a conversation is opened.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Opening {
    /// The folder the agent works in.
    pub directory: PathBuf,
    /// The folder the logins are kept in, or the harness's own when there is none.
    pub home: Option<PathBuf>,
    /// The provider that serves the conversation, `claude` or `codex`.
    pub provider: String,
    /// The model to start with, or the provider's default when there is none.
    pub model: Option<String>,
    /// Whether the tools that change things run without asking.
    pub yolo: bool,
    /// The tool servers whose tools the agent is given besides its own.
    #[serde(default)]
    pub tools: Vec<ToolServer>,
    /// The names of the tools the agent is not given, whoever offers them.
    #[serde(default)]
    pub denied: Vec<String>,
    /// The instruction files (by path), skills and agent definitions (by name) the reader turned off.
    #[serde(default)]
    pub disabled: Vec<String>,
}

/// One request from the client, answered by [`crate::Event`]s.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum Command {
    /// Opens the conversation, which must come before anything else.
    Open(Opening),
    /// Starts a turn with these words and these files.
    Prompt {
        /// What the reader wrote.
        text: String,
        /// The files attached, by path.
        files: Vec<PathBuf>,
    },
    /// Answers the question under `ticket` with the choice `option`, or declines it.
    Choose {
        /// The question being answered.
        ticket: u64,
        /// The choice made, or nothing to decline.
        option: Option<String>,
    },
    /// Answers the form under `ticket` with `replies`, or abandons it.
    Answer {
        /// The form being answered.
        ticket: u64,
        /// What the reader answered, or nothing to abandon the form.
        replies: Option<Vec<Reply>>,
    },
    /// Stops the turn that is running.
    Cancel,
    /// Gives the setting `setting` the value `value`.
    Set {
        /// The setting.
        setting: String,
        /// The value.
        value: String,
    },
    /// Signs in to `provider`, reporting the address to open as a [`crate::Event::LoginUrl`].
    Login {
        /// The provider, `claude` or `codex`.
        provider: String,
    },
    /// Ends the conversation and the harness.
    Close,
}
