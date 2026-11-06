//! Finding the file a mistyped path most likely meant: the files in the
//! working directory whose path ends with the path given, or matches it when
//! case is ignored.
//!
//! `read` follows a single such file and says so; `edit` and `write` only name
//! the candidates, since changing a file the model did not name is not theirs
//! to decide.

use std::path::{Path, PathBuf};

use ignore::WalkBuilder;

use crate::{Sandbox, ToolError, ToolOutput};

/// Most entries the search looks at before giving up, so a huge tree stays cheap.
const MAX_ENTRIES: usize = 50_000;

/// Most candidates an error lists.
const MAX_LISTED: usize = 5;

/// The files under the root of `sandbox` that the missing path `raw` most
/// likely meant, as shown to the user and sorted.
///
/// A file matches when its path relative to the root ends with `raw` at a
/// directory boundary. When none does, the same test runs with case ignored.
/// The walk honors `.gitignore`, skips `.git` and stops after
/// [`MAX_ENTRIES`] entries.
pub(crate) fn lookalikes(sandbox: &Sandbox, raw: &str) -> Vec<String> {
    let Some(wanted) = normalize(sandbox.root(), raw) else {
        return Vec::new();
    };
    let files: Vec<String> = WalkBuilder::new(sandbox.root())
        .hidden(false)
        .require_git(false)
        .filter_entry(|entry| entry.file_name() != ".git")
        .build()
        .take(MAX_ENTRIES)
        .flatten()
        .filter(|entry| entry.file_type().is_some_and(|kind| kind.is_file()))
        .map(|entry| sandbox.display(entry.path()))
        .collect();
    let exact = matching(&files, &wanted, |path| path.to_owned());
    let mut found = if exact.is_empty() {
        matching(&files, &wanted.to_lowercase(), str::to_lowercase)
    } else {
        exact
    };
    found.sort();
    found
}

/// The paths of `files` that end with `wanted` at a directory boundary once
/// both are put through `fold`.
fn matching(files: &[String], wanted: &str, fold: impl Fn(&str) -> String) -> Vec<String> {
    let tail = format!("/{wanted}");
    files
        .iter()
        .filter(|path| {
            let folded = fold(path);
            folded == wanted || folded.ends_with(&tail)
        })
        .cloned()
        .collect()
}

/// `raw` as a relative path with `/` separators and no `./` prefix, leading or
/// trailing slash, made relative to `root` when it is absolute under it;
/// `None` when nothing is left.
fn normalize(root: &Path, raw: &str) -> Option<String> {
    let raw = Path::new(raw)
        .strip_prefix(root)
        .map_or_else(|_| raw.to_owned(), |rest| rest.display().to_string());
    let mut text = raw.replace('\\', "/");
    while let Some(rest) = text.strip_prefix("./") {
        text = rest.to_owned();
    }
    let text = text.trim_matches('/');
    if text.is_empty() || text.split('/').any(|part| part == "..") {
        return None;
    }
    Some(text.to_owned())
}

/// The message for the missing path `shown` that `candidates` might have meant.
pub(crate) fn missing(shown: &str, candidates: &[String]) -> String {
    match candidates {
        [] => format!("{shown} does not exist"),
        [only] => format!(
            "{shown} does not exist; the only file whose path ends with it is {only}. Use \
             that path if it is the file you mean."
        ),
        many => {
            let mut listed = many
                .iter()
                .take(MAX_LISTED)
                .cloned()
                .collect::<Vec<_>>()
                .join(", ");
            if many.len() > MAX_LISTED {
                listed.push_str(&format!(" and {} more", many.len() - MAX_LISTED));
            }
            format!(
                "{shown} does not exist; files whose path ends with it: {listed}. Use the full \
                 path of the one you mean."
            )
        }
    }
}

/// Fails with [`missing`] for the file `shown` at `path` when it does not
/// exist, naming the files it might have meant.
///
/// # Errors
///
/// Returns [`ToolError::Failed`] when `path` does not exist.
pub(crate) fn require(sandbox: &Sandbox, path: &Path, shown: &str) -> Result<(), ToolError> {
    if path.exists() {
        return Ok(());
    }
    Err(ToolError::Failed(missing(
        shown,
        &lookalikes(sandbox, shown),
    )))
}

/// The file a read of the missing `path`, shown as `label`, goes to instead:
/// the only lookalike with its name and a note saying so.
///
/// Returns the arguments unchanged with no note when `path` exists or nothing
/// looks like it, so the read reports the missing file itself.
///
/// # Errors
///
/// Returns [`ToolError::Failed`] listing the candidates when more than one
/// file looks like the path.
pub(crate) fn redirect(
    sandbox: &Sandbox,
    path: PathBuf,
    label: String,
) -> Result<(PathBuf, String, Option<String>), ToolError> {
    if path.exists() {
        return Ok((path, label, None));
    }
    match lookalikes(sandbox, &label).as_slice() {
        [] => Ok((path, label, None)),
        [only] => {
            let note = format!(
                "[{label} does not exist; showing {only}, the only file whose path ends with it]\n"
            );
            Ok((sandbox.resolve(only)?, only.clone(), Some(note)))
        }
        many => Err(ToolError::Failed(missing(&label, many))),
    }
}

/// `output` with the `note` of [`redirect`], when there is one, before its text.
pub(crate) fn noted(mut output: ToolOutput, note: Option<String>) -> ToolOutput {
    if let Some(note) = note {
        output.text.insert_str(0, &note);
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::scratch;

    /// A sandbox over a fresh scratch directory `name` holding empty files at `paths`.
    fn tree(name: &str, paths: &[&str]) -> Sandbox {
        let dir = scratch(name);
        for path in paths {
            let file = dir.join(path);
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            std::fs::write(file, "").unwrap();
        }
        Sandbox::new(&dir).unwrap()
    }

    /// A path matches files that end with it at a directory boundary, then
    /// with case ignored, and never partial names.
    #[test]
    fn finds_files_ending_with_the_path() {
        let sandbox = tree(
            "lookalike-find",
            &[
                "crates/a/src/lib.rs",
                "crates/b/src/main.rs",
                "src/xlib.rs",
                "docs/README.md",
            ],
        );
        assert_eq!(
            lookalikes(&sandbox, "a/src/lib.rs"),
            vec!["crates/a/src/lib.rs"]
        );
        assert_eq!(
            lookalikes(&sandbox, "./src/main.rs"),
            vec!["crates/b/src/main.rs"]
        );
        assert_eq!(lookalikes(&sandbox, "readme.md"), vec!["docs/README.md"]);
        assert_eq!(lookalikes(&sandbox, "lib.rs"), vec!["crates/a/src/lib.rs"]);
        assert!(lookalikes(&sandbox, "nothing.rs").is_empty());
        assert!(lookalikes(&sandbox, "../lib.rs").is_empty());
        assert!(lookalikes(&sandbox, "").is_empty());
    }

    /// Ignored files are not offered.
    #[test]
    fn honors_gitignore() {
        let sandbox = tree("lookalike-ignore", &["target/out/lib.rs", "src/lib.rs"]);
        std::fs::write(sandbox.root().join(".gitignore"), "target/\n").unwrap();
        assert_eq!(lookalikes(&sandbox, "lib.rs"), vec!["src/lib.rs"]);
    }

    /// A read goes to a single lookalike with a note, fails on several and
    /// leaves the path alone when nothing matches.
    #[test]
    fn redirects_reads() {
        let sandbox = tree("lookalike-redirect", &["a/x.txt", "b/y.txt", "c/y.txt"]);
        let root = sandbox.root().to_path_buf();
        let (path, label, note) = redirect(&sandbox, root.join("x.txt"), "x.txt".into()).unwrap();
        assert_eq!((path, label.as_str()), (root.join("a/x.txt"), "a/x.txt"));
        assert!(
            note.unwrap()
                .contains("x.txt does not exist; showing a/x.txt")
        );
        let several = redirect(&sandbox, root.join("y.txt"), "y.txt".into()).unwrap_err();
        assert!(
            several.to_string().contains("b/y.txt, c/y.txt"),
            "{several}"
        );
        let (_, label, note) = redirect(&sandbox, root.join("z.txt"), "z.txt".into()).unwrap();
        assert_eq!((label.as_str(), note), ("z.txt", None));
    }

    /// The message lists at most five candidates and counts the rest.
    #[test]
    fn lists_at_most_five() {
        let many: Vec<String> = (0..7).map(|i| format!("d{i}/f.rs")).collect();
        let text = missing("f.rs", &many);
        assert!(
            text.contains("d0/f.rs, d1/f.rs, d2/f.rs, d3/f.rs, d4/f.rs and 2 more"),
            "{text}"
        );
        assert_eq!(missing("f.rs", &[]), "f.rs does not exist");
    }
}
