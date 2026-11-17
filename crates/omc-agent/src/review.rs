//! Reviews by a second model: a plan judged before the user sees it, and a goal's completion judged before goal mode ends.
//!
//! Each review is one request without tools. The reviewer answers with a verdict line and a short list of problems; an answer that cannot be read passes, so a review never holds the work up by itself.

use omc_providers::{ChatModel, Effort};

use crate::AgentError;
use crate::aside::ask;

/// The system prompt of a plan review.
const PLAN_SYSTEM: &str = include_str!("prompts/review_plan_system.md");

/// The system prompt of a completion review.
const COMPLETION_SYSTEM: &str = include_str!("prompts/review_completion_system.md");

/// What the agent is told when its plan is sent back, with `{problems}` in place of the reviewer's list.
const PLAN_RETURNED: &str = include_str!("prompts/review_plan_returned.md");

/// What the agent is told when its completion is refused, with `{problems}` in place of the reviewer's list.
const GOAL_REFUSED: &str = include_str!("prompts/review_goal_refused.md");

/// The most problems kept from one review.
const MOST_PROBLEMS: usize = 5;

/// The most characters kept of one problem.
const PROBLEM_CHARS: usize = 400;

/// What a reviewer decided.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Verdict {
    /// Nothing concrete is wrong, or the answer could not be read.
    Pass,
    /// The work should change first, for these problems, never empty.
    Revise(Vec<String>),
}

/// Asks `model`, thinking as hard as `effort` says, to review `plan`, written for the user's `requests`, oldest first.
///
/// # Errors
///
/// Returns [`AgentError::Provider`] when the request fails, and [`AgentError::Empty`] when the reply has no text.
pub async fn review_plan(
    model: &dyn ChatModel,
    effort: Option<Effort>,
    requests: &[String],
    plan: &str,
) -> Result<Verdict, AgentError> {
    let asked = if requests.is_empty() {
        "(the conversation holds no request of the user's)".to_owned()
    } else {
        requests
            .iter()
            .map(|request| format!("<request>\n{}\n</request>", request.trim()))
            .collect::<Vec<_>>()
            .join("\n")
    };
    let prompt = format!(
        "<user-requests>\n{asked}\n</user-requests>\n\n<plan>\n{}\n</plan>",
        plan.trim()
    );
    ask(model, effort, PLAN_SYSTEM, prompt)
        .await
        .map(|answer| parse_verdict(&answer))
}

/// Asks `model`, thinking as hard as `effort` says, whether the goal `objective` is achieved, given the agent's `summary` of the work and its evidence.
///
/// # Errors
///
/// Returns [`AgentError::Provider`] when the request fails, and [`AgentError::Empty`] when the reply has no text.
pub async fn review_completion(
    model: &dyn ChatModel,
    effort: Option<Effort>,
    objective: &str,
    summary: &str,
) -> Result<Verdict, AgentError> {
    let prompt = format!(
        "<goal>\n{}\n</goal>\n\n<agent-summary>\n{}\n</agent-summary>",
        objective.trim(),
        summary.trim()
    );
    ask(model, effort, COMPLETION_SYSTEM, prompt)
        .await
        .map(|answer| parse_verdict(&answer))
}

/// What the agent is told when the reviewer sent its plan back for `problems`.
pub fn plan_returned(problems: &[String]) -> String {
    PLAN_RETURNED
        .trim()
        .replace("{problems}", &listed(problems))
}

/// What the agent is told when the reviewer refused its completion for `problems`.
pub fn completion_refused(problems: &[String]) -> String {
    GOAL_REFUSED.trim().replace("{problems}", &listed(problems))
}

/// `problems` as a markdown list, one per line.
fn listed(problems: &[String]) -> String {
    problems
        .iter()
        .map(|problem| format!("- {problem}"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Reads a reviewer's `answer`: the first line naming a verdict decides, `revise`, `reject` or `fail` sending the work back with the list items that follow it; anything else, an answer with no verdict line, or a negative verdict with no problem listed passes.
pub(crate) fn parse_verdict(answer: &str) -> Verdict {
    let mut lines = answer.lines().map(str::trim);
    let Some(verdict) = lines.find_map(|line| {
        let lower = clean(line).to_ascii_lowercase();
        let rest = lower.strip_prefix("verdict")?;
        Some(
            rest.trim_start_matches([':', '=', '-', ' ', '*', '`'])
                .to_owned(),
        )
    }) else {
        return Verdict::Pass;
    };
    if !["revise", "reject", "fail"]
        .iter()
        .any(|word| verdict.starts_with(word))
    {
        return Verdict::Pass;
    }
    let problems: Vec<String> = lines
        .filter_map(item)
        .take(MOST_PROBLEMS)
        .map(|problem| problem.chars().take(PROBLEM_CHARS).collect())
        .collect();
    if problems.is_empty() {
        Verdict::Pass
    } else {
        Verdict::Revise(problems)
    }
}

/// `line` without surrounding space and the markdown emphasis a model may wrap a verdict in.
fn clean(line: &str) -> &str {
    line.trim()
        .trim_matches(|c: char| c == '*' || c == '_' || c == '`' || c == '#')
        .trim()
}

/// The text of `line` when it is a list item, bulleted or numbered, and says something.
fn item(line: &str) -> Option<String> {
    let rest = line
        .strip_prefix("- ")
        .or_else(|| line.strip_prefix("* "))
        .or_else(|| line.strip_prefix("• "))
        .or_else(|| {
            let digits = line.find(|c: char| !c.is_ascii_digit())?;
            (digits > 0)
                .then(|| {
                    line[digits..]
                        .strip_prefix(". ")
                        .or_else(|| line[digits..].strip_prefix(") "))
                })
                .flatten()
        })?
        .trim();
    (!rest.is_empty()).then(|| rest.to_owned())
}

#[cfg(test)]
/// Tests for reviews by a second model.
mod tests {
    use super::*;
    use crate::testkit::{Router, say};

    /// A clean pass, a list of problems, numbered items, emphasis and unreadable answers all read as they should.
    #[test]
    fn verdicts_are_read_forgivingly() {
        assert_eq!(parse_verdict("VERDICT: pass"), Verdict::Pass);
        assert_eq!(
            parse_verdict("VERDICT: revise\n- no test step\n- step 3 contradicts step 1"),
            Verdict::Revise(vec![
                "no test step".to_owned(),
                "step 3 contradicts step 1".to_owned()
            ])
        );
        assert_eq!(
            parse_verdict("Some thoughts.\n**Verdict: Reject**\n1. tests were not run"),
            Verdict::Revise(vec!["tests were not run".to_owned()])
        );
        assert_eq!(parse_verdict("VERDICT: revise"), Verdict::Pass);
        assert_eq!(parse_verdict("looks fine to me\n- nit"), Verdict::Pass);
        assert_eq!(parse_verdict(""), Verdict::Pass);
        let many = format!("VERDICT: revise\n{}", "- x\n".repeat(9));
        let Verdict::Revise(kept) = parse_verdict(&many) else {
            panic!("a list of problems sends the plan back");
        };
        assert_eq!(kept.len(), MOST_PROBLEMS);
    }

    /// A plan review is one request without tools that carries the requests and the plan.
    #[tokio::test]
    async fn a_plan_review_sends_the_requests_and_the_plan() {
        let model = Router::new(|_| say("VERDICT: revise\n- nothing is verified"));
        let verdict = review_plan(
            model.as_ref(),
            None,
            &["add a flag".to_owned()],
            "# P\n1. x",
        )
        .await
        .unwrap();
        assert_eq!(
            verdict,
            Verdict::Revise(vec!["nothing is verified".to_owned()])
        );
        let seen = model.seen.lock().unwrap();
        assert_eq!(seen.len(), 1);
        assert!(seen[0].tools.is_empty());
        assert_eq!(seen[0].system, PLAN_SYSTEM);
        let asked = seen[0].last_user.clone().unwrap();
        assert!(asked.contains("<request>\nadd a flag\n</request>"));
        assert!(asked.contains("<plan>\n# P\n1. x\n</plan>"));
        assert!(plan_returned(&["a".to_owned()]).contains("<review>\n- a\n</review>"));
    }

    /// A completion review carries the goal and the summary, and a refusal lists the reasons.
    #[tokio::test]
    async fn a_completion_review_sends_the_goal_and_the_summary() {
        let model = Router::new(|_| say("VERDICT: pass"));
        let verdict = review_completion(model.as_ref(), None, "tests pass", "ran them")
            .await
            .unwrap();
        assert_eq!(verdict, Verdict::Pass);
        let seen = model.seen.lock().unwrap();
        assert_eq!(seen[0].system, COMPLETION_SYSTEM);
        let asked = seen[0].last_user.clone().unwrap();
        assert!(asked.contains("<goal>\ntests pass\n</goal>"));
        assert!(asked.contains("<agent-summary>\nran them\n</agent-summary>"));
        assert!(completion_refused(&["b".to_owned()]).contains("- b"));
    }
}
