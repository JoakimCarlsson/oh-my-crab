//! Which tools are hidden from the tool list until the model looks them up.
//!
//! A hidden tool stays in the [`crate::Registry`] and runs like any other when
//! it is called, its arguments checked against its schema, approved and shown
//! to the hooks. Only its description and schema are left out of the tool list
//! sent with each request: the lookup tool names every hidden tool with a
//! one-line summary, and returns the description and schema of the ones the
//! model asks for. The tool list and the lookup tool's description are made
//! once, so the prefix the provider caches does not change when a tool is
//! looked up.

/// The environment variable that says which tools are hidden until looked up.
pub const DEFERRED_VARIABLE: &str = "OMC_DEFERRED_TOOLS";

/// The built-in tools hidden until looked up when [`DEFERRED_VARIABLE`] does not say: ones most turns never call, or whose description is long for how rarely they are used.
pub const DEFAULT_DEFERRED: &[&str] = &[
    "ast_edit",
    "ast_grep",
    "debug",
    "mcp_resources",
    "memory_edit",
];

/// The word in [`DEFERRED_VARIABLE`] that stands for [`DEFAULT_DEFERRED`].
const DEFAULT_WORD: &str = "default";

/// The tools that are never hidden: the lookup tool itself and the ones the agent loop answers or needs to end a turn or a mode.
const NEVER_DEFERRED: &[&str] = &[crate::tools::LOAD_TOOLS, "exit_plan_mode", "submit_result"];

/// Which tools are hidden until looked up, by name or by a name prefix ending in `*`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Deferral {
    /// The names, and the prefixes with their `*`.
    patterns: Vec<String>,
}

impl Default for Deferral {
    /// The built-in tools of [`DEFAULT_DEFERRED`].
    fn default() -> Self {
        Self::parse(None)
    }
}

impl Deferral {
    /// Hides no tool.
    pub fn none() -> Self {
        Self {
            patterns: Vec::new(),
        }
    }

    /// The tools [`DEFERRED_VARIABLE`] names, as [`Deferral::parse`] reads it.
    pub fn from_env() -> Self {
        Self::parse(std::env::var(DEFERRED_VARIABLE).ok().as_deref())
    }

    /// The tools `value`, the text of [`DEFERRED_VARIABLE`], names: comma-separated tool names, name prefixes ending in `*` such as `mcp__*`, and `default` for [`DEFAULT_DEFERRED`]. Nothing gives the default; an empty or blank value, `0`, `none`, `off`, `false` or `no` hides no tool.
    pub fn parse(value: Option<&str>) -> Self {
        let Some(value) = value else {
            return Self::defaults();
        };
        let off = value.trim().to_ascii_lowercase();
        if matches!(off.as_str(), "" | "0" | "none" | "off" | "false" | "no") {
            return Self::none();
        }
        let mut patterns: Vec<String> = Vec::new();
        for entry in value
            .split(',')
            .map(str::trim)
            .filter(|entry| !entry.is_empty())
        {
            if entry.eq_ignore_ascii_case(DEFAULT_WORD) {
                patterns.extend(DEFAULT_DEFERRED.iter().map(|name| (*name).to_owned()));
            } else {
                patterns.push(entry.to_owned());
            }
        }
        Self { patterns }
    }

    /// The tools of [`DEFAULT_DEFERRED`].
    fn defaults() -> Self {
        Self {
            patterns: DEFAULT_DEFERRED
                .iter()
                .map(|name| (*name).to_owned())
                .collect(),
        }
    }

    /// Whether the tool called `name` is hidden until looked up.
    pub fn defers(&self, name: &str) -> bool {
        !NEVER_DEFERRED.contains(&name)
            && self
                .patterns
                .iter()
                .any(|pattern| match pattern.strip_suffix('*') {
                    Some(prefix) => name.starts_with(prefix),
                    None => pattern == name,
                })
    }
}

#[cfg(test)]
/// Tests for reading which tools are hidden.
mod tests {
    use super::*;

    /// Nothing set hides the default tools; names, prefixes and `default` combine; the words that turn a switch off hide nothing; the lookup tool is never hidden.
    #[test]
    fn reads_the_variable() {
        let unset = Deferral::parse(None);
        assert!(unset.defers("debug") && unset.defers("ast_grep"));
        assert!(!unset.defers("read") && !unset.defers("mcp__github__get_issue"));
        let mixed = Deferral::parse(Some(" default , mcp__* ,lsp"));
        assert!(mixed.defers("debug") && mixed.defers("lsp"));
        assert!(mixed.defers("mcp__github__get_issue") && !mixed.defers("mcp_resources_x"));
        let only = Deferral::parse(Some("web_search"));
        assert!(only.defers("web_search") && !only.defers("debug"));
        for off in ["", " ", "0", "none", "OFF", "false", "no"] {
            assert!(!Deferral::parse(Some(off)).defers("debug"), "{off:?}");
        }
        assert!(!Deferral::parse(Some("*")).defers(crate::tools::LOAD_TOOLS));
        assert!(Deferral::parse(Some("*")).defers("bash"));
    }
}
