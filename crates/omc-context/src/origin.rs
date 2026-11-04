//! Where something found on disk came from.

use crate::place::Root;

/// Where an instruction file, skill or agent definition came from.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub enum Origin {
    /// The project's own folders.
    #[default]
    Project,
    /// The reader's own folders, for every project.
    User,
    /// A Claude Code plugin the reader installed.
    Plugin,
    /// The harness itself.
    BuiltIn,
}

impl From<Root> for Origin {
    /// The origin of what is found under `root`: the project's for the project's folders, the reader's for every other.
    fn from(root: Root) -> Self {
        match root {
            Root::Project => Self::Project,
            Root::Claude | Root::Codex | Root::Home => Self::User,
        }
    }
}
