//! Tests for reviews of plans and goal completions by a second model.

use std::sync::Mutex;
use std::sync::atomic::AtomicUsize;

use omc_providers::{ProviderError, Reply, Request, Sink};
use omc_tools::{ExitPlanMode, Tool, ToolError};
use omc_wire::GoalState;
use serde_json::json;

use super::*;
use crate::goal::GoalTool;

/// A reviewer model that gives the same answer to every request, or fails each one, and counts them.
struct Judge {
    /// The answer, or nothing to fail.
    answer: Option<&'static str>,
    /// How many requests it was sent.
    asked: AtomicUsize,
}

#[async_trait]
impl ChatModel for Judge {
    /// Counts the request and answers it, or fails it when there is no answer.
    async fn complete(
        &self,
        request: Request<'_>,
        _: &mut dyn Sink,
    ) -> Result<Reply, ProviderError> {
        assert!(request.tools.is_empty());
        self.asked.fetch_add(1, Ordering::Relaxed);
        match self.answer {
            Some(text) => Ok(Reply {
                text: text.to_owned(),
                ..Reply::default()
            }),
            None => Err(ProviderError::Unfinished),
        }
    }
}

/// A reader who approves every plan and keeps the plans put to them.
#[derive(Default)]
struct Approver {
    /// The plans put to the reader.
    plans: Mutex<Vec<String>>,
}

#[async_trait]
impl Host for Approver {
    /// Ignores the plan steps.
    fn plan(&self, _steps: Vec<PlanStep>) {}

    /// Answers no form.
    async fn inquire(&self, _questions: Vec<Question>) -> Option<Vec<omc_wire::Reply>> {
        None
    }

    /// Ignores the exhibit.
    fn show(&self, _exhibit: Exhibit) {}

    /// Names nothing.
    fn name(&self, _title: &str) -> bool {
        false
    }

    /// Keeps the plan and approves it.
    async fn approve_plan(&self, plan: &str) -> PlanVerdict {
        self.plans.lock().unwrap().push(plan.to_owned());
        PlanVerdict::Approved {
            fresh: false,
            note: None,
        }
    }
}

/// An open conversation with reviews turned on as `reviews` says, and a judge answering `answer`.
fn judged(answer: Option<&'static str>, reviews: bool) -> (Arc<Shared>, Arc<Judge>, Arc<Reviewer>) {
    let (out, _) = tokio::sync::mpsc::unbounded_channel();
    let shared = Shared::new(out);
    shared.reviews.set_enabled(reviews);
    let judge = Arc::new(Judge {
        answer,
        asked: AtomicUsize::new(0),
    });
    let reviewer = Arc::new(Reviewer {
        shared: shared.clone(),
        picker: None,
        fallback: judge.clone(),
        effort: None,
        requests: vec!["add a --dry-run flag".to_owned()],
    });
    (shared, judge, reviewer)
}

/// The `exit_plan_mode` tool putting plans to an approving reader through `reviewer`.
fn submitter(reviewer: Arc<Reviewer>) -> (ExitPlanMode, Arc<Approver>) {
    let reader = Arc::new(Approver::default());
    let host = Arc::new(ReviewingHost {
        inner: reader.clone(),
        reviewer,
    });
    (ExitPlanMode::new(host, false), reader)
}

/// A plan the reviewer sends back returns its problems to the agent without reaching the reader; the next submission reaches the reader unreviewed, so nothing loops.
#[tokio::test]
async fn a_plan_sent_back_once_then_goes_to_the_reader() {
    let (_shared, judge, reviewer) =
        judged(Some("VERDICT: revise\n- no step runs the tests"), true);
    let (tool, reader) = submitter(reviewer);
    let Err(ToolError::Failed(returned)) = tool.call(json!({"plan": "# P\n1. x"})).await else {
        panic!("a plan sent back is an error");
    };
    assert!(returned.contains("no step runs the tests"));
    assert!(reader.plans.lock().unwrap().is_empty());
    tool.call(json!({"plan": "# P\n1. x\n2. test"}))
        .await
        .unwrap();
    assert_eq!(reader.plans.lock().unwrap().len(), 1);
    assert_eq!(judge.asked.load(Ordering::Relaxed), 1);
}

/// A plan that passes goes straight to the reader after one review.
#[tokio::test]
async fn a_plan_that_passes_goes_to_the_reader() {
    let (_shared, judge, reviewer) = judged(Some("VERDICT: pass"), true);
    let (tool, reader) = submitter(reviewer);
    tool.call(json!({"plan": "# P\n1. x"})).await.unwrap();
    assert_eq!(reader.plans.lock().unwrap().len(), 1);
    assert_eq!(judge.asked.load(Ordering::Relaxed), 1);
}

/// A review that fails, or one that cannot be read, is skipped and the plan goes to the reader.
#[tokio::test]
async fn a_failed_review_is_skipped() {
    for answer in [None, Some("I would not do it this way.")] {
        let (_shared, judge, reviewer) = judged(answer, true);
        let (tool, reader) = submitter(reviewer);
        tool.call(json!({"plan": "# P\n1. x"})).await.unwrap();
        assert_eq!(reader.plans.lock().unwrap().len(), 1);
        assert_eq!(judge.asked.load(Ordering::Relaxed), 1);
    }
}

/// With the setting off, no review is asked for, of a plan or of a completion.
#[tokio::test]
async fn reviews_off_ask_nothing() {
    let (shared, judge, reviewer) = judged(Some("VERDICT: revise\n- x"), false);
    let (tool, reader) = submitter(reviewer.clone());
    tool.call(json!({"plan": "# P\n1. x"})).await.unwrap();
    assert_eq!(reader.plans.lock().unwrap().len(), 1);
    shared.goals.set("ship it", None);
    let goal = GoalTool {
        shared: shared.clone(),
        reviewer: Some(reviewer),
    };
    goal.call(json!({"op": "complete", "summary": "done"}))
        .await
        .unwrap();
    assert_eq!(judge.asked.load(Ordering::Relaxed), 0);
}

/// A completion the reviewer refuses keeps the goal open and tells the agent why; the next completion is accepted unreviewed, its note keeping the objections.
#[tokio::test]
async fn a_refused_completion_is_accepted_the_second_time() {
    let (shared, judge, reviewer) =
        judged(Some("VERDICT: reject\n- the tests were never run"), true);
    shared.goals.set("make every test pass", None);
    let goal = GoalTool {
        shared: shared.clone(),
        reviewer: Some(reviewer),
    };
    let Err(ToolError::Failed(refused)) = goal
        .call(json!({"op": "complete", "summary": "fixed it"}))
        .await
    else {
        panic!("a refused completion is an error");
    };
    assert!(refused.contains("the tests were never run"));
    assert_eq!(shared.goals.current().unwrap().state, GoalState::Active);
    goal.call(json!({"op": "complete", "summary": "ran the tests: all pass"}))
        .await
        .unwrap();
    let done = shared.goals.current().unwrap();
    assert_eq!(done.state, GoalState::Complete);
    let note = done.note.unwrap();
    assert!(note.starts_with("ran the tests: all pass"));
    assert!(note.contains("the tests were never run"));
    assert_eq!(judge.asked.load(Ordering::Relaxed), 1);
}

/// The requests a plan is judged against drop the notices before them and keep the latest few.
#[test]
fn requests_drop_notices() {
    let history = vec![
        Message::User("first".to_owned()),
        Message::User("second".to_owned()),
        Message::User("<goal>\nkeep going\n</goal>\n\nthird".to_owned()),
    ];
    assert_eq!(
        requests(&history, Some("fourth")),
        vec!["second".to_owned(), "third".to_owned(), "fourth".to_owned()]
    );
}
