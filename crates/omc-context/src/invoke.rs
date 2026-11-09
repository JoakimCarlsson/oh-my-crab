//! Loading a skill into a turn when the reader names it, and the messages plan mode gives.

use crate::Skill;
use crate::template::render;

/// What a turn starts with when the reader invoked a skill.
const INVOCATION: &str = include_str!("prompts/skill_invocation.md");

/// What a subagent is first told.
const ASSIGNMENT: &str = include_str!("prompts/subagent_user.md");

/// What the agent is told when its plan is approved.
const APPROVED: &str = include_str!("prompts/plan_approved.md");

/// What the agent is told when its plan is approved to be carried out in a fresh context window.
const APPROVED_FRESH: &str = include_str!("prompts/plan_approved_fresh.md");

/// What a fresh context window opened to carry out an approved plan starts with.
const FRESH: &str = include_str!("prompts/plan_fresh.md");

/// What the agent is told when its plan is not approved yet.
const REJECTED: &str = include_str!("prompts/plan_rejected.md");

/// What the agent is told when its plan is rejected outright.
const ABANDONED: &str = include_str!("prompts/plan_abandoned.md");

/// What a subagent started while an approved plan is carried out is told of the plan.
const SUBAGENT_PLAN: &str = include_str!("prompts/subagent_plan.md");

/// The tag a fresh context window opened for an approved plan starts with, around the plan.
const SEEDED_OPEN: &str = "<approved-plan>\n";

/// The tag that closes the plan a fresh context window opened for an approved plan starts with.
const SEEDED_CLOSE: &str = "\n</approved-plan>";

/// The text of a turn in which the reader invoked `skill` and said `request` besides.
///
/// # Errors
///
/// Fails when the skill's file cannot be read.
pub fn skill_invocation(skill: &Skill, request: &str) -> std::io::Result<String> {
    let body = skill.body()?;
    Ok(render(
        INVOCATION,
        &[
            ("name", skill.name.as_str()),
            ("path", &skill.path.display().to_string()),
            ("base", &skill.base.display().to_string()),
            ("body", body.trim()),
            ("request", request.trim()),
        ],
    ))
}

/// The prompt a turn starts with: `text` itself, or with a skill loaded when `text` begins with `/name` or `$name` for one of `skills`.
pub fn expand_skill(text: &str, skills: &[Skill]) -> String {
    let trimmed = text.trim_start();
    let Some(rest) = trimmed.strip_prefix(['/', '$']) else {
        return text.to_owned();
    };
    let (name, request) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
    skills
        .iter()
        .find(|skill| skill.name == name)
        .and_then(|skill| skill_invocation(skill, request).ok())
        .unwrap_or_else(|| text.to_owned())
}

/// The first message of a subagent whose assignment is `assignment`.
pub fn subagent_assignment(assignment: &str) -> String {
    render(ASSIGNMENT, &[("assignment", assignment)])
}

/// A flag for a template: set when `on`.
fn flag(on: bool) -> &'static str {
    if on { "1" } else { "" }
}

/// `note` trimmed, or nothing when there is none or it is blank.
fn noted(note: Option<&str>) -> &str {
    note.map(str::trim).unwrap_or_default()
}

/// What the agent is told once the user approved `plan`, with `note`, what they wrote along with the approval, when there is one; `has_todo` says whether it has a `todo` tool to track the work with.
pub fn plan_approved(plan: &str, has_todo: bool, note: Option<&str>) -> String {
    render(
        APPROVED,
        &[
            ("plan", plan),
            ("has_todo", flag(has_todo)),
            ("note", noted(note)),
        ],
    )
}

/// What the agent is told once the user approved its plan to be carried out in a fresh context window, which opens after the batch of calls the approval is in.
pub fn plan_approved_fresh() -> String {
    APPROVED_FRESH.trim().to_owned()
}

/// The first message of a fresh context window opened to carry out `plan`, which the user approved with `note` when there is one; `has_todo` says whether the agent has a `todo` tool. It starts with the plan, which [`seeded_plan`] finds again.
pub fn plan_fresh(plan: &str, has_todo: bool, note: Option<&str>) -> String {
    render(
        FRESH,
        &[
            ("plan", plan.trim()),
            ("has_todo", flag(has_todo)),
            ("note", noted(note)),
        ],
    )
}

/// The plan `text` starts with when it is the first message of a fresh context window opened for an approved plan, as [`plan_fresh`] makes it.
pub fn seeded_plan(text: &str) -> Option<&str> {
    let rest = text.strip_prefix(SEEDED_OPEN)?;
    let end = rest.find(SEEDED_CLOSE)?;
    Some(&rest[..end])
}

/// What the agent is told when the user did not approve its plan yet, with `note`, what they said should change, when they said anything.
pub fn plan_rejected(note: Option<&str>) -> String {
    render(REJECTED, &[("note", noted(note))])
}

/// What the agent is told when the user rejected its plan outright, with `note`, what they wrote along with it, when they wrote anything.
pub fn plan_abandoned(note: Option<&str>) -> String {
    render(ABANDONED, &[("note", noted(note))])
}

/// What a subagent's system prompt carries of `plan`, the plan the user approved and the main agent carries out.
pub fn subagent_plan(plan: &str) -> String {
    render(SUBAGENT_PLAN, &[("plan", plan.trim())])
}

#[cfg(test)]
/// Tests for skill invocation.
mod tests {
    use super::*;
    use crate::testutil::{put, scratch};

    /// Builds one skill named `pdf` in a scratch folder.
    fn pdf(name: &str) -> Skill {
        let base = scratch(name);
        put(
            &base,
            "pdf/SKILL.md",
            "---\ndescription: d\n---\nRun pdftotext.",
        );
        Skill {
            name: "pdf".into(),
            description: "d".into(),
            path: base.join("pdf/SKILL.md"),
            base: base.join("pdf"),
            hidden: false,
            origin: crate::Origin::User,
        }
    }

    /// Both prefixes load the skill and keep the rest of the request.
    #[test]
    fn slash_and_dollar_load_the_skill() {
        let skills = vec![pdf("invoke-both")];
        for text in ["/pdf summarize a.pdf", "$pdf summarize a.pdf"] {
            let expanded = expand_skill(text, &skills);
            assert!(expanded.contains("Run pdftotext."), "{expanded}");
            assert!(expanded.ends_with("summarize a.pdf"));
        }
    }

    /// Text that names no skill is left alone.
    #[test]
    fn other_text_is_unchanged() {
        let skills = vec![pdf("invoke-other")];
        assert_eq!(expand_skill("/nope x", &skills), "/nope x");
        assert_eq!(expand_skill("plain", &skills), "plain");
    }

    /// The approval message carries the plan, and the note only when there is one.
    #[test]
    fn approval_carries_the_plan() {
        let said = plan_approved("1. do it", true, None);
        assert!(said.contains("1. do it") && said.contains("`todo`"));
        assert!(!said.contains("user-note"));
        let noted = plan_approved("1. do it", false, Some(" skip the docs "));
        assert!(noted.contains("<user-note>\nskip the docs\n</user-note>"));
        assert!(!noted.contains("`todo`"));
        assert!(plan_approved_fresh().contains("fresh context window"));
    }

    /// Not approving keeps the agent planning with what the user said; rejecting tells it to stop.
    #[test]
    fn refusals_pass_the_note_on() {
        assert!(plan_rejected(None).contains("exit_plan_mode"));
        assert!(!plan_rejected(Some("  ")).contains("user-note"));
        let refine = plan_rejected(Some("split step 2"));
        assert!(refine.contains("split step 2") && refine.contains("exit_plan_mode"));
        let stop = plan_abandoned(Some("not now"));
        assert!(stop.contains("not now") && stop.contains("End your turn now"));
        assert!(!plan_abandoned(None).contains("user-note"));
    }

    /// A fresh window starts with the plan, which is found again from its text and from nothing else.
    #[test]
    fn a_fresh_window_starts_with_the_plan() {
        let opening = plan_fresh("# Plan\n1. do it\n", true, Some("go"));
        assert_eq!(seeded_plan(&opening), Some("# Plan\n1. do it"));
        assert!(opening.contains("<user-note>\ngo\n</user-note>"));
        assert_eq!(seeded_plan("<plan>\nx\n</plan>"), None);
        assert_eq!(
            seeded_plan("text <approved-plan>\nx\n</approved-plan>"),
            None
        );
        assert!(subagent_plan("1. do it").contains("1. do it"));
    }
}
