//! Reviews by the model of the `review` slot: a plan is judged before the reader sees it, and a goal's completion before goal mode ends.
//!
//! A review is one request without tools, asked at most once per plan put forward and once per goal. When the setting is off, the slot's model cannot be used, or the request fails or takes too long, the review is skipped and the work goes on as it would without one: a review never fails a turn.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use omc_agent::Verdict;
use omc_providers::{ChatModel, Effort, Message};
use omc_tools::{Host, PlanVerdict};
use omc_wire::{Exhibit, PlanStep, Question};

use crate::picker::SessionPicker;
use crate::shared::Shared;
use crate::slots;

/// The longest a review is waited for before it is skipped.
const REVIEW_WAIT: Duration = Duration::from_secs(180);

/// How many of the reader's latest requests a plan review is shown.
const REQUESTS_KEPT: usize = 3;

/// The most characters kept of one request.
const REQUEST_CHARS: usize = 6000;

/// Whether a conversation's reviews are on, and where its plan stands in its round of review.
pub(crate) struct Reviews {
    /// Whether reviews are off.
    off: AtomicBool,
    /// Whether the last plan put forward was sent back by the reviewer, so that the next one goes to the reader unreviewed.
    plan_returned: AtomicBool,
}

impl Default for Reviews {
    /// Reviews on, except in tests, where a turn's scripted model would otherwise be asked for them too; a test that wants them turns them on.
    fn default() -> Self {
        Self {
            off: AtomicBool::new(cfg!(test)),
            plan_returned: AtomicBool::new(false),
        }
    }
}

impl Reviews {
    /// Whether reviews are on.
    pub(crate) fn enabled(&self) -> bool {
        !self.off.load(Ordering::Relaxed)
    }

    /// Turns reviews on or off for the plans and completions from now on.
    pub(crate) fn set_enabled(&self, enabled: bool) {
        self.off.store(!enabled, Ordering::Relaxed);
    }

    /// Whether the last plan was sent back, forgetting it, so that only the submission right after a review skips one.
    fn take_returned(&self) -> bool {
        self.plan_returned.swap(false, Ordering::Relaxed)
    }

    /// Notes that the reviewer sent the plan back.
    fn returned(&self) {
        self.plan_returned.store(true, Ordering::Relaxed);
    }
}

/// What a turn's reviews ask, and of what.
pub(crate) struct Reviewer {
    /// The conversation, whose setting says whether reviews are on.
    pub(crate) shared: Arc<Shared>,
    /// Finds the `review` slot's model; without it, `fallback` reviews.
    pub(crate) picker: Option<Arc<SessionPicker>>,
    /// The model that reviews when the slot sets none: the turn's own, its requests counted like those of any helper request.
    pub(crate) fallback: Arc<dyn ChatModel>,
    /// How hard `fallback` thinks.
    pub(crate) effort: Option<Effort>,
    /// The reader's latest requests, oldest first, which a plan is judged against.
    pub(crate) requests: Vec<String>,
}

impl Reviewer {
    /// The model to review with and how hard it thinks: the `review` slot's when it sets one, else `fallback`; nothing when the slot's model cannot be used.
    async fn model(&self) -> Option<(Arc<dyn ChatModel>, Option<Effort>)> {
        let Some(picker) = &self.picker else {
            return Some((self.fallback.clone(), self.effort));
        };
        match picker.slot(slots::REVIEW).await {
            Ok(Some((_, _, picked))) => Some((picked.model, picked.effort)),
            Ok(None) => Some((self.fallback.clone(), self.effort)),
            Err(_) => None,
        }
    }

    /// Has `plan` reviewed, unless the plan before it was sent back; what the agent is told to fix when the reviewer sends this one back, else nothing.
    pub(crate) async fn plan(&self, plan: &str) -> Option<String> {
        if self.shared.reviews.take_returned() || !self.shared.reviews.enabled() {
            return None;
        }
        let (model, effort) = self.model().await?;
        let review = omc_agent::review_plan(model.as_ref(), effort, &self.requests, plan);
        match tokio::time::timeout(REVIEW_WAIT, review).await {
            Ok(Ok(Verdict::Revise(problems))) => {
                self.shared.reviews.returned();
                Some(omc_agent::plan_returned(&problems))
            }
            _ => None,
        }
    }

    /// Has the completion of the goal `objective`, with the agent's `summary`, reviewed; the reviewer's reasons when it refuses it, else nothing.
    pub(crate) async fn completion(&self, objective: &str, summary: &str) -> Option<Vec<String>> {
        if !self.shared.reviews.enabled() {
            return None;
        }
        let (model, effort) = self.model().await?;
        let review = omc_agent::review_completion(model.as_ref(), effort, objective, summary);
        match tokio::time::timeout(REVIEW_WAIT, review).await {
            Ok(Ok(Verdict::Revise(problems))) => Some(problems),
            _ => None,
        }
    }
}

/// The reader's latest requests in `history` and `prompt`, the prompt of the turn starting when it has one, oldest first, without the notices crab put before them.
pub(crate) fn requests(history: &[Message], prompt: Option<&str>) -> Vec<String> {
    let said = history.iter().filter_map(|message| match message {
        Message::User(text) => Some(text.as_str()),
        _ => None,
    });
    let mut kept: Vec<String> = said
        .chain(prompt)
        .map(bare)
        .filter(|text| !text.is_empty())
        .map(|text| text.chars().take(REQUEST_CHARS).collect())
        .collect();
    let from = kept.len().saturating_sub(REQUESTS_KEPT);
    kept.drain(..from);
    kept
}

/// `text` without the tagged notices that open it, such as `<goal>…</goal>`, trimmed.
fn bare(text: &str) -> &str {
    let mut rest = text.trim();
    while rest.starts_with('<') {
        let Some(name) = rest[1..]
            .split(|c: char| c == '>' || c.is_whitespace())
            .next()
            .filter(|name| !name.is_empty() && !name.starts_with('/'))
        else {
            break;
        };
        let close = format!("</{name}>");
        let Some(at) = rest.find(&close) else {
            break;
        };
        rest = rest[at + close.len()..].trim_start();
    }
    rest
}

/// The reader, with each plan reviewed before it is put to them.
pub(crate) struct ReviewingHost {
    /// The reader.
    pub(crate) inner: Arc<dyn Host>,
    /// What reviews the plan.
    pub(crate) reviewer: Arc<Reviewer>,
}

#[async_trait]
impl Host for ReviewingHost {
    /// Shows the reader the plan steps.
    fn plan(&self, steps: Vec<PlanStep>) {
        self.inner.plan(steps);
    }

    /// Puts the questions to the reader.
    async fn inquire(&self, questions: Vec<Question>) -> Option<Vec<omc_wire::Reply>> {
        self.inner.inquire(questions).await
    }

    /// Shows the reader the exhibit.
    fn show(&self, exhibit: Exhibit) {
        self.inner.show(exhibit);
    }

    /// Names the conversation when it has no name.
    fn name(&self, title: &str) -> bool {
        self.inner.name(title)
    }

    /// Puts the plan to the reader.
    async fn approve_plan(&self, plan: &str) -> PlanVerdict {
        self.inner.approve_plan(plan).await
    }

    /// Has the plan reviewed by the `review` slot's model, at most once in a row.
    async fn review_plan(&self, plan: &str) -> Option<String> {
        self.reviewer.plan(plan).await
    }
}

#[cfg(test)]
#[path = "review_tests.rs"]
mod tests;
