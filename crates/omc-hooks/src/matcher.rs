//! Which tools, or kinds of subagent, a hook runs for.
//!
//! A matcher is read the way Claude Code reads one, with globs added: empty or `*` matches everything; letters, digits, `_`, `-`, spaces, `,` and `|` alone are a list of exact names split at `|` or `,`; a pattern whose other characters are only `*` and `?` is a glob over the whole name, such as `mcp__*`; anything else is a regular expression that may match anywhere in the name, as in `Edit.*` or `^mcp__github__`.

use globset::{Glob, GlobMatcher};
use regex::Regex;

/// A parsed matcher.
#[derive(Clone, Debug)]
pub enum Matcher {
    /// Everything.
    All,
    /// Any of these exact names.
    Names(Vec<String>),
    /// Names the glob matches whole.
    Glob(GlobMatcher),
    /// Names the regular expression matches somewhere in.
    Pattern(Regex),
}

impl Matcher {
    /// The matcher written as `text`.
    ///
    /// # Errors
    ///
    /// Returns why, when `text` is neither a list of names, a glob nor a regular expression.
    pub fn parse(text: &str) -> Result<Self, String> {
        let text = text.trim();
        if text.is_empty() || text == "*" {
            return Ok(Self::All);
        }
        let plain = |c: char| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | ' ' | ',' | '|');
        if text.chars().all(plain) {
            let names = text
                .split(['|', ','])
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .map(str::to_owned)
                .collect();
            return Ok(Self::Names(names));
        }
        if text.chars().all(|c| plain(c) || matches!(c, '*' | '?')) && !text.contains(['|', ',']) {
            return Glob::new(text)
                .map(|glob| Self::Glob(glob.compile_matcher()))
                .map_err(|error| format!("bad glob {text:?}: {error}"));
        }
        Regex::new(text)
            .map(Self::Pattern)
            .map_err(|error| format!("bad regular expression {text:?}: {error}"))
    }

    /// Whether any of `names`, the names one tool or subagent kind goes by, is matched.
    pub fn matches(&self, names: &[&str]) -> bool {
        match self {
            Self::All => true,
            Self::Names(listed) => names
                .iter()
                .any(|name| listed.iter().any(|listed| listed == name)),
            Self::Glob(glob) => names.iter().any(|name| glob.is_match(name)),
            Self::Pattern(pattern) => names.iter().any(|name| pattern.is_match(name)),
        }
    }
}

/// The names Claude Code gives crab's tools that do the same job, the first being the one hooks from Claude Code's files are told.
pub fn claude_names(tool: &str) -> &'static [&'static str] {
    match tool {
        "bash" => &["Bash"],
        "read" => &["Read"],
        "write" => &["Write"],
        "edit" => &["Edit", "MultiEdit"],
        "glob" => &["Glob"],
        "grep" => &["Grep"],
        "task" => &["Task", "Agent"],
        "todo" => &["TodoWrite"],
        "ask" => &["AskUserQuestion"],
        "exit_plan_mode" => &["ExitPlanMode"],
        "skill" => &["Skill"],
        _ => &[],
    }
}

#[cfg(test)]
/// Tests for matchers.
mod tests {
    use super::*;

    /// Whether `matcher` matches the tool `tool`, by crab's name or Claude Code's.
    fn hits(matcher: &str, tool: &str) -> bool {
        let mut names = vec![tool];
        names.extend(claude_names(tool));
        Matcher::parse(matcher).unwrap().matches(&names)
    }

    /// Empty and `*` match everything; plain names are exact, in lists split by `|` or `,`, and Claude Code's names match crab's tools.
    #[test]
    fn names_are_exact() {
        assert!(hits("", "bash") && hits("*", "mcp__x__y"));
        assert!(hits("bash", "bash") && hits("Bash", "bash"));
        assert!(!hits("bas", "bash") && !hits("BASH", "bash"));
        assert!(hits("Edit|Write", "write") && hits("Edit, Write", "edit"));
        assert!(hits("MultiEdit", "edit"));
        assert!(!hits("Edit|Write", "read"));
    }

    /// A pattern of names with `*` or `?` is a glob over the whole name.
    #[test]
    fn globs_match_whole_names() {
        assert!(hits("mcp__*", "mcp__github__issue"));
        assert!(!hits("mcp__*", "bash"));
        assert!(hits("ba?h", "bash"));
        assert!(hits("*_grep", "ast_grep") && !hits("*_grep", "grep"));
    }

    /// Anything else is a regular expression that may match anywhere, unless anchored.
    #[test]
    fn patterns_match_anywhere() {
        assert!(hits("Edit.*", "edit"));
        assert!(hits("^mcp__github__.*", "mcp__github__issue"));
        assert!(!hits("^mcp__github__", "mcp__gitlab__issue"));
        assert!(hits("^(read|write)$", "write") && !hits("^(read|write)$", "rewrite"));
        assert!(Matcher::parse("(").is_err());
    }
}
