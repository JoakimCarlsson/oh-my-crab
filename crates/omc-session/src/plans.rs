//! The plans a conversation's agent submits: each saved as markdown beside the conversation's history, and in the project too when autosave is on, and the approved one kept for the subagents that help carry it out.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// The environment variable that names the folder plans are autosaved in, relative to the working folder unless it is absolute.
pub(crate) const PLANS_DIR_VARIABLE: &str = "OMC_PLANS_DIR";

/// The folder plans are autosaved in, relative to the working folder, when [`PLANS_DIR_VARIABLE`] does not name another.
const DEFAULT_DIR: &str = ".oh-my-crab/plans";

/// The longest a file name made from a plan's title gets, in bytes, before `.md`.
const MAX_SLUG: usize = 60;

/// The plans of a conversation.
#[derive(Default)]
pub(crate) struct Plans {
    /// What changes as the conversation goes on.
    state: Mutex<State>,
}

/// What the plans of a conversation keep.
#[derive(Default)]
struct State {
    /// The plan the reader approved, which the agent carries out.
    approved: Option<Arc<str>>,
    /// The working folder, which autosaved plans are kept under.
    directory: Option<PathBuf>,
    /// Whether each plan is also saved in the project.
    autosave: bool,
}

impl Plans {
    /// Runs `with` on the state, whatever a panicking holder left.
    fn state<T>(&self, with: impl FnOnce(&mut State) -> T) -> T {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        with(&mut state)
    }

    /// The plan the reader approved, when there is one.
    pub(crate) fn approved(&self) -> Option<Arc<str>> {
        self.state(|state| state.approved.clone())
    }

    /// Records `plan` as the one the reader approved, or that none is, as the history of a turn says.
    pub(crate) fn set_approved(&self, plan: Option<&str>) {
        self.state(|state| {
            if state.approved.as_deref() != plan {
                state.approved = plan.map(Arc::from);
            }
        });
    }

    /// Keeps autosaved plans under the working folder `directory`.
    pub(crate) fn set_directory(&self, directory: PathBuf) {
        self.state(|state| state.directory = Some(directory));
    }

    /// Whether each plan is also saved in the project.
    pub(crate) fn autosave(&self) -> bool {
        self.state(|state| state.autosave)
    }

    /// Turns saving each plan in the project too on or off.
    pub(crate) fn set_autosave(&self, enabled: bool) {
        self.state(|state| state.autosave = enabled);
    }

    /// The folder plans are autosaved in when autosave is on and the working folder is known: [`PLANS_DIR_VARIABLE`] when it is set, under the working folder unless it is absolute, else [`DEFAULT_DIR`] under the working folder.
    fn autosave_folder(&self) -> Option<PathBuf> {
        let (on, directory) = self.state(|state| (state.autosave, state.directory.clone()));
        let directory = directory.filter(|_| on)?;
        let named = std::env::var(PLANS_DIR_VARIABLE)
            .ok()
            .filter(|named| !named.trim().is_empty());
        Some(match named {
            Some(named) => directory.join(named.trim()),
            None => directory.join(DEFAULT_DIR),
        })
    }

    /// Saves `plan` as markdown: as the next numbered plan in the folder beside `history`, the conversation's history file, when it is saved, and, when autosave is on, in the autosave folder under a name made from its title, replacing an earlier plan of the same title. Returns the files written.
    ///
    /// # Errors
    ///
    /// Returns what went wrong, for the reader, when a folder cannot be made or a file cannot be written; the files written before it stay.
    pub(crate) fn save(&self, plan: &str, history: Option<&Path>) -> Result<Vec<PathBuf>, String> {
        let mut written = Vec::new();
        if let Some(history) = history {
            let folder = conversation_folder(history);
            create_private_folder(&folder).map_err(|error| cannot(&folder, &error))?;
            let file = folder.join(format!("plan-{}.md", next_number(&folder)));
            write_private(&file, plan).map_err(|error| cannot(&file, &error))?;
            written.push(file);
        }
        if let Some(folder) = self.autosave_folder() {
            std::fs::create_dir_all(&folder).map_err(|error| cannot(&folder, &error))?;
            let file = folder.join(format!("{}.md", slug(plan)));
            std::fs::write(&file, with_newline(plan)).map_err(|error| cannot(&file, &error))?;
            written.push(file);
        }
        Ok(written)
    }
}

/// What to tell the reader when `path` could not be made or written for `error`.
fn cannot(path: &Path, error: &std::io::Error) -> String {
    format!("cannot save the plan to {}: {error}", path.display())
}

/// The folder the plans of the conversation whose history is kept in `history` are saved in: beside it, named like it with `.plans` in place of `.jsonl`.
pub(crate) fn conversation_folder(history: &Path) -> PathBuf {
    history.with_extension("plans")
}

/// The number the next plan saved in `folder` gets: one more than the highest of the `plan-<n>.md` files in it, or 1.
fn next_number(folder: &Path) -> u64 {
    std::fs::read_dir(folder)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name();
            name.to_str()?
                .strip_prefix("plan-")?
                .strip_suffix(".md")?
                .parse::<u64>()
                .ok()
        })
        .max()
        .map_or(1, |highest| highest + 1)
}

/// `plan` ending in exactly one newline.
fn with_newline(plan: &str) -> String {
    format!("{}\n", plan.trim_end())
}

/// A file name for `plan`, without `.md`: its first line that is not blank, without the marks of a heading, in lowercase, with every run of characters that are not letters or digits as one `-`, cut to [`MAX_SLUG`] bytes; `plan` when that leaves nothing.
fn slug(plan: &str) -> String {
    let title = plan
        .lines()
        .map(|line| line.trim().trim_start_matches('#').trim())
        .find(|line| !line.is_empty())
        .unwrap_or_default();
    let mut slug = String::new();
    for c in title.chars().flat_map(char::to_lowercase) {
        if c.is_alphanumeric() {
            if slug.len() + c.len_utf8() > MAX_SLUG {
                break;
            }
            slug.push(c);
        } else if !slug.is_empty() && !slug.ends_with('-') {
            slug.push('-');
        }
    }
    let slug = slug.trim_end_matches('-');
    if slug.is_empty() {
        "plan".to_owned()
    } else {
        slug.to_owned()
    }
}

/// Makes `folder` and the folders above it, readable by the owner only on systems that have file modes, as the conversation's history is.
///
/// # Errors
///
/// Returns the error of making it.
fn create_private_folder(folder: &Path) -> std::io::Result<()> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    builder.create(folder)
}

/// Writes `plan` to `file`, made readable by the owner only on systems that have file modes, as the conversation's history is.
///
/// # Errors
///
/// Returns the error of opening or writing it.
fn write_private(file: &Path, plan: &str) -> std::io::Result<()> {
    use std::io::Write;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    options.open(file)?.write_all(with_newline(plan).as_bytes())
}

#[cfg(test)]
/// Tests for saving plans.
mod tests {
    use super::*;

    /// An empty folder for the test `name` under the workspace's git-ignored `build/tests/omc-session`.
    fn scratch(name: &str) -> PathBuf {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../build/tests/omc-session/plans")
            .join(name);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// File names come from the title, in lowercase with one `-` between words, and are never empty.
    #[test]
    fn names_come_from_the_title() {
        assert_eq!(slug("\n## Add a --name flag!\n1. x"), "add-a-name-flag");
        assert_eq!(slug("# ***\n"), "plan");
        assert_eq!(slug(""), "plan");
        assert_eq!(slug(&"word ".repeat(40)).len(), MAX_SLUG - 1);
    }

    /// Each plan is saved beside the conversation's history under the next number, and in the project only while autosave is on, replacing a plan of the same title there.
    #[test]
    fn plans_are_numbered_beside_the_history_and_autosaved_by_title() {
        let dir = scratch("save");
        let history = dir.join("conversations/c1.jsonl");
        let plans = Plans::default();
        plans.set_directory(dir.join("project"));
        let written = plans.save("# First\n1. a", Some(&history)).unwrap();
        let folder = dir.join("conversations/c1.plans");
        assert_eq!(written, [folder.join("plan-1.md")]);
        assert_eq!(
            std::fs::read_to_string(&written[0]).unwrap(),
            "# First\n1. a\n"
        );
        plans.set_autosave(true);
        assert!(plans.autosave());
        let written = plans.save("# First\n1. b", Some(&history)).unwrap();
        let saved = dir.join("project").join(DEFAULT_DIR).join("first.md");
        assert_eq!(written, [folder.join("plan-2.md"), saved.clone()]);
        plans.save("# First\n1. c", None).unwrap();
        assert_eq!(std::fs::read_to_string(&saved).unwrap(), "# First\n1. c\n");
        assert_eq!(next_number(&folder), 3);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(folder.join("plan-1.md"))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o077, 0);
        }
    }

    /// The approved plan is kept until the history says otherwise.
    #[test]
    fn the_approved_plan_is_kept() {
        let plans = Plans::default();
        assert_eq!(plans.approved(), None);
        plans.set_approved(Some("# P"));
        let kept = plans.approved().unwrap();
        plans.set_approved(Some("# P"));
        assert!(Arc::ptr_eq(&kept, &plans.approved().unwrap()));
        plans.set_approved(None);
        assert_eq!(plans.approved(), None);
    }
}
