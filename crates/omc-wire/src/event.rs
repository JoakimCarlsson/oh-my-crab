//! What the harness tells its client, in the order it happened.

use crate::inquiry::Inquiry;
use crate::language::LanguageRequest;
use crate::permission::Permission;
use crate::plan::PlanStep;
use crate::queue::Queued;
use crate::setting::{Setting, Slash};
use crate::tool::{Background, Tool};
use crate::trust::TrustRequest;
use crate::usage::Usage;

/// One turn of a saved conversation: what the reader prompted and what the agent did, as the events it would have sent live.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PastTurn {
    /// The id the client gave the turn when it prompted, when it gave one.
    #[serde(default)]
    pub id: Option<String>,
    /// What the reader prompted with.
    pub prompt: String,
    /// The names of the files the reader attached.
    #[serde(default)]
    pub files: Vec<String>,
    /// What the agent did for it, in order.
    pub events: Vec<Event>,
}

/// A saved conversation: its name, the choices made about it and its turns.
#[derive(Clone, Debug, Default, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Conversation {
    /// The conversation's name, when it has one.
    #[serde(default)]
    pub title: Option<String>,
    /// The latest value chosen for each setting, by the setting's id.
    #[serde(default)]
    pub settings: Vec<(String, String)>,
    /// The turns, oldest first.
    pub turns: Vec<PastTurn>,
    /// What can be set, as it was last offered, with the choices made.
    #[serde(default)]
    pub offered: Vec<Setting>,
    /// The commands and skills that were last offered behind a slash.
    #[serde(default)]
    pub commands: Vec<Slash>,
    /// The context window of the model the conversation last used, in tokens.
    #[serde(default)]
    pub window: Option<u64>,
}

/// What the agent did, in the order it did it.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum Event {
    /// What can be changed about the conversation, and how it stands now.
    Settings(Vec<Setting>),
    /// The commands the agent offers behind a slash.
    Commands(Vec<Slash>),
    /// How full the conversation's context is now.
    Used(Usage),
    /// A new block of the agent's reply starts, of the kind given; the `Said` or `Thought` chunks that follow belong to it until the next block starts.
    Started(Part),
    /// The agent said this much more of its reply.
    Said(String),
    /// The agent thought this much more.
    Thought(String),
    /// The agent started using a tool, or said more about one it was using.
    Tool(Tool),
    /// A command or task left running beyond its call started, or ended.
    Background(Background),
    /// The turn was stopped, so that nothing the agent had running goes on.
    Halted,
    /// The agent's plan, whole, as it stands now.
    Plan(Vec<PlanStep>),
    /// The agent waits for an answer to this.
    Ask(Permission),
    /// The agent waits for the answers to this form of questions.
    Inquire(Inquiry),
    /// The harness asks the client's language services this, and waits for a [`crate::Command::Language`] answer under its ticket; sent only to a client that said, when it opened the conversation, that it answers them.
    Language(LanguageRequest),
    /// The turn ended, with the reason it failed where it did.
    Finished(Option<String>),
    /// The turn ended because the account needs signing in again.
    SignedOut {
        /// Why authentication failed.
        reason: String,
        /// The name of the account, when it has one.
        account: Option<String>,
    },
    /// The account was signed in.
    SignedIn,
    /// The address the reader opens in a browser to sign in; the client opens it, since the harness does not.
    LoginUrl(String),
    /// The page the reader opens to sign in, and the code to enter there.
    LoginCode {
        /// The page to open.
        url: String,
        /// The code to enter on it.
        code: String,
    },
    /// The harness could not start or went on, for this reason.
    Failed(String),
    /// The conversation as it was saved: each turn the reader prompted and what the agent did in it, oldest first, sent when a saved conversation is opened and again when it is cut back.
    Resumed(Conversation),
    /// The agent named the conversation, or named it again.
    Titled(String),
    /// A prompt sent while a turn was running waits, was put into that turn, started a turn of its own or was dropped; the steered or started prompt is the reader's from then on, as one sent between turns is.
    Queued(Queued),
    /// The harness left out what the project folder brings, its hooks, instruction files, skills and agent definitions, or some of them, until the reader trusts it, and waits for a [`crate::Command::Trust`] answer; sent when the conversation opens and again when what was left out changes.
    Trust(TrustRequest),
}

/// The kind of a block of the agent's reply.
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum Part {
    /// What the agent is thinking, sent as `Thought` chunks.
    Thinking,
    /// What the agent says, sent as `Said` chunks.
    Text,
}
