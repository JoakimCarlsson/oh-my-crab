//! Persistent credential storage in `auth.json`, in the folder of the work being done under [`with_home`] or `~/.config/oh-my-crab`.

use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::{AuthError, Credentials};

/// On-disk shape of `auth.json`.
#[derive(Default, Serialize, Deserialize)]
pub struct AuthFile {
    /// The id of the provider messages are sent to.
    pub active: Option<String>,
    /// Stored credentials by provider id.
    pub credentials: BTreeMap<String, Credentials>,
}

tokio::task_local! {
    /// The folder `auth.json` lives in for the work running in this task, when the host chose one.
    static HOME: Option<PathBuf>;
}

/// Runs `work` with its credentials kept in `home`, or in the default folder when there is none.
///
/// The folder belongs to `work` alone, so conversations of one process may each keep their own logins. It does not reach tasks `work` spawns.
pub async fn with_home<F: std::future::Future>(home: Option<PathBuf>, work: F) -> F::Output {
    HOME.scope(home, work).await
}

/// Returns the path of `auth.json`.
///
/// # Errors
///
/// Returns [`AuthError::Storage`] when no config directory can be determined.
pub fn path() -> Result<PathBuf, AuthError> {
    if let Ok(Some(home)) = HOME.try_with(Clone::clone) {
        return Ok(home.join("auth.json"));
    }
    let base = dirs::config_dir()
        .ok_or_else(|| AuthError::Storage("no config directory available".into()))?;
    Ok(base.join("oh-my-crab").join("auth.json"))
}

/// Loads the credential file, returning an empty one when it does not exist.
///
/// # Errors
///
/// Returns [`AuthError::Storage`] when the file exists but cannot be read or parsed.
pub fn load() -> Result<AuthFile, AuthError> {
    let path = path()?;
    match fs::read(&path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map_err(|e| AuthError::Storage(format!("{}: {e}", path.display()))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(AuthFile::default()),
        Err(e) => Err(AuthError::Storage(format!("{}: {e}", path.display()))),
    }
}

/// Writes the credential file atomically with `0600` permissions.
///
/// # Errors
///
/// Returns [`AuthError::Storage`] when the directory or file cannot be written.
pub fn save(file: &AuthFile) -> Result<(), AuthError> {
    let path = path()?;
    let dir = path
        .parent()
        .ok_or_else(|| AuthError::Storage("auth path has no parent".into()))?;
    fs::create_dir_all(dir).map_err(|e| AuthError::Storage(e.to_string()))?;
    let staging = path.with_extension("json.tmp");
    let json = serde_json::to_vec_pretty(file).map_err(|e| AuthError::Storage(e.to_string()))?;
    write_private(&staging, &json).map_err(|e| AuthError::Storage(e.to_string()))?;
    fs::rename(&staging, &path).map_err(|e| AuthError::Storage(e.to_string()))
}

/// Creates `path` and writes `bytes` to it; on systems with file modes the file is readable by its owner only (`0600`).
fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let mut file = options.open(path)?;
    file.write_all(bytes)?;
    file.sync_all()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A file written before providers were looked up by id still loads and saves unchanged.
    #[test]
    fn an_existing_file_round_trips() {
        let saved = r#"{"active":"claude","credentials":{"claude":{"access":"a","refresh":"r","expires_at_ms":1,"account_id":null}}}"#;
        let file: AuthFile = serde_json::from_str(saved).unwrap();
        assert_eq!(file.active.as_deref(), Some("claude"));
        assert_eq!(file.credentials["claude"].access, "a");
        let again: serde_json::Value =
            serde_json::from_slice(&serde_json::to_vec(&file).unwrap()).unwrap();
        assert_eq!(again["active"], "claude");
        assert!(again["credentials"]["claude"].is_object());
    }
}

#[cfg(test)]
/// Tests for where credentials are kept.
mod home_tests {
    use super::*;

    /// Work that runs side by side keeps its credentials in its own folder, and work outside any scope in the default one.
    #[tokio::test]
    async fn homes_belong_to_the_work_that_names_them() {
        let one = with_home(Some(PathBuf::from("/one")), async { path().unwrap() });
        let two = with_home(Some(PathBuf::from("/two")), async {
            tokio::task::yield_now().await;
            path().unwrap()
        });
        let (one, two) = tokio::join!(one, two);
        assert_eq!(one, PathBuf::from("/one/auth.json"));
        assert_eq!(two, PathBuf::from("/two/auth.json"));
        assert!(path().unwrap().ends_with("oh-my-crab/auth.json"));
    }
}
