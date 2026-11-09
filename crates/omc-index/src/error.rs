//! What can go wrong with the index.

use std::path::PathBuf;

/// What can go wrong while indexing, searching or fetching the embedding model.
#[derive(Debug, thiserror::Error)]
pub enum IndexError {
    /// The index database could not be opened, read or written.
    #[error("the code index database failed: {0}")]
    Database(#[from] rusqlite::Error),
    /// A file or folder could not be read or written.
    #[error("cannot use {path}: {source}")]
    Io {
        /// The file or folder.
        path: PathBuf,
        /// What went wrong.
        source: std::io::Error,
    },
    /// The embedding model could not be downloaded.
    #[error("cannot download the embedding model: {0}")]
    Download(String),
    /// The embedding model could not be loaded.
    #[error("cannot load the embedding model: {0}")]
    Model(String),
    /// The embedding model failed to download or load a short while ago, for the reason given, and is not tried again yet.
    #[error("{0}")]
    Unavailable(String),
    /// The path pattern given to a search is not a valid glob.
    #[error("invalid glob: {0}")]
    Glob(String),
}

impl IndexError {
    /// An [`IndexError::Io`] for `path`.
    pub(crate) fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        Self::Io {
            path: path.into(),
            source,
        }
    }
}
