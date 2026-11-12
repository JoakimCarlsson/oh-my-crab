//! What the client asks of the harness.

use std::path::PathBuf;

use crate::inquiry::Reply;
use crate::language::LanguageAnswer;
use crate::permission::Verdict;
use crate::toolserver::ToolServer;

/// How a conversation is opened.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Opening {
    /// The folder the agent works in.
    pub directory: PathBuf,
    /// The folder the logins are kept in, or the harness's own when there is none.
    pub home: Option<PathBuf>,
    /// The provider that serves the conversation, the id of one of the providers in `docs/providers/supported.md`, such as `claude`.
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
    /// The id the client gave the conversation; its history is saved under it and loaded again when it is opened, and nothing is saved when there is none.
    #[serde(default)]
    pub conversation: Option<String>,
    /// The folder the history files are kept in, or a folder under the harness's own data folder when there is none.
    #[serde(default)]
    pub history: Option<PathBuf>,
    /// The number of turns of the saved history to keep, or all of it when there is none; a turn is one prompt and everything the agent did for it.
    #[serde(default)]
    pub keep: Option<usize>,
    /// The values to give settings the saved conversation has no choice for, as setting ids and values; a choice that was saved wins over these.
    #[serde(default)]
    pub defaults: Vec<(String, String)>,
    /// The id of the turn to go back to before when the saved history is loaded: the conversation goes on from before it on a new branch, and it and the turns after it stay saved on the branch they were on, which [`Command::Switch`] goes back to. Opening fails when no turn has the id.
    #[serde(default)]
    pub before: Option<String>,
    /// Whether the client answers [`crate::Event::Language`] requests: it runs language servers of its own, so the harness asks it to format the files it writes and to say what is wrong with them, and starts none itself.
    #[serde(default)]
    pub languages: bool,
    /// Whether the client, which set `languages`, also answers the questions about code the `lsp` tool asks: definitions, references, hovers, symbols, the diagnostics of the whole project and renames, as the [`crate::LanguageAsk`] variants besides `Format` and `Diagnostics`. A client that leaves it out is never sent those, so a client written before they existed keeps working; the harness then refuses those questions itself. Means nothing without `languages`.
    #[serde(default)]
    pub navigation: bool,
}

/// One request from the client, answered by [`crate::Event`]s.
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum Command {
    /// Opens the conversation, which must come before anything else.
    ///
    /// Sent while a turn of the conversation open before is running, it cancels that turn as [`Command::Cancel`] does, with a [`crate::Event::Halted`] and a [`crate::Event::Finished`], and drops the prompts queued for it, each reported as [`crate::Event::Queued`] in the dropped state, before the new conversation is opened.
    Open(Opening),
    /// Starts a turn with these words and these files.
    ///
    /// Sent while a turn is running, the prompt is queued and reported as a [`crate::Event::Queued`]: it is put into the running turn after its current batch of tool calls, or starts a turn of its own when that turn ends first. A `/compact` sent while a turn is running is refused with a [`crate::Event::Failed`].
    Prompt {
        /// What the reader wrote.
        text: String,
        /// The files attached, by path.
        files: Vec<PathBuf>,
        /// The id the client knows the turn by, which is saved with the turn so that the client can go back to before it, or leave it out to have none.
        #[serde(default)]
        turn: Option<String>,
        /// Words the model is told before the reader's, which are not the reader's and so are not saved as what they wrote.
        #[serde(default)]
        context: Option<String>,
    },
    /// Answers the question under `ticket` with the choice `option`, or declines it.
    Choose {
        /// The question being answered.
        ticket: u64,
        /// The choice made, or nothing to decline.
        option: Option<String>,
        /// What the reader wrote along with the choice, which the agent is told; only a plan's choices pass it on, and a client that never sends it leaves it out.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        note: Option<String>,
    },
    /// Answers the form under `ticket` with `replies`, or abandons it.
    Answer {
        /// The form being answered.
        ticket: u64,
        /// What the reader answered, or nothing to abandon the form.
        replies: Option<Vec<Reply>>,
    },
    /// Names the conversation, replacing any name it has, and says so with a [`crate::Event::Titled`].
    Rename {
        /// The new name.
        title: String,
    },
    /// Stops the turn that is running.
    Cancel,
    /// Stops the subagent `id`, as a [`crate::Event::Subagent`] named it, and the subagents it started; the others and the turn go on, and the agent is told the reader stopped it. Fails with a [`crate::Event::Failed`] when no subagent with that id is running.
    StopSubagent {
        /// The id of the subagent.
        id: String,
    },
    /// Gives the setting `setting` the value `value`.
    Set {
        /// The setting.
        setting: String,
        /// The value.
        value: String,
    },
    /// Signs in to `provider`, reporting what to open as a [`crate::Event::LoginUrl`], or the page and code as a [`crate::Event::LoginCode`].
    Login {
        /// The id of the provider.
        provider: String,
    },
    /// Answers the language request under `ticket`; a ticket the harness no longer waits on is ignored.
    Language {
        /// The request being answered.
        ticket: u64,
        /// The answer.
        answer: LanguageAnswer,
    },
    /// Answers the trust request under `ticket`: [`Verdict::Always`] trusts the project for good, recording it for every worktree of the repository, [`Verdict::Allow`] trusts it for this conversation only, and [`Verdict::Reject`] records that it is not trusted. What a trusted project brings is used from the next turn on; a ticket the harness no longer waits on is ignored.
    Trust {
        /// The request being answered.
        ticket: u64,
        /// The reader's decision.
        verdict: Verdict,
    },
    /// Switches the saved conversation to the branch `branch`, at its tip, and tells the client the turns it holds with a [`crate::Event::Resumed`]; a running turn is cancelled first, as [`Command::Cancel`] does. Fails with a [`crate::Event::Failed`] when the conversation is not saved or has no branch with that id.
    Switch {
        /// The id of the branch, as [`crate::Branch::id`] gives it.
        branch: String,
    },
    /// Ends the conversation and the harness.
    Close,
}

#[cfg(test)]
/// Tests for the opening request.
mod tests {
    use super::*;

    /// An opening request from a client that predates the conversation fields still reads, and the fields are empty.
    #[test]
    fn an_opening_without_the_history_fields_reads() {
        let opening: Opening = serde_json::from_str(
            r#"{"directory":"/w","home":null,"provider":"claude","model":null,"yolo":false}"#,
        )
        .unwrap();
        assert_eq!(opening.conversation, None);
        assert_eq!(opening.history, None);
        assert_eq!(opening.keep, None);
    }

    /// A choice without a note, as a client written before notes sends it, still reads, and one with a note keeps it through a trip through a line.
    #[test]
    fn a_choice_reads_with_or_without_a_note() {
        let bare: Command = crate::decode(r#"{"Choose":{"ticket":1,"option":"allow"}}"#).unwrap();
        assert_eq!(
            bare,
            Command::Choose {
                ticket: 1,
                option: Some("allow".to_owned()),
                note: None
            }
        );
        assert!(!crate::encode(&bare).unwrap().contains("note"));
        let noted = Command::Choose {
            ticket: 2,
            option: Some("reject".to_owned()),
            note: Some("split step 2".to_owned()),
        };
        assert_eq!(
            crate::decode::<Command>(&crate::encode(&noted).unwrap()).unwrap(),
            noted
        );
    }

    /// A trust answer and a trust request each survive a trip through a line, in the shape a client writes and reads.
    #[test]
    fn trust_round_trips() {
        let answer: Command =
            crate::decode(r#"{"Trust":{"ticket":3,"verdict":"Always"}}"#).unwrap();
        assert_eq!(
            answer,
            Command::Trust {
                ticket: 3,
                verdict: Verdict::Always
            }
        );
        assert_eq!(
            crate::decode::<Command>(&crate::encode(&answer).unwrap()).unwrap(),
            answer
        );
        let request = crate::Event::Trust(crate::TrustRequest {
            ticket: 3,
            root: PathBuf::from("/w"),
            standing: crate::Standing::Unknown,
            skipped: vec![crate::Skipped {
                kind: crate::Input::Hook,
                name: "PreToolUse bash: ./guard.sh".to_owned(),
                path: PathBuf::from("/w/.claude/settings.json"),
            }],
        });
        let line = crate::encode(&request).unwrap();
        assert!(line.starts_with(r#"{"Trust":{"ticket":3,"root":"/w","standing":"Unknown","#));
        assert_eq!(crate::decode::<crate::Event>(&line).unwrap(), request);
    }
}
