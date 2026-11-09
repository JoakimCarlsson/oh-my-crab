//! Keeping the plan the agent works from in its context: the plan it last submitted with `exit_plan_mode`, or the one a fresh context window opened for an approved plan starts with, is never left out of the context to make room, and is restated after a compaction hides it.
//!
//! The plan is read from the history, so it survives a reopened conversation and follows the branch the conversation is on.

use std::collections::HashMap;

use omc_context::{render, seeded_plan};
use omc_providers::Message;

/// The tool the agent submits its plan with.
pub(crate) const EXIT_PLAN: &str = "exit_plan_mode";

/// Added to the summary of a compaction that hid the plan.
const RESTATED: &str = include_str!("prompts/plan_restated.md");

/// A plan the agent submitted, as the history holds it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Submitted {
    /// The position of the message that holds it: the reply that called `exit_plan_mode`, or the first message of the fresh context window opened for it.
    pub(crate) at: usize,
    /// The plan, as markdown.
    pub(crate) plan: String,
    /// Whether the reader approved it.
    pub(crate) approved: bool,
}

/// The latest plan in `history`: the last plan submitted with `exit_plan_mode`, approved when the call succeeded, or the plan the first message of a fresh context window opened for an approved plan starts with; nothing when there is neither.
pub(crate) fn latest(history: &[Message]) -> Option<Submitted> {
    let outcomes: HashMap<&str, bool> = history
        .iter()
        .filter_map(|message| match message {
            Message::ToolResults(results) => Some(results),
            _ => None,
        })
        .flatten()
        .filter(|result| result.name == EXIT_PLAN)
        .map(|result| (result.call_id.as_str(), !result.is_error))
        .collect();
    history
        .iter()
        .enumerate()
        .rev()
        .find_map(|(at, message)| match message {
            Message::Assistant(reply) => reply
                .tool_calls
                .iter()
                .rev()
                .filter(|call| call.name == EXIT_PLAN)
                .find_map(|call| {
                    let plan = call.arguments["plan"].as_str()?.trim();
                    (!plan.is_empty()).then(|| Submitted {
                        at,
                        plan: plan.to_owned(),
                        approved: outcomes.get(call.id.as_str()) == Some(&true),
                    })
                }),
            Message::User(text) => seeded_plan(text).map(|plan| Submitted {
                at,
                plan: plan.to_owned(),
                approved: true,
            }),
            _ => None,
        })
}

/// The plan the agent carries out: the latest plan of `history` when the reader approved it; nothing when there is none, or the latest one was not approved, as while a new plan is being made.
pub fn approved_plan(history: &[Message]) -> Option<String> {
    latest(history)
        .filter(|submitted| submitted.approved)
        .map(|submitted| submitted.plan)
}

/// Whether `message` is the first message of a fresh context window opened for an approved plan, whose text is never cut to make room.
pub(crate) fn seeds_plan(message: &Message) -> bool {
    matches!(message, Message::User(text) if seeded_plan(text).is_some())
}

/// The text that restates the latest plan to the model after a compaction of `history` whose first kept message is at `first_kept`: only when that plan was submitted before `first_kept`, where the model no longer sees it. A plan submitted later is in the part the model sees.
pub(crate) fn restated(history: &[Message], first_kept: usize) -> Option<String> {
    let submitted = latest(history)?;
    (submitted.at < first_kept).then(|| {
        render(
            RESTATED,
            &[
                ("plan", submitted.plan.as_str()),
                ("approved", if submitted.approved { "1" } else { "" }),
            ],
        )
    })
}

#[cfg(test)]
/// Tests for finding and restating the plan.
mod tests {
    use omc_providers::{Reply, ToolCall, ToolResult};
    use serde_json::json;

    use super::*;

    /// A reply that submits `plan` under the id `id`.
    fn submit(id: &str, plan: &str) -> Message {
        Message::Assistant(Reply {
            tool_calls: vec![ToolCall {
                id: id.to_owned(),
                name: EXIT_PLAN.to_owned(),
                arguments: json!({ "plan": plan }),
            }],
            ..Reply::default()
        })
    }

    /// The results of the submission `id`, approved or not.
    fn verdict(id: &str, approved: bool) -> Message {
        Message::ToolResults(vec![ToolResult {
            call_id: id.to_owned(),
            name: EXIT_PLAN.to_owned(),
            text: String::new(),
            is_error: !approved,
            images: Vec::new(),
        }])
    }

    /// The latest submission wins, approved only when its call succeeded; a call still waiting for its verdict counts as not approved.
    #[test]
    fn the_latest_submission_is_the_plan() {
        let mut history = vec![
            Message::User("plan it".into()),
            submit("a", "# One"),
            verdict("a", false),
            submit("b", "# Two"),
            verdict("b", true),
        ];
        let latest = latest(&history).unwrap();
        assert_eq!((latest.at, latest.plan.as_str()), (3, "# Two"));
        assert!(latest.approved);
        assert_eq!(approved_plan(&history).as_deref(), Some("# Two"));
        history.push(submit("c", "# Three"));
        assert_eq!(approved_plan(&history), None);
        assert_eq!(super::latest(&history).unwrap().plan, "# Three");
        assert_eq!(approved_plan(&[Message::User("hi".into())]), None);
    }

    /// A fresh window opened for an approved plan carries it, and is never cut.
    #[test]
    fn a_fresh_window_carries_the_plan() {
        let opening = Message::User(omc_context::plan_fresh("# Plan\n1. x", false, None));
        assert!(seeds_plan(&opening));
        assert!(!seeds_plan(&Message::User("<plan>".into())));
        let history = vec![opening, Message::User("go on".into())];
        assert_eq!(approved_plan(&history).as_deref(), Some("# Plan\n1. x"));
    }

    /// The plan is restated only when the compaction hid it, saying whether it was approved.
    #[test]
    fn a_hidden_plan_is_restated() {
        let history = vec![
            Message::User("plan it".into()),
            submit("a", "# Plan\n1. x"),
            verdict("a", true),
            Message::User("more".into()),
        ];
        let said = restated(&history, 3).unwrap();
        assert!(said.contains("# Plan\n1. x") && said.contains("which you are carrying out"));
        assert_eq!(restated(&history, 1), None);
        let draft = vec![Message::User("p".into()), submit("a", "# Draft")];
        let said = restated(&draft, 2).unwrap();
        assert!(said.contains("# Draft") && said.contains("has not approved"));
    }
}
