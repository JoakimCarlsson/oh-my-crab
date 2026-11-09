//! How the copy an isolated subagent works in is made, as the user picks it with [`ISOLATION_VARIABLE`]; [`crate::worktree`] makes it.

/// The environment variable that picks how isolated subagents get their copy of the working tree: `auto`, `cow` or `worktree`, as [`Backend`] tells.
pub const ISOLATION_VARIABLE: &str = "OMC_ISOLATION";

/// How the copy an isolated subagent works in is made.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Backend {
    /// A copy-on-write clone when the file system can make one, and a git worktree checked out from the snapshot otherwise; outside a git repository, a clone or nothing.
    #[default]
    Auto,
    /// A copy-on-write clone always; isolation is refused where none can be made.
    Cow,
    /// A git worktree checked out from the snapshot always; isolation is refused outside a git repository.
    Worktree,
}

/// How isolated subagents get their copy, as [`ISOLATION_VARIABLE`] says; [`Backend::Auto`] when it says nothing this knows.
pub fn isolation_backend() -> Backend {
    backend_from(std::env::var(ISOLATION_VARIABLE).ok().as_deref())
}

/// The backend `value`, the text of [`ISOLATION_VARIABLE`], asks for, as [`isolation_backend`] reads it.
fn backend_from(value: Option<&str>) -> Backend {
    match value
        .map(|text| text.trim().to_ascii_lowercase())
        .as_deref()
    {
        Some("cow") => Backend::Cow,
        Some("worktree") => Backend::Worktree,
        _ => Backend::Auto,
    }
}

#[cfg(test)]
/// Tests of reading the setting.
mod tests {
    use super::*;

    /// The setting is read without regard to case or surrounding space, and anything it does not name, or nothing, is `auto`.
    #[test]
    fn the_backend_setting_is_read() {
        assert_eq!(backend_from(Some(" COW ")), Backend::Cow);
        assert_eq!(backend_from(Some("worktree")), Backend::Worktree);
        assert_eq!(backend_from(Some("auto")), Backend::Auto);
        assert_eq!(backend_from(Some("copy")), Backend::Auto);
        assert_eq!(backend_from(None), Backend::Auto);
    }
}
