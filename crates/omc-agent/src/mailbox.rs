//! The mailbox of a long-lived subagent: the messages other members of its crew sent it, which reach it at its next step boundary while it works, or wake it when it is idle.

use std::collections::VecDeque;
use std::sync::Mutex;

use tokio::sync::Notify;

use crate::Steering;

/// The messages waiting for one long-lived subagent, oldest first, and the bell that wakes its worker when one comes.
#[derive(Default)]
pub(crate) struct Mailbox {
    /// The notices waiting, oldest first, each already marked with its sender.
    notes: Mutex<VecDeque<String>>,
    /// Rung each time a notice is posted.
    bell: Notify,
}

impl Mailbox {
    /// The notices, locked; a poisoned lock is taken over, since every change to it is whole.
    fn lock(&self) -> std::sync::MutexGuard<'_, VecDeque<String>> {
        self.notes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Puts `note` at the back of the mailbox and rings the bell.
    pub(crate) fn post(&self, note: String) {
        self.lock().push_back(note);
        self.bell.notify_one();
    }

    /// Takes every notice waiting, oldest first; empty when none waits.
    pub(crate) fn take_all(&self) -> Vec<String> {
        self.lock().drain(..).collect()
    }

    /// Waits until the bell is rung; a ring that came while nobody waited is heard at once, even when its notice was taken since.
    pub(crate) async fn rung(&self) {
        self.bell.notified().await;
    }
}

impl Steering for Mailbox {
    /// Takes the notice that has waited longest, without images.
    fn next(&self, _at: usize) -> Option<(String, Vec<omc_providers::Image>)> {
        self.lock().pop_front().map(|note| (note, Vec::new()))
    }
}

/// The notice a member called `from` sends with `text`, as its recipient reads it.
pub(crate) fn notice(from: &str, text: &str) -> String {
    format!(
        "<crew-message from=\"{from}\">\n{}\n</crew-message>\nThis is a message from {from}, another member of your crew, not from the user. Answer it with crew_message when it asks for an answer.",
        text.trim()
    )
}
