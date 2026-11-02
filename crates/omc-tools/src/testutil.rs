//! Helpers shared by the unit tests.

use std::path::PathBuf;

/// Creates an empty directory named `name` under the workspace's git-ignored
/// `build/tests/omc-tools` and returns its path.
pub fn scratch(name: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../build/tests/omc-tools")
        .join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir.canonicalize().unwrap()
}
