//! A conversation a client kept itself, handed to the harness to keep instead.

use crate::setting::{Setting, Slash};

/// One turn of a conversation a client kept: what the reader prompted and what the agent said back, without the tools it used.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ImportedTurn {
    /// The id the client knows the turn by.
    pub id: String,
    /// What the reader prompted with.
    pub prompt: String,
    /// The names of the files the reader attached.
    #[serde(default)]
    pub files: Vec<String>,
    /// What the agent said in its reply, in one piece.
    #[serde(default)]
    pub said: String,
    /// How the turn ended when it did not end with an answer: `stopped`, `interrupted`, or the reason it failed.
    #[serde(default)]
    pub end: Option<String>,
}

/// A conversation a client kept itself, which the harness saves as its own.
#[derive(Clone, Debug, Default, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Imported {
    /// The conversation's name, when it has one.
    #[serde(default)]
    pub title: Option<String>,
    /// The latest value chosen for each setting.
    #[serde(default)]
    pub settings: Vec<(String, String)>,
    /// What can be set, as it was last offered.
    #[serde(default)]
    pub offered: Vec<Setting>,
    /// The commands that were last offered behind a slash.
    #[serde(default)]
    pub commands: Vec<Slash>,
    /// The context window of the model the conversation last used.
    #[serde(default)]
    pub window: Option<u64>,
    /// The turns, oldest first.
    pub turns: Vec<ImportedTurn>,
}
