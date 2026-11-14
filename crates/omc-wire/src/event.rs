//! What the harness tells its client, in the order it happened.

use crate::branch::Branch;
use crate::change::TurnChange;
use crate::goal::Goal;
use crate::inquiry::Inquiry;
use crate::language::LanguageRequest;
use crate::permission::Permission;
use crate::plan::PlanStep;
use crate::queue::Queued;
use crate::server::Server;
use crate::setting::{Setting, Slash};
use crate::slot::{PresetInfo, SlotInfo};
use crate::spend::Spent;
use crate::tool::{Background, Subagent, Tool};
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
    /// The id of the branch `turns` are of, or nothing for a conversation that has none saved.
    #[serde(default)]
    pub branch: Option<String>,
    /// Every branch of the conversation, in the order they were made, the first one first.
    #[serde(default)]
    pub branches: Vec<Branch>,
    /// What the conversation has cost so far, when it has cost something; nothing from a harness that predates the field.
    #[serde(default)]
    pub spent: Option<Spent>,
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
    /// A subagent started, or ended.
    Subagent(Subagent),
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
    /// The conversation as it was saved: each turn the reader prompted and what the agent did in it, oldest first, sent when a saved conversation is opened, again when it goes back to before a turn, and when it switches to another branch.
    Resumed(Conversation),
    /// The agent named the conversation, or named it again.
    Titled(String),
    /// A prompt sent while a turn was running waits, was put into that turn, started a turn of its own or was dropped; the steered or started prompt is the reader's from then on, as one sent between turns is.
    Queued(Queued),
    /// The harness left out what the project folder brings, its hooks, instruction files, skills and agent definitions, or some of them, until the reader trusts it, and waits for a [`crate::Command::Trust`] answer; sent when the conversation opens and again when what was left out changes.
    Trust(TrustRequest),
    /// The conversation's goal was set, or how it stands changed; sent when the conversation opens too, when it has one.
    Goal(Goal),
    /// The harness started a turn of its own toward the goal, since the last turn ended with the goal open; the turn's events follow as for a prompt the reader sent.
    Continued {
        /// The prompt the turn started with.
        prompt: String,
        /// The id the harness saved the turn under, by which the client goes back to before it and asks about its files, as with the id of a turn it prompted itself.
        turn: String,
    },
    /// What a turn changed in the files, in answer to a [`crate::Command::Changes`].
    Changes {
        /// The id of the turn.
        turn: String,
        /// Whether the turn is still running, so that the files are set against what they hold now and more may follow.
        open: bool,
        /// The files the turn changed, each with its diff.
        files: Vec<TurnChange>,
    },
    /// The files were put back as they were before a turn, in answer to a [`crate::Command::Restore`].
    Restored {
        /// The id of the turn.
        turn: String,
        /// The files that changed, from the root of the repository.
        files: Vec<String>,
    },
    /// What a turn changed was taken back out of the files, in answer to a [`crate::Command::Undo`].
    Undone {
        /// The id of the turn.
        turn: String,
        /// The files that changed, from the root of the repository.
        files: Vec<String>,
    },
    /// What the conversation has cost so far, whole, for each model it asked, subagents and cache warming included; sent after every model request that was billed, and when a saved conversation that cost something is opened.
    Spent(Spent),
    /// The answer to a question asked on the side with a [`crate::Command::Btw`], which is not part of the conversation; a question that cannot be answered is told with a [`Event::Failed`] instead.
    Aside {
        /// What the reader asked.
        question: String,
        /// What the model answered.
        answer: String,
    },
    /// The conversation was handed over to a new one, in answer to a [`crate::Command::Handoff`]: it started afresh, and a turn starts with `prompt`, whose events follow as for a prompt the reader sent.
    HandedOff {
        /// The prompt the model wrote to hand the conversation over, which the new conversation starts with.
        prompt: String,
        /// The id the harness saved the new conversation's first turn under, as with the id of a turn the client prompted itself.
        turn: String,
    },
    /// The OpenAI-compatible servers declared in the harness's configuration, in order, in answer to a [`crate::Command::Servers`], [`crate::Command::SaveServer`] or [`crate::Command::RemoveServer`]; no API key is sent back.
    Servers(Vec<Server>),
    /// The models a server offers, in answer to the [`crate::Command::FindModels`] under `ticket`.
    FoundModels {
        /// The ticket of the command answered.
        ticket: u64,
        /// The model ids, in the order the server gave them; empty when it could not be asked.
        models: Vec<String>,
        /// Why the server could not be asked, or nothing when it was.
        error: Option<String>,
    },
    /// The model slots, built-in ones first, then custom ones by name, in answer to [`crate::Command::Slots`], [`crate::Command::SetSlot`], [`crate::Command::ClearSlot`] and [`crate::Command::ApplyPreset`].
    Slots(Vec<SlotInfo>),
    /// The presets kept, by name, in answer to [`crate::Command::Presets`], [`crate::Command::SavePreset`] and [`crate::Command::DeletePreset`].
    Presets(Vec<PresetInfo>),
    /// A request to the model of `slot` failed, after it was sent again as often as is worth it, in a way another model may not, so the rest of the turn asks `to`, the slot's next backup, instead of `from`.
    ModelFallback {
        /// The slot whose backup is used, such as `main` or `plan`.
        slot: String,
        /// The model that failed.
        from: String,
        /// The model asked from now on in this turn.
        to: String,
        /// Why the request failed, as the provider said.
        reason: String,
    },
}

/// The kind of a block of the agent's reply.
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum Part {
    /// What the agent is thinking, sent as `Thought` chunks.
    Thinking,
    /// What the agent says, sent as `Said` chunks.
    Text,
}
