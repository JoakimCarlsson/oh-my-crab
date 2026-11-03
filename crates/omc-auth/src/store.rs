//! Persistent credential storage in `auth.json`, in the folder set with [`set_home`] or `~/.config/oh-my-crab`.

use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

use crate::{AuthError, Credentials, Provider};

/// On-disk shape of `auth.json`.
#[derive(Default, Serialize, Deserialize)]
pub struct AuthFile {
    /// The provider messages are sent to.
    pub active: Option<Provider>,
    /// Stored credentials by provider.
    pub credentials: BTreeMap<Provider, Credentials>,
}

/// The folder `auth.json` lives in, when the host chose one.
static HOME: OnceLock<PathBuf> = OnceLock::new();

/// Keeps credentials in `home` for the rest of the process; the first call wins.
pub fn set_home(home: PathBuf) {
    let _ = HOME.set(home);
}

/// Returns the path of `auth.json`.
///
/// # Errors
///
/// Returns [`AuthError::Storage`] when no config directory can be determined.
pub fn path() -> Result<PathBuf, AuthError> {
    if let Some(home) = HOME.get() {
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
