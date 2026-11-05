//! Listing a directory's entries for `read`.

use std::path::Path;

use ignore::WalkBuilder;

/// Most entries one listing shows.
pub(super) const MAX_ENTRIES: usize = 500;

/// Lists the entries of the directory `dir`, called `label` in the header.
///
/// Directories come first and end in `/`; within each group entries are sorted
/// by name. Entries that `.gitignore` excludes, and `.git` itself, are left
/// out. Past [`MAX_ENTRIES`] a last line says how many more there are.
pub(super) fn list(dir: &Path, label: &str) -> String {
    let mut entries: Vec<(bool, String)> = WalkBuilder::new(dir)
        .hidden(false)
        .require_git(false)
        .max_depth(Some(1))
        .filter_entry(|entry| entry.file_name() != ".git")
        .build()
        .flatten()
        .filter(|entry| entry.depth() == 1)
        .map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            (entry.path().is_dir(), name)
        })
        .collect();
    if entries.is_empty() {
        return format!("[directory {label} is empty]");
    }
    entries.sort_by(|(a_dir, a), (b_dir, b)| b_dir.cmp(a_dir).then_with(|| a.cmp(b)));
    let total = entries.len();
    let noun = if total == 1 { "entry" } else { "entries" };
    let mut out = format!("[directory {label}, {total} {noun}]\n");
    for (is_dir, name) in entries.iter().take(MAX_ENTRIES) {
        out.push_str(name);
        if *is_dir {
            out.push('/');
        }
        out.push('\n');
    }
    if total > MAX_ENTRIES {
        out.push_str(&format!("… {} more\n", total - MAX_ENTRIES));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::scratch;

    /// Directories come first with a slash, ignored entries and `.git` are hidden.
    #[test]
    fn lists_entries_respecting_gitignore() {
        let dir = scratch("read-listing");
        std::fs::create_dir_all(dir.join("src/inner")).unwrap();
        std::fs::create_dir(dir.join(".git")).unwrap();
        std::fs::create_dir(dir.join("target")).unwrap();
        std::fs::write(dir.join(".gitignore"), "target/\n*.log\n").unwrap();
        std::fs::write(dir.join("b.txt"), "").unwrap();
        std::fs::write(dir.join("a.rs"), "").unwrap();
        std::fs::write(dir.join("debug.log"), "").unwrap();
        std::fs::write(dir.join("src/inner/deep.rs"), "").unwrap();
        assert_eq!(
            list(&dir, "."),
            "[directory ., 4 entries]\nsrc/\n.gitignore\na.rs\nb.txt\n"
        );
    }

    /// Long listings stop at the cap and count the rest.
    #[test]
    fn caps_long_listings() {
        let dir = scratch("read-listing-cap");
        for index in 0..MAX_ENTRIES + 7 {
            std::fs::write(dir.join(format!("f{index:04}")), "").unwrap();
        }
        let out = list(&dir, "d");
        assert!(out.ends_with("… 7 more\n"));
        assert_eq!(out.lines().count(), MAX_ENTRIES + 2);
    }

    /// An empty directory says so.
    #[test]
    fn reports_empty_directories() {
        let dir = scratch("read-listing-empty");
        assert_eq!(list(&dir, "d"), "[directory d is empty]");
    }
}
