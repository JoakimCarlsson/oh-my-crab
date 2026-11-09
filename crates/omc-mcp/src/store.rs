//! Sign-ins to tool servers, kept in `mcp-auth.json` in crab's config folder, readable by the user alone, and shared by every account a client signs in with.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The variable that names the file sign-ins to tool servers are kept in, instead of `mcp-auth.json` in crab's config folder.
pub const MCP_AUTH_FILE_VARIABLE: &str = "OMC_MCP_AUTH_FILE";

/// What is kept about signing in to one tool server.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct Grant {
    /// The client id crab signs in as.
    pub client_id: String,
    /// The client secret, for a confidential client.
    #[serde(default)]
    pub client_secret: Option<String>,
    /// Where tokens are asked for and refreshed.
    pub token_endpoint: String,
    /// The resource the tokens are for: the server's address.
    pub resource: String,
    /// The token requests carry.
    pub access: String,
    /// The token that gets a new access token, when the server gave one.
    #[serde(default)]
    pub refresh: Option<String>,
    /// When the access token expires, in milliseconds since the Unix epoch, when the server said.
    #[serde(default)]
    pub expires_at_ms: Option<u64>,
}

impl Grant {
    /// Whether the access token has expired, or expires within a minute.
    pub fn expired(&self) -> bool {
        self.expires_at_ms
            .is_some_and(|at| at <= now_ms().saturating_add(60_000))
    }
}

/// Milliseconds since the Unix epoch.
pub(crate) fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| {
            u64::try_from(since.as_millis()).unwrap_or(u64::MAX)
        })
}

/// The shape of the file.
#[derive(Default, Serialize, Deserialize)]
struct Kept {
    /// The grants, by the address of the server.
    #[serde(default)]
    servers: BTreeMap<String, Grant>,
}

/// Where sign-ins to tool servers are kept.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Tokens {
    /// The file.
    path: PathBuf,
}

impl Tokens {
    /// The store in the file at `path`.
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// The store in the file [`MCP_AUTH_FILE_VARIABLE`] names, or `mcp-auth.json` in crab's config folder; nothing when there is no config folder.
    ///
    /// It does not follow the folder a client keeps a provider login in: a tool server is signed in to once, and every account of every provider uses that sign-in.
    pub fn locate() -> Option<Self> {
        match std::env::var_os(MCP_AUTH_FILE_VARIABLE) {
            Some(path) if !path.is_empty() => Some(Self::new(path)),
            _ => dirs::config_dir()
                .map(|base| Self::new(base.join("oh-my-crab").join("mcp-auth.json"))),
        }
    }

    /// The file sign-ins are kept in.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The grant kept for the server at `url`, when there is one; a file that cannot be read keeps none.
    pub fn get(&self, url: &str) -> Option<Grant> {
        self.read().servers.remove(url)
    }

    /// Keeps `grant` for the server at `url`, replacing what was kept for it.
    ///
    /// # Errors
    ///
    /// Fails when the folder or the file cannot be written.
    pub fn set(&self, url: &str, grant: Grant) -> std::io::Result<()> {
        let mut kept = self.read();
        kept.servers.insert(url.to_owned(), grant);
        self.write(&kept)
    }

    /// Forgets the grant for the server at `url`.
    ///
    /// # Errors
    ///
    /// Fails when the file cannot be written.
    pub fn remove(&self, url: &str) -> std::io::Result<()> {
        let mut kept = self.read();
        if kept.servers.remove(url).is_none() {
            return Ok(());
        }
        self.write(&kept)
    }

    /// What the file keeps; nothing when it is missing or cannot be read.
    fn read(&self) -> Kept {
        std::fs::read(&self.path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    /// Replaces the file with `kept`, whole, through a file beside it, readable by the user alone on systems with file modes.
    ///
    /// # Errors
    ///
    /// Fails when the folder or the file cannot be written.
    fn write(&self, kept: &Kept) -> std::io::Result<()> {
        if let Some(folder) = self.path.parent() {
            std::fs::create_dir_all(folder)?;
        }
        let bytes = serde_json::to_vec_pretty(kept).map_err(std::io::Error::other)?;
        let staging = self
            .path
            .with_extension(format!("json.{}", std::process::id()));
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        let mut file = options.open(&staging)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        std::fs::rename(&staging, &self.path)
    }
}

#[cfg(test)]
/// Tests for keeping sign-ins.
mod tests {
    use super::*;

    /// A grant is kept, read back and forgotten, in a file only its owner can read.
    #[test]
    fn grants_round_trip() {
        let folder = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../build/tests/mcp-store");
        let _ = std::fs::remove_dir_all(&folder);
        let tokens = Tokens::new(folder.join("mcp-auth.json"));
        assert!(tokens.get("https://a").is_none());
        let grant = Grant {
            client_id: "c".into(),
            token_endpoint: "https://a/token".into(),
            resource: "https://a".into(),
            access: "t".into(),
            expires_at_ms: Some(1),
            ..Grant::default()
        };
        tokens.set("https://a", grant.clone()).unwrap();
        assert_eq!(tokens.get("https://a"), Some(grant.clone()));
        assert!(grant.expired());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(tokens.path())
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        tokens.remove("https://a").unwrap();
        assert!(tokens.get("https://a").is_none());
        let _ = std::fs::remove_dir_all(&folder);
    }
}
