//! Helpers shared by the unit tests.

use std::path::PathBuf;

/// Creates an empty directory named `name` under the workspace's git-ignored `build/tests/omc-context` and returns its path.
pub fn scratch(name: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../build/tests/omc-context")
        .join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir.canonicalize().unwrap()
}

/// Writes `text` to `path` under `base`, creating the folders on the way.
pub fn put(base: &std::path::Path, path: &str, text: &str) {
    let file = base.join(path);
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(file, text).unwrap();
}

/// `path` written as the inside of a JSON string, its backslashes and quotes escaped, so a Windows path can go into JSON text.
pub fn json_path(path: &std::path::Path) -> String {
    let quoted = serde_json::to_string(&path.display().to_string()).unwrap();
    quoted[1..quoted.len() - 1].to_owned()
}
