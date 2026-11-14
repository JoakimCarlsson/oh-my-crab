//! The commands that take the conversation back or start it over: answering the last prompt again, starting afresh, asking on the side and handing the work over to a new conversation.

use omc_providers::Message;
use omc_wire::Event;

use super::{START_CLEAR, Session};
use crate::picker::SessionPicker;
use crate::slots;
use crate::spend::Priced;
use crate::turn::{Ended, Job, connect};

/// The command that answers the last prompt again.
pub(super) const RETRY_COMMAND: &str = "retry";

/// The command that starts the conversation afresh.
pub(super) const FRESH_COMMAND: &str = "fresh";

/// The command that asks a question on the side.
pub(super) const BTW_COMMAND: &str = "btw";

/// The command that hands the work over to a new conversation.
pub(super) const HANDOFF_COMMAND: &str = "handoff";

/// Why the turns of a saved conversation that was started afresh are recorded as lost.
const FRESH_REASON: &str = "the reader started the conversation afresh";

/// Why the turns of a saved conversation that was handed over are recorded as lost.
const HANDOFF_REASON: &str = "the conversation was handed over to a new one";

/// One of the commands this module carries out, as the reader asked for it.
#[derive(Debug, PartialEq)]
pub(super) enum Asked {
    /// Answer the last prompt again.
    Retry,
    /// Start the conversation afresh.
    Fresh,
    /// Ask this question on the side.
    Btw(String),
    /// Hand the work over to a new conversation, toward this goal when there is one.
    Handoff(Option<String>),
}

impl Asked {
    /// The name the command is typed under, after the slash.
    fn name(&self) -> &'static str {
        match self {
            Self::Retry => RETRY_COMMAND,
            Self::Fresh => FRESH_COMMAND,
            Self::Btw(_) => BTW_COMMAND,
            Self::Handoff(_) => HANDOFF_COMMAND,
        }
    }
}

/// The command `text` asks for when it is `/retry`, `/fresh`, `/btw` followed by a question or `/handoff` followed by an optional goal; nothing when it asks for something else. What follows `/retry` and `/fresh` is ignored.
pub(super) fn request(text: &str) -> Option<Asked> {
    let rest = text.trim().strip_prefix('/')?;
    let (name, after) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
    let after = after.trim();
    match name {
        RETRY_COMMAND => Some(Asked::Retry),
        FRESH_COMMAND => Some(Asked::Fresh),
        BTW_COMMAND => Some(Asked::Btw(after.to_owned())),
        HANDOFF_COMMAND => Some(Asked::Handoff(
            Some(after.to_owned()).filter(|goal| !goal.is_empty()),
        )),
        _ => None,
    }
}

/// An id for the first turn of a conversation handed over, unlike any other: the time to the nanosecond, after `handoff-`.
fn handoff_id() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_nanos());
    format!("handoff-{now}")
}

/// Why a model could not be had, as `ended` says it.
fn reason_of(ended: Ended) -> String {
    match ended {
        Ended::SignedOut(reason) | Ended::Failed(reason) => reason,
        Ended::Done(_) | Ended::HandedOff(_) => "the model could not be reached".to_owned(),
    }
}

impl Session {
    /// Carries out `asked`, which the client sent as a prompt with `files` when `slashed` is set and as a command of its own otherwise.
    ///
    /// A prompt that asks for one of these commands takes no files, and one that starts afresh ends with [`Event::Finished`], so that every prompt sent between turns that starts no turn ends with it. Whatever cannot be done is told with [`Event::Failed`].
    pub(super) fn carry_out(&mut self, asked: Asked, files: &[std::path::PathBuf], slashed: bool) {
        let name = asked.name();
        if !files.is_empty() {
            return self.say(Event::Failed(format!(
                "/{name} takes no files; send them with a prompt instead"
            )));
        }
        if self.open.is_none() {
            return self.say(Event::Failed("the conversation is not open".to_owned()));
        }
        if self.running.is_some() && !matches!(asked, Asked::Btw(_)) {
            return self.say(Event::Failed(format!(
                "/{name} cannot run while a turn is running; send it again when the turn has finished"
            )));
        }
        match asked {
            Asked::Retry => self.retry(),
            Asked::Fresh => {
                if self.start_afresh(FRESH_REASON) && slashed {
                    self.say(Event::Finished(None));
                }
            }
            Asked::Btw(question) => self.aside(question),
            Asked::Handoff(goal) => self.start(Job::Handoff(goal), None),
        }
    }

    /// Takes the last reply back out of the conversation and answers the reader's last prompt again: the conversation is cut back to before that prompt, the client is told the turns left, and a turn starts with the prompt as the reader wrote it and under the id it was saved with, or as the model was told it when the conversation does not say.
    fn retry(&mut self) {
        let Some(at) = self
            .history
            .iter()
            .rposition(|message| matches!(message, Message::User(_)))
        else {
            return self.say(Event::Failed(
                "/retry has no prompt to answer again".to_owned(),
            ));
        };
        let mark = self.journal.as_ref().and_then(|journal| {
            journal
                .marks()
                .into_iter()
                .rev()
                .find(|(marked, _)| *marked == at)
                .map(|(_, mark)| mark)
        });
        let images = match self.history.get(at + 1) {
            Some(Message::Images(images)) => images.clone(),
            _ => Vec::new(),
        };
        let told = match (&mark, &self.history[at]) {
            (Some(mark), _) => mark.text.clone(),
            (None, Message::User(text)) => text.clone(),
            (None, _) => String::new(),
        };
        if let Some(Err(error)) = self.journal.as_ref().map(|journal| journal.cut(at)) {
            return self.say(Event::Failed(format!(
                "cannot save the conversation: {error}"
            )));
        }
        self.history.truncate(at);
        self.shared.warmer.stop();
        self.shared.checkpoint.clear();
        self.say_history();
        let (turn, text, files) = match mark {
            Some(mark) => (Some(mark.id), mark.text, mark.files),
            None => (None, told.clone(), Vec::new()),
        };
        self.prompt(told, images, turn, text, files);
    }

    /// Starts the conversation afresh: takes every message out of it, recording the turns of a saved conversation as lost for `reason`, stops warming its last request, starts the `SessionStart` hooks again and tells the client the turns left; the settings, the name, the goal and the notebook stay. Returns whether it did, telling the client why with [`Event::Failed`] when the saved conversation cannot be cut back.
    fn start_afresh(&mut self, reason: &str) -> bool {
        if let Some(Err(error)) = self
            .journal
            .as_ref()
            .map(|journal| journal.cut_losing(0, reason))
        {
            self.say(Event::Failed(format!(
                "cannot save the conversation: {error}"
            )));
            return false;
        }
        self.history.clear();
        self.shared.warmer.stop();
        self.shared.checkpoint.clear();
        let model = self.shared.model().map(|(_, model)| model);
        self.shared.lifecycle.begin(START_CLEAR, model);
        self.say_history();
        true
    }

    /// Asks the model of the `fast` slot, else the conversation's, `question` on the side, on a task of its own, showing it the conversation as it stood when the last turn ended, and tells the client the answer with [`Event::Aside`], or why there is none with [`Event::Failed`]; the conversation is left as it is, and a running turn goes on.
    fn aside(&self, question: String) {
        if question.is_empty() {
            return self.say(Event::Failed(
                "/btw needs a question: /btw <question>".to_owned(),
            ));
        }
        let Some((provider, id)) = self.shared.model() else {
            return self.say(Event::Failed("the conversation is not open".to_owned()));
        };
        let shared = self.shared.clone();
        let history = self.history.clone();
        tokio::spawn(async move {
            let picker = SessionPicker {
                shared: shared.clone(),
                provider,
                effort: shared.effort_now(),
            };
            let answered = match picker.slot(slots::FAST).await {
                Ok(Some((_, _, fast))) => {
                    omc_agent::answer_aside(fast.model.as_ref(), fast.effort, &history, &question)
                        .await
                        .map_err(|error| error.to_string())
                }
                Err(reason) => Err(format!("the fast model cannot be used: {reason}")),
                Ok(None) => match connect(&shared, provider, &id).await {
                    Ok(model) => {
                        let priced = Priced::of(model, shared.clone(), provider, &id, false);
                        omc_agent::answer_aside(&priced, shared.effort_now(), &history, &question)
                            .await
                            .map_err(|error| error.to_string())
                    }
                    Err(ended) => Err(reason_of(ended)),
                },
            };
            shared.say(match answered {
                Ok(answer) => Event::Aside { question, answer },
                Err(reason) => Event::Failed(format!("/btw could not be answered: {reason}")),
            });
        });
    }

    /// Hands the work over to a new conversation once the model wrote `prompt` for it: starts the conversation afresh, tells the client the prompt, and starts a turn with it under an id of its own; when that turn cannot start, the client is told the turn ended with why, and the prompts waiting are dropped.
    pub(super) fn hand_over(&mut self, prompt: String) {
        if !self.start_afresh(HANDOFF_REASON) {
            self.say(Event::Finished(Some(
                "the conversation could not be handed over".to_owned(),
            )));
            return self.drop_queue();
        }
        let id = handoff_id();
        self.say(Event::HandedOff {
            prompt: prompt.clone(),
            turn: id.clone(),
        });
        let mark = omc_agent::Mark {
            id,
            text: prompt.clone(),
            files: Vec::new(),
        };
        self.start(Job::Prompt(prompt, Vec::new()), Some(mark));
        if self.running.is_none() {
            self.say(Event::Finished(Some(
                "the turn of the handed-over conversation could not start".to_owned(),
            )));
            self.drop_queue();
        }
    }
}

#[cfg(test)]
#[path = "restart_tests.rs"]
mod tests;
