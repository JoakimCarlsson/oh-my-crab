//! Prompts the client sent while a turn was running, and what became of them.

/// What became of a prompt that was sent while a turn was running.
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum QueueState {
    /// It waits for the next safe point of the running turn.
    Waiting,
    /// It was put into the running turn, after a batch of tool calls, so the model reads it before its next request; from here on the turn answers it.
    Steered,
    /// The running turn ended before it could be put in, so it started a turn of its own.
    Started,
    /// It was never sent to the model, because the running turn was stopped, failed or needs signing in again; the client may offer it to the reader again.
    Dropped,
}

/// A prompt the client sent while a turn was running.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Queued {
    /// The number the harness gave the prompt, the same in every event about it.
    pub ticket: u64,
    /// The id the client gave the turn, when it gave one; the prompt is saved under it once it is sent to the model.
    #[serde(default)]
    pub turn: Option<String>,
    /// What the reader wrote.
    pub text: String,
    /// The names of the files the reader attached.
    #[serde(default)]
    pub files: Vec<String>,
    /// What became of it.
    pub state: QueueState,
}

#[cfg(test)]
/// Tests for the queue's events.
mod tests {
    use crate::{Event, QueueState, Queued, decode, encode};

    /// A queue event survives the wire, and one without a turn id or files still reads.
    #[test]
    fn a_queue_event_reads_back() {
        let event = Event::Queued(Queued {
            ticket: 3,
            turn: Some("t".into()),
            text: "and this".into(),
            files: vec!["a.txt".into()],
            state: QueueState::Steered,
        });
        assert_eq!(decode::<Event>(&encode(&event).unwrap()).unwrap(), event);
        let bare: Event =
            decode(r#"{"Queued":{"ticket":1,"text":"x","state":"Dropped"}}"#).unwrap();
        assert_eq!(
            bare,
            Event::Queued(Queued {
                ticket: 1,
                turn: None,
                text: "x".into(),
                files: Vec::new(),
                state: QueueState::Dropped,
            })
        );
    }
}
