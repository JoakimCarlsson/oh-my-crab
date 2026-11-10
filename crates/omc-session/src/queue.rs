//! The prompts the client sent while a turn was running, waiting to be put into that turn or to start the next.

use std::collections::VecDeque;
use std::sync::Mutex;

use omc_wire::{Event, QueueState, Queued};

use crate::shared::Shared;

/// A prompt that waits for the running turn.
pub(crate) struct Waiting {
    /// What the model is told: the context, the reader's words, the files and any skill they invoke.
    pub(crate) told: String,
    /// The images the reader attached, which the model is shown with `told`.
    pub(crate) images: Vec<omc_providers::Image>,
    /// The prompt as the client is shown it.
    pub(crate) queued: Queued,
}

impl Waiting {
    /// What the client said about the prompt, to save with the turn it starts, when the client gave the turn an id.
    pub(crate) fn mark(&self) -> Option<omc_agent::Mark> {
        self.queued.turn.as_ref().map(|id| omc_agent::Mark {
            id: id.clone(),
            text: self.queued.text.clone(),
            files: self.queued.files.clone(),
        })
    }

    /// The event that says the prompt is now in `state`.
    pub(crate) fn event(&self, state: QueueState) -> Event {
        Event::Queued(Queued {
            state,
            ..self.queued.clone()
        })
    }
}

/// The prompts waiting, oldest first, and the number the next one gets.
#[derive(Default)]
struct Line {
    /// The prompts waiting, oldest first.
    waiting: VecDeque<Waiting>,
    /// How many prompts were ever queued, which numbers the next one.
    tickets: u64,
}

/// The prompts the client sent while a turn was running, shared by the conversation, which adds them, and the turn, which takes them.
#[derive(Default)]
pub(crate) struct Queue {
    /// The prompts and their numbering.
    line: Mutex<Line>,
}

impl Queue {
    /// Runs `with` on the line, whatever a panicking holder left.
    fn line<T>(&self, with: impl FnOnce(&mut Line) -> T) -> T {
        with(&mut self.line.lock().unwrap_or_else(|held| held.into_inner()))
    }

    /// Queues the prompt that tells the model `told` and shows it `images`, which the client sent as `text` with the files named `files` under the turn id `turn`, and returns the event that says it waits.
    pub(crate) fn push(
        &self,
        told: String,
        images: Vec<omc_providers::Image>,
        turn: Option<String>,
        text: String,
        files: Vec<String>,
    ) -> Event {
        self.line(|line| {
            line.tickets += 1;
            let waiting = Waiting {
                told,
                images,
                queued: Queued {
                    ticket: line.tickets,
                    turn,
                    text,
                    files,
                    state: QueueState::Waiting,
                },
            };
            let event = waiting.event(QueueState::Waiting);
            line.waiting.push_back(waiting);
            event
        })
    }

    /// Takes the prompt that has waited longest, when one waits.
    pub(crate) fn pop(&self) -> Option<Waiting> {
        self.line(|line| line.waiting.pop_front())
    }

    /// Takes every prompt waiting, oldest first.
    pub(crate) fn drain(&self) -> Vec<Waiting> {
        self.line(|line| line.waiting.drain(..).collect())
    }
}

impl omc_agent::Steering for Shared {
    /// Takes the prompt that has waited longest for the running turn, with its images, saves what the client said about it as the turn that starts at message `at`, and tells the client it was put in.
    fn next(&self, at: usize) -> Option<(String, Vec<omc_providers::Image>)> {
        let waiting = self.queue.pop()?;
        if let (Some(journal), Some(mark)) = (self.journal(), waiting.mark())
            && let Err(error) = journal.mark(at, &mark)
        {
            eprintln!("history not saved: {error}");
        }
        self.say(waiting.event(QueueState::Steered));
        Some((waiting.told, waiting.images))
    }
}
