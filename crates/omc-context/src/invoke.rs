//! Loading a skill into a turn when the reader names it, and the messages plan mode gives.

use crate::Skill;
use crate::template::render;

/// What a turn starts with when the reader invoked a skill.
const INVOCATION: &str = include_str!("prompts/skill_invocation.md");

/// What a subagent is first told.
const ASSIGNMENT: &str = include_str!("prompts/subagent_user.md");

/// What the agent is told when its plan is approved.
const APPROVED: &str = include_str!("prompts/plan_approved.md");

/// What the agent is told when its plan is not approved.
const REJECTED: &str = include_str!("prompts/plan_rejected.md");

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

/// What the agent is told once the user approved `plan`.
pub fn plan_approved(plan: &str, has_todo: bool) -> String {
    render(
        APPROVED,
        &[
            ("plan", plan),
            ("has_todo", if has_todo { "1" } else { "" }),
        ],
    )
}

/// What the agent is told when the user did not approve its plan.
pub fn plan_rejected() -> String {
    REJECTED.trim().to_owned()
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

    /// The approval message carries the plan.
    #[test]
    fn approval_carries_the_plan() {
        let said = plan_approved("1. do it", true);
        assert!(said.contains("1. do it") && said.contains("`todo`"));
        assert!(plan_rejected().contains("exit_plan_mode"));
    }
}
