//! Finding the program a command names, the way a shell would.

use std::path::{Path, PathBuf};

/// The path of the program `name`: `name` itself when it names a file, otherwise the first file of that name in a folder of the `PATH`; on Windows a name without an extension also finds the program with one, as `rust-analyzer.exe`.
pub fn find_program(name: &str) -> Option<PathBuf> {
    let direct = Path::new(name);
    if direct.components().count() > 1 {
        return candidates(direct).into_iter().find(|path| path.is_file());
    }
    let folders = std::env::var_os("PATH")?;
    std::env::split_paths(&folders)
        .flat_map(|folder| candidates(&folder.join(name)))
        .find(|candidate| candidate.is_file())
}

/// The files the program at `path` may be: the path itself, and on Windows the path with each extension `PATHEXT` names, or `.exe`, `.cmd` and `.bat` when it names none, for a path without one.
fn candidates(path: &Path) -> Vec<PathBuf> {
    let mut found = vec![path.to_path_buf()];
    if cfg!(windows) && path.extension().is_none() {
        let extensions = std::env::var("PATHEXT").unwrap_or_else(|_| ".EXE;.CMD;.BAT".to_owned());
        found.extend(
            extensions
                .split(';')
                .map(str::trim)
                .filter(|extension| extension.starts_with('.'))
                .map(|extension| {
                    let mut named = path.as_os_str().to_owned();
                    named.push(extension.to_ascii_lowercase());
                    PathBuf::from(named)
                }),
        );
    }
    found
}

#[cfg(test)]
/// Tests for finding programs.
mod tests {
    use super::*;

    /// A program on the `PATH` is found by its bare name, a path is taken as it is, and a name nothing has is not found.
    #[test]
    fn finds_programs() {
        if let Some(found) = find_program("git") {
            assert!(found.is_file());
            let given = found.to_string_lossy().into_owned();
            assert_eq!(find_program(&given), Some(found));
        }
        assert_eq!(find_program("no-such-program-anywhere-at-all"), None);
    }

    /// Only Windows tries extensions, and only for a name without one.
    #[test]
    fn extensions_are_tried_on_windows_only() {
        let tried = candidates(Path::new("bin/tool"));
        assert_eq!(tried[0], PathBuf::from("bin/tool"));
        assert_eq!(tried.len() > 1, cfg!(windows));
        assert_eq!(candidates(Path::new("bin/tool.exe")).len(), 1);
    }
}
