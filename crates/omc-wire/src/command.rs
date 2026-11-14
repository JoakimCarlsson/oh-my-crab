//! What the client asks of the harness.

use std::path::PathBuf;

use crate::inquiry::Reply;
use crate::language::LanguageAnswer;
use crate::permission::Verdict;
use crate::server::Server;
use crate::slot::SlotScope;
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
    /// Asks what the turn `turn` changed in the files of the folder the agent works in, answered with a [`crate::Event::Changes`]: the files as they were when the turn ended set against the files as they were before it, or against the files as they are now for the turn that is running. Fails with a [`crate::Event::Failed`] when no files were kept for the turn.
    Changes {
        /// The id the client gave the turn when it prompted.
        turn: String,
    },
    /// Puts the files of the folder the agent works in back as they were before the turn `turn`, and says so with a [`crate::Event::Restored`]; the conversation stays as it is, and the agent is told with the next prompt. Fails with a [`crate::Event::Failed`] while a turn is running or when no files were kept before the turn.
    Restore {
        /// The id the client gave the turn when it prompted.
        turn: String,
    },
    /// Takes what the turn `turn` changed back out of the files `paths`, or out of every file it changed when there are none, leaving what was done to them since, and says so with a [`crate::Event::Undone`]; the agent is told with the next prompt. Fails with a [`crate::Event::Failed`], changing nothing, while a turn is running, when no files were kept for the turn, or when lines the turn changed were changed again since.
    Undo {
        /// The id the client gave the turn when it prompted.
        turn: String,
        /// The files, from the root of the repository, as [`crate::TurnChange::path`] gives them; a file the turn renamed is put back only when both its names are here.
        #[serde(default)]
        paths: Vec<String>,
    },
    /// Ends the conversation and the harness.
    Close,
    /// Takes the last reply back out of the conversation and answers the reader's last prompt again: the conversation is cut back to before that prompt, the client is told the turns left with a [`crate::Event::Resumed`], and a turn starts with the prompt as the reader wrote it, under the id it was saved with. The same as the prompt `/retry`. Fails with a [`crate::Event::Failed`] while a turn is running or when the conversation has no prompt to answer again.
    Retry,
    /// Starts the conversation afresh: every message is taken out of what the model is given, the settings, the name, the goal and the notebook stay, and the client is told the turns left with a [`crate::Event::Resumed`]; a saved conversation keeps the turns, recorded as lost. The same as the prompt `/fresh`. Fails with a [`crate::Event::Failed`] while a turn is running.
    Fresh,
    /// Asks the model `question` on the side: it is shown the conversation as it stood when the last turn ended and answers with a [`crate::Event::Aside`], without tools, and neither the question nor the answer is added to the conversation. Allowed while a turn is running, which goes on undisturbed. The same as the prompt `/btw <question>`.
    Btw {
        /// What the reader asks.
        question: String,
    },
    /// Hands the work over to a new conversation: a turn of its own has the model write a prompt that hands the conversation over, toward `goal` when one is given, then the conversation starts afresh as [`Command::Fresh`] does, the client is told the prompt with a [`crate::Event::HandedOff`], and a turn starts with it as the first turn of the new conversation. The same as the prompt `/handoff [goal]`. Fails with a [`crate::Event::Failed`] while a turn is running.
    Handoff {
        /// What the new conversation should work toward, or nothing to carry on with what this one was doing.
        #[serde(default)]
        goal: Option<String>,
    },
    /// Asks for the OpenAI-compatible servers declared in the harness's configuration, answered with a [`crate::Event::Servers`]. Allowed before [`Command::Open`] and while a turn is running.
    Servers,
    /// Adds the server, or replaces the one with the same id, in the harness's configuration, and answers with the servers as they now stand in a [`crate::Event::Servers`]; the server is a provider from then on, for conversations opened after. Fails with a [`crate::Event::Failed`], changing nothing, when the server is not valid. Allowed before [`Command::Open`] and while a turn is running.
    SaveServer(Server),
    /// Takes the server `id` out of the harness's configuration, and answers with the servers left in a [`crate::Event::Servers`]. Fails with a [`crate::Event::Failed`] when no server has that id. Allowed before [`Command::Open`] and while a turn is running.
    RemoveServer {
        /// The id of the server.
        id: String,
    },
    /// Asks which models `server` offers, saved or not, answered with a [`crate::Event::FoundModels`] under `ticket`: the models it was given, or those it lists at `GET {base_url}/models`. A server sent without `api_key` uses the key saved for its id. Allowed before [`Command::Open`] and while a turn is running.
    FindModels {
        /// The number the answer comes back under, chosen by the client.
        ticket: u64,
        /// The server to ask.
        server: Server,
    },
    /// Asks for the model slots, answered with a [`crate::Event::Slots`]: each slot with the model it resolves to, where that comes from, and a suggestion for a slot with no model of its own. Allowed before [`Command::Open`], when the project's file and the conversation's model are not known yet, and while a turn is running.
    Slots,
    /// Sets the slot `name` in the file of `scope`: its model and its backups, replacing what that file held for it; a custom name is made. Answered with the slots as they now stand in a [`crate::Event::Slots`], or a [`crate::Event::Failed`], changing nothing, when a name or selector is not valid or the file cannot be written. Applies from the next request.
    SetSlot {
        /// The slot's name: letters, digits, `-` and `_`.
        name: String,
        /// The file to write.
        #[serde(default)]
        scope: SlotScope,
        /// The model selector, as `provider/model` or `model` with an optional `:effort`; nothing keeps only the backups.
        #[serde(default)]
        model: Option<String>,
        /// The selectors tried in order when the model fails, as the model is written.
        #[serde(default)]
        backups: Vec<String>,
        /// The provider id whose assignment is set, such as `claude`; nothing sets the any-provider assignment.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider: Option<String>,
    },
    /// Takes the slot `name` out of the file of `scope`, so that it falls back to the other file or to the conversation's model. Answered like [`Command::SetSlot`].
    ClearSlot {
        /// The slot's name.
        name: String,
        /// The file to change.
        #[serde(default)]
        scope: SlotScope,
        /// The provider id whose assignment is taken out; nothing takes out the any-provider assignment.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider: Option<String>,
    },
    /// Asks for the presets kept in the user's file, answered with a [`crate::Event::Presets`]. Allowed before [`Command::Open`] and while a turn is running.
    Presets,
    /// Saves every slot as it stands now, with the conversation's effort or `effort` when given, as the preset `name`, replacing one of that name. Answered with a [`crate::Event::Presets`], or a [`crate::Event::Failed`].
    SavePreset {
        /// The preset's name: letters, digits, `-` and `_`.
        name: String,
        /// The effort to keep with it instead of the conversation's.
        #[serde(default)]
        effort: Option<String>,
    },
    /// Sets every slot the file of `scope` holds to what the preset `name` holds, and the conversation's effort to its effort when it has one and the model takes it. Answered with a [`crate::Event::Slots`], or a [`crate::Event::Failed`] when there is no such preset.
    ApplyPreset {
        /// The preset's name.
        name: String,
        /// The file whose slots are replaced.
        #[serde(default)]
        scope: SlotScope,
    },
    /// Takes the preset `name` out of the user's file. Answered with a [`crate::Event::Presets`], or a [`crate::Event::Failed`] when there is no such preset.
    DeletePreset {
        /// The preset's name.
        name: String,
    },
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

    /// The session commands read from the lines a client writes, a hand-off without a goal too, and survive a trip through a line.
    #[test]
    fn session_commands_round_trip() {
        let lines = [
            (r#""Retry""#, Command::Retry),
            (r#""Fresh""#, Command::Fresh),
            (
                r#"{"Btw":{"question":"why?"}}"#,
                Command::Btw {
                    question: "why?".to_owned(),
                },
            ),
            (r#"{"Handoff":{}}"#, Command::Handoff { goal: None }),
            (
                r#"{"Handoff":{"goal":"ship it"}}"#,
                Command::Handoff {
                    goal: Some("ship it".to_owned()),
                },
            ),
        ];
        for (line, command) in lines {
            assert_eq!(crate::decode::<Command>(line).unwrap(), command);
            assert_eq!(
                crate::decode::<Command>(&crate::encode(&command).unwrap()).unwrap(),
                command
            );
        }
        let aside = crate::Event::Aside {
            question: "why?".to_owned(),
            answer: "because".to_owned(),
        };
        let line = crate::encode(&aside).unwrap();
        assert_eq!(
            line.trim_end(),
            r#"{"Aside":{"question":"why?","answer":"because"}}"#
        );
        let handed = crate::Event::HandedOff {
            prompt: "carry on".to_owned(),
            turn: "handoff-1".to_owned(),
        };
        let line = crate::encode(&handed).unwrap();
        assert_eq!(crate::decode::<crate::Event>(&line).unwrap(), handed);
    }

    /// The slot and preset commands read from the lines a client writes, with the scope left out meaning the user's file, and they and their answers survive a trip through a line.
    #[test]
    fn slot_commands_round_trip() {
        let set: Command =
            crate::decode(r#"{"SetSlot":{"name":"fast","model":"claude/haiku:low"}}"#).unwrap();
        assert_eq!(
            set,
            Command::SetSlot {
                name: "fast".to_owned(),
                scope: SlotScope::User,
                model: Some("claude/haiku:low".to_owned()),
                backups: Vec::new(),
                provider: None,
            }
        );
        let codex: Command =
            crate::decode(r#"{"SetSlot":{"name":"fast","model":"gpt-5-mini","provider":"codex"}}"#)
                .unwrap();
        assert!(matches!(
            codex,
            Command::SetSlot { provider: Some(ref id), .. } if id == "codex"
        ));
        let commands = [
            Command::Slots,
            set,
            codex,
            Command::ClearSlot {
                name: "plan".to_owned(),
                scope: SlotScope::Project,
                provider: None,
            },
            Command::ClearSlot {
                name: "plan".to_owned(),
                scope: SlotScope::User,
                provider: Some("claude".to_owned()),
            },
            Command::Presets,
            Command::SavePreset {
                name: "cheap".to_owned(),
                effort: Some("low".to_owned()),
            },
            Command::ApplyPreset {
                name: "cheap".to_owned(),
                scope: SlotScope::User,
            },
            Command::DeletePreset {
                name: "cheap".to_owned(),
            },
        ];
        for command in commands {
            assert_eq!(
                crate::decode::<Command>(&crate::encode(&command).unwrap()).unwrap(),
                command
            );
        }
        let events = [
            crate::Event::Slots(vec![crate::SlotInfo {
                name: "fast".to_owned(),
                model: None,
                backups: Vec::new(),
                resolved: Some("m".to_owned()),
                source: crate::SlotSource::Main,
                inherited: true,
                suggested: Some("claude-haiku".to_owned()),
                providers: vec![crate::SlotProvider {
                    provider: "codex".to_owned(),
                    model: Some("gpt-5-mini".to_owned()),
                    backups: Vec::new(),
                    suggested: None,
                }],
            }]),
            crate::Event::Presets(vec![crate::PresetInfo {
                name: "cheap".to_owned(),
                slots: vec![crate::PresetSlot {
                    name: "fast".to_owned(),
                    model: Some("haiku".to_owned()),
                    backups: vec!["openai/gpt-5-mini".to_owned()],
                    providers: vec![crate::SlotProvider {
                        provider: "claude".to_owned(),
                        model: Some("claude/haiku".to_owned()),
                        backups: Vec::new(),
                        suggested: None,
                    }],
                }],
                effort: None,
            }]),
            crate::Event::ModelFallback {
                slot: "main".to_owned(),
                from: "a".to_owned(),
                to: "b".to_owned(),
                reason: "provider returned 529: overloaded".to_owned(),
            },
        ];
        for event in events {
            assert_eq!(
                crate::decode::<crate::Event>(&crate::encode(&event).unwrap()).unwrap(),
                event
            );
        }
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
