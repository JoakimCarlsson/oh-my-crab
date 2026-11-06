//! Helpers shared by the unit tests.

use std::path::{Path, PathBuf};

/// Creates an empty folder named `name` under the workspace's git-ignored `build/tests/omc-hooks` and returns its canonical path.
pub(crate) fn scratch(name: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../build/tests/omc-hooks")
        .join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir.canonicalize().unwrap()
}

/// Writes `text` to `path` under `base`, creating the folders on the way.
pub(crate) fn put(base: &Path, path: &str, text: &str) {
    let file = base.join(path);
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(file, text).unwrap();
}

/// Runs `git` with `arguments` in `directory`, quietly and without the user's git config, panicking when it fails.
pub(crate) fn git(directory: &Path, arguments: &[&str]) {
    let status = std::process::Command::new("git")
        .args(arguments)
        .current_dir(directory)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .env_remove("GIT_COMMON_DIR")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .unwrap();
    assert!(status.success(), "git {arguments:?} failed");
}
