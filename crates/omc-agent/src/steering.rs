//! Messages the reader sends while a turn runs, which the loop puts into the conversation between rounds.

use omc_providers::Message;

use crate::Scribe;

/// Where the loop takes the messages the reader sent while the turn runs.
///
/// The loop asks at each safe point: after a batch of tool calls has finished and any compaction or fresh context window that followed it, before the next model request. A message is the reader's from the moment it is taken, so the implementation reports it and saves what the client said about it then.
pub trait Steering: Send + Sync {
    /// Takes the message that has waited longest, which becomes message number `at` of the history, or nothing when none waits.
    fn next(&self, at: usize) -> Option<String>;
}

/// Appends every message waiting in `steering`, when there is one, to `history` as the reader's, each recorded with `scribe` as it is added; a journal that cannot be written to is reported on standard error.
pub(crate) fn inject(
    steering: Option<&dyn Steering>,
    history: &mut Vec<Message>,
    scribe: Option<&Scribe>,
) {
    let Some(steering) = steering else {
        return;
    };
    while let Some(text) = steering.next(history.len()) {
        let message = Message::User(text);
        if let Some(Err(error)) = scribe.map(|scribe| scribe.append(&message)) {
            eprintln!("history not saved: {error}");
        }
        history.push(message);
    }
}
