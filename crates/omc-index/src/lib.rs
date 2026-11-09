//! A local index of a project's code, searched by meaning and by keyword.
//!
//! The files of a project, as `git` sees them, are cut into chunks along their
//! syntax tree ([`chunk`]), each chunk is embedded with a small static model
//! for code ([`Embedder`]) and stored with its words in an SQLite database
//! kept in crab's cache folder ([`Index`]): sqlite-vec holds the vectors and
//! FTS5 the words. A search ranks the chunks both ways and fuses the two
//! rankings by reciprocal rank ([`fuse`]). [`CodeIndex`] is what a tool uses:
//! it opens the index the first time it is asked, brings it up to date before
//! every search, re-reading only files whose size or time of change moved and
//! re-embedding only those whose bytes changed, and searches by keyword alone
//! when the model cannot be had.

pub mod chunk;
mod embed;
mod error;
mod index;
mod rank;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

pub use embed::{Embed, Embedder, MODEL_REPO, StaticEmbedder};
pub use error::IndexError;
pub use index::{Hit, Index, Refreshed};
pub use rank::{FUSION_K, cosine, fuse, query_terms};

/// The environment variable that turns the code index, and with it `code_search` and semantic memory search, off when it is `0`, `off`, `false` or `no`.
pub const SWITCH_VARIABLE: &str = "OMC_CODE_INDEX";

/// The environment variable that moves the folder the indexes and the embedding model are kept in.
pub const ROOT_VARIABLE: &str = "OMC_INDEX_DIR";

/// Whether [`SWITCH_VARIABLE`] turns the code index off.
pub fn disabled_by_environment() -> bool {
    std::env::var(SWITCH_VARIABLE).is_ok_and(|value| {
        matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "0" | "off" | "false" | "no"
        )
    })
}

/// The folder the indexes and the embedding model are kept in: the one [`ROOT_VARIABLE`] names when it is set and not empty, else `index` in crab's cache folder, or nothing when there is none.
pub fn index_root() -> Option<PathBuf> {
    match std::env::var_os(ROOT_VARIABLE) {
        Some(folder) if !folder.is_empty() => Some(PathBuf::from(folder)),
        _ => Some(dirs::cache_dir()?.join("oh-my-crab").join("index")),
    }
}

/// What a search of a [`CodeIndex`] found.
#[derive(Debug)]
pub struct Found {
    /// The chunks, best first.
    pub hits: Vec<Hit>,
    /// What bringing the index up to date did first.
    pub refreshed: Refreshed,
    /// Why the search went by keyword alone, when it did although it was to use the model.
    pub keyword_only: Option<String>,
}

/// The index of one project, opened the first time it is searched and brought up to date before each search.
pub struct CodeIndex {
    /// The project's folder.
    root: PathBuf,
    /// The database file.
    database: PathBuf,
    /// The embedding model, when searches go by meaning too.
    embedder: Option<Arc<Embedder>>,
    /// The index, once opened.
    index: Arc<Mutex<Option<Index>>>,
}

impl CodeIndex {
    /// The index of the project in `root`, kept in `database`, embedding with `embedder` when it is given.
    pub fn new(root: &Path, database: &Path, embedder: Option<Arc<Embedder>>) -> Self {
        Self {
            root: root.to_path_buf(),
            database: database.to_path_buf(),
            embedder,
            index: Arc::new(Mutex::new(None)),
        }
    }

    /// The index of the project in `root` in the index folder of [`index_root`], embedding with the process's shared model; nothing when the code index is turned off or there is no cache folder.
    pub fn for_project(root: &Path) -> Option<Self> {
        if disabled_by_environment() {
            return None;
        }
        let database = Index::location(&index_root()?, root);
        Some(Self::new(root, &database, Embedder::shared()))
    }

    /// The project's folder.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Brings the index up to date and returns its `limit` best chunks for `query`, from files that `path` matches when it is given; see [`Index::search`].
    ///
    /// The model is downloaded on the first search that needs it. When it cannot be downloaded or loaded the search goes by keyword alone and says why.
    ///
    /// # Errors
    ///
    /// Returns [`IndexError::Glob`] when `path` is not a valid glob, and [`IndexError::Database`] or [`IndexError::Io`] when the index cannot be opened, read or written.
    pub async fn search(
        &self,
        query: &str,
        path: Option<&str>,
        limit: usize,
    ) -> Result<Found, IndexError> {
        let (model, keyword_only) = match &self.embedder {
            Some(embedder) => match embedder.ensure().await {
                Ok(model) => (Some(model), None),
                Err(error) => (None, Some(error.to_string())),
            },
            None => (None, None),
        };
        let index = self.index.clone();
        let (root, database) = (self.root.clone(), self.database.clone());
        let (query, path) = (query.to_owned(), path.map(str::to_owned));
        tokio::task::spawn_blocking(move || {
            let mut guard = index
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let index = match &mut *guard {
                Some(index) => index,
                empty => empty.insert(Index::open(&database, &root)?),
            };
            let refreshed = index.refresh(model.as_deref())?;
            let vector = model
                .as_ref()
                .and_then(|model| model.embed(std::slice::from_ref(&query)).pop());
            let hits = index.search(&query, vector.as_deref(), path.as_deref(), limit)?;
            Ok(Found {
                hits,
                refreshed,
                keyword_only,
            })
        })
        .await
        .map_err(|error| IndexError::Model(format!("the search stopped: {error}")))?
    }
}

#[cfg(test)]
/// Tests for the index of a project as a tool uses it.
mod tests {
    use super::*;

    /// An embedder that gives every text the same vector.
    struct Flat;

    impl Embed for Flat {
        /// Names the test model.
        fn model(&self) -> &str {
            "flat"
        }

        /// Uses four numbers.
        fn dimensions(&self) -> usize {
            4
        }

        /// Gives each text the same vector.
        fn embed(&self, texts: &[String]) -> Vec<Vec<f32>> {
            texts.iter().map(|_| vec![0.5; 4]).collect()
        }
    }

    /// The folder `name` under the workspace's git-ignored `build/tests/omc-index`, emptied, with a project in `project`.
    fn folder(name: &str) -> PathBuf {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../build/tests/omc-index")
            .join(name);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("project")).unwrap();
        dir
    }

    /// A search opens the index, picks up a file written after the first search, and uses the model it is given.
    #[tokio::test]
    async fn searches_open_and_refresh_the_index() {
        let dir = folder("code-index");
        let root = dir.join("project");
        std::fs::write(root.join("a.py"), "def fetch_rows():\n    pass\n").unwrap();
        let embedder = Arc::new(Embedder::with_model(Arc::new(Flat)));
        let index = CodeIndex::new(&root, &dir.join("db.sqlite3"), Some(embedder));
        let first = index.search("fetch rows", None, 5).await.unwrap();
        assert_eq!(first.hits[0].path, "a.py");
        assert_eq!((first.refreshed.changed, first.refreshed.embedded), (1, 1));
        assert!(first.keyword_only.is_none());
        std::fs::write(root.join("b.py"), "def store_rows():\n    pass\n").unwrap();
        let second = index.search("store", None, 5).await.unwrap();
        assert_eq!(second.hits[0].path, "b.py");
        assert_eq!(second.refreshed.changed, 1);
    }

    /// With the real model, downloaded into the build folder, a question in plain words about this workspace finds the code that answers it, and a second search re-reads nothing and keeps to the folder it is given.
    #[tokio::test]
    #[ignore = "downloads the 64 MB model from Hugging Face and indexes the whole workspace"]
    async fn finds_this_workspaces_code_by_meaning() {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../build/tests/omc-index");
        let workspace = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .canonicalize()
            .unwrap();
        let _ = std::fs::remove_file(dir.join("workspace.sqlite3"));
        let embedder = Arc::new(Embedder::new(dir.join("model")));
        let index = CodeIndex::new(&workspace, &dir.join("workspace.sqlite3"), Some(embedder));
        let started = std::time::Instant::now();
        let first = index
            .search("forgive typos when matching saved memories", None, 5)
            .await
            .unwrap();
        let built = started.elapsed();
        let paths: Vec<&str> = first.hits.iter().map(|hit| hit.path.as_str()).collect();
        assert!(first.keyword_only.is_none());
        assert!(
            paths.contains(&"crates/omc-memory/src/search.rs"),
            "{paths:?} after {built:?}"
        );
        let again = index
            .search("download the embedding model", Some("crates/omc-index"), 3)
            .await
            .unwrap();
        assert_eq!(again.refreshed.changed, 0);
        assert!(
            again
                .hits
                .iter()
                .any(|hit| hit.path == "crates/omc-index/src/embed.rs")
        );
    }

    /// Without a model at hand the search goes by keyword and says why.
    #[tokio::test]
    async fn falls_back_to_keywords() {
        let dir = folder("code-index-offline");
        let root = dir.join("project");
        std::fs::write(root.join("a.go"), "func Serve() {}\n").unwrap();
        let embedder = Arc::new(Embedder::from_source(
            dir.join("model"),
            "http://127.0.0.1:1/none",
        ));
        let index = CodeIndex::new(&root, &dir.join("db.sqlite3"), Some(embedder));
        let found = index.search("serve", None, 5).await.unwrap();
        assert_eq!(found.hits[0].path, "a.go");
        assert!(found.keyword_only.unwrap().contains("download"));
    }
}
