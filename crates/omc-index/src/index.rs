//! The index of one project, kept in an SQLite database.
//!
//! The database holds the files that were indexed, with their size, time of
//! change and a hash of their bytes; their chunks with the lines they cover
//! and their text; a full-text table of the chunks, ranked by BM25; and,
//! once an embedding model is at hand, a `vec0` table of sqlite-vec holding
//! the vector of each chunk. A refresh walks the project as `git` sees it,
//! skips files whose size and time of change are as recorded, reads the rest,
//! and cuts and embeds again only those whose bytes changed. A search ranks
//! the chunks by their words and by their vectors and fuses the two rankings.

use std::collections::{HashMap, HashSet};
use std::ffi::{c_char, c_int};
use std::path::{Path, PathBuf};
use std::time::{Duration, UNIX_EPOCH};

use globset::{GlobBuilder, GlobMatcher};
use ignore::WalkBuilder;
use rusqlite::functions::FunctionFlags;
use rusqlite::{Connection, OptionalExtension, Transaction, ffi, params};

use crate::chunk::chunks;
use crate::rank::{fuse, identifier_parts, match_expression, query_terms};
use crate::{Embed, IndexError};

/// The version of the database layout; a database of another version is emptied and built again.
const SCHEMA_VERSION: &str = "1";

/// The largest file indexed, in bytes.
const MAX_FILE_BYTES: u64 = 1024 * 1024;

/// The most files indexed in one project.
const MAX_FILES: usize = 50_000;

/// How many chunks are embedded and written at once.
const EMBED_BATCH: usize = 512;

/// How many of the best chunks of each ranking are fused, at the least.
const CANDIDATES: usize = 50;

/// Names of files never worth indexing: lockfiles and other generated lists.
const SKIPPED_NAMES: &[&str] = &[
    "Cargo.lock",
    "package-lock.json",
    "yarn.lock",
    "pnpm-lock.yaml",
    "bun.lockb",
    "poetry.lock",
    "uv.lock",
    "Gemfile.lock",
    "composer.lock",
    "go.sum",
];

/// Names of files that commonly hold secrets, never indexed: a name equal to one, or starting with one followed by `.` or `_` (`.env.local`, `id_rsa.pub`).
const SECRET_NAMES: &[&str] = &[
    ".env",
    ".envrc",
    ".netrc",
    ".npmrc",
    ".pypirc",
    ".pgpass",
    ".htpasswd",
    ".git-credentials",
    "id_rsa",
    "id_dsa",
    "id_ecdsa",
    "id_ed25519",
];

/// Whole names of files that commonly hold secrets, never indexed.
const SECRET_FILES: &[&str] = &[
    "credentials",
    "credentials.json",
    "secrets.json",
    "secrets.yaml",
    "secrets.yml",
    "service-account.json",
];

/// Extensions of files that commonly hold keys, certificates or secrets, never indexed.
const SECRET_EXTENSIONS: &[&str] = &[
    "pem", "key", "crt", "cer", "der", "p12", "pfx", "jks", "keystore", "kdbx", "gpg", "asc",
    "ovpn", "tfstate", "tfvars",
];

/// One chunk a search found.
#[derive(Clone, Debug, PartialEq)]
pub struct Hit {
    /// The file, relative to the project with `/` between folders.
    pub path: String,
    /// The first line of the chunk, counting from 1.
    pub start: usize,
    /// The last line of the chunk, counting from 1.
    pub end: usize,
    /// The text of the chunk.
    pub text: String,
    /// Its fused score; higher is better.
    pub score: f64,
}

/// What a refresh did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Refreshed {
    /// How many files the index holds now.
    pub files: usize,
    /// How many files were cut into chunks again, being new or changed.
    pub changed: usize,
    /// How many files were dropped, being gone or no longer indexable.
    pub removed: usize,
    /// How many chunks were embedded.
    pub embedded: usize,
    /// Whether the walk stopped at [`MAX_FILES`] files.
    pub capped: bool,
}

/// The index of one project.
pub struct Index {
    /// The database.
    connection: Connection,
    /// The project's folder.
    root: PathBuf,
}

impl Index {
    /// The database file the index of the project in `root` is kept in, under the index folder `folder`: `projects/<name>-<hash>.sqlite3`.
    pub fn location(folder: &Path, root: &Path) -> PathBuf {
        let name: String = root
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "root".to_owned())
            .chars()
            .map(|c| {
                if c.is_alphanumeric() || c == '-' || c == '_' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        let hash = fnv(root.to_string_lossy().as_bytes());
        folder
            .join("projects")
            .join(format!("{name}-{hash:016x}.sqlite3"))
    }

    /// Opens the index kept in `database` of the project in `root`, making the file and its tables when they are missing and emptying them when they are of another layout.
    ///
    /// # Errors
    ///
    /// Returns [`IndexError::Io`] when the folder of `database` cannot be made, and [`IndexError::Database`] when the database cannot be opened or set up.
    pub fn open(database: &Path, root: &Path) -> Result<Self, IndexError> {
        if let Some(folder) = database.parent() {
            std::fs::create_dir_all(folder).map_err(|error| IndexError::io(folder, error))?;
        }
        let connection = Connection::open(database)?;
        load_vectors(&connection)?;
        connection.busy_timeout(Duration::from_secs(10))?;
        connection.pragma_update(None, "journal_mode", "WAL")?;
        connection.pragma_update(None, "synchronous", "NORMAL")?;
        let index = Self {
            connection,
            root: root.to_path_buf(),
        };
        index.prepare()?;
        Ok(index)
    }

    /// Makes the tables, first dropping every one when the recorded layout is another.
    ///
    /// # Errors
    ///
    /// Returns [`IndexError::Database`] when a statement fails.
    fn prepare(&self) -> Result<(), IndexError> {
        self.connection.execute_batch(
            "CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);",
        )?;
        if self.meta("schema")?.as_deref() != Some(SCHEMA_VERSION) {
            self.connection.execute_batch(
                "DROP TABLE IF EXISTS chunks_vec;
                 DROP TABLE IF EXISTS chunks_fts;
                 DROP TABLE IF EXISTS chunks;
                 DROP TABLE IF EXISTS files;
                 DELETE FROM meta;",
            )?;
            self.set_meta("schema", SCHEMA_VERSION)?;
        }
        self.connection.execute_batch(
            "CREATE TABLE IF NOT EXISTS files (
                 id INTEGER PRIMARY KEY,
                 path TEXT NOT NULL UNIQUE,
                 size INTEGER NOT NULL,
                 modified INTEGER NOT NULL,
                 hash INTEGER NOT NULL
             );
             CREATE TABLE IF NOT EXISTS chunks (
                 id INTEGER PRIMARY KEY,
                 file INTEGER NOT NULL,
                 start INTEGER NOT NULL,
                 end INTEGER NOT NULL,
                 text TEXT NOT NULL,
                 embedded INTEGER NOT NULL DEFAULT 0
             );
             CREATE INDEX IF NOT EXISTS chunks_file ON chunks (file);
             CREATE INDEX IF NOT EXISTS chunks_unembedded ON chunks (embedded) WHERE embedded = 0;
             CREATE VIRTUAL TABLE IF NOT EXISTS chunks_fts USING fts5 (
                 path, body, parts, content = '', contentless_delete = 1
             );",
        )?;
        Ok(())
    }

    /// The value recorded under `key`, when there is one.
    ///
    /// # Errors
    ///
    /// Returns [`IndexError::Database`] when the query fails.
    fn meta(&self, key: &str) -> Result<Option<String>, IndexError> {
        Ok(self
            .connection
            .query_row("SELECT value FROM meta WHERE key = ?1", [key], |row| {
                row.get(0)
            })
            .optional()?)
    }

    /// Records `value` under `key`.
    ///
    /// # Errors
    ///
    /// Returns [`IndexError::Database`] when the statement fails.
    fn set_meta(&self, key: &str, value: &str) -> Result<(), IndexError> {
        self.connection.execute(
            "INSERT INTO meta (key, value) VALUES (?1, ?2)
             ON CONFLICT (key) DO UPDATE SET value = excluded.value",
            [key, value],
        )?;
        Ok(())
    }

    /// Whether the index holds vectors made by the model `embed`.
    ///
    /// # Errors
    ///
    /// Returns [`IndexError::Database`] when the query fails.
    pub fn has_vectors_of(&self, embed: &dyn Embed) -> Result<bool, IndexError> {
        Ok(self.meta("model")?.as_deref() == Some(embed.model()))
    }

    /// Makes the vector table for the model `embed`, dropping the vectors of another model first.
    ///
    /// # Errors
    ///
    /// Returns [`IndexError::Database`] when a statement fails.
    fn adopt(&self, embed: &dyn Embed) -> Result<(), IndexError> {
        if self.has_vectors_of(embed)? {
            return Ok(());
        }
        self.connection.execute_batch(&format!(
            "DROP TABLE IF EXISTS chunks_vec;
             UPDATE chunks SET embedded = 0;
             CREATE VIRTUAL TABLE chunks_vec USING vec0 (
                 embedding float[{}] distance_metric=cosine
             );",
            embed.dimensions()
        ))?;
        self.set_meta("model", embed.model())
    }

    /// Brings the index up to date with the files of the project, and embeds every chunk without a vector with `embed` when it is given.
    ///
    /// # Errors
    ///
    /// Returns [`IndexError::Database`] when the database cannot be read or written.
    pub fn refresh(&mut self, embed: Option<&dyn Embed>) -> Result<Refreshed, IndexError> {
        if let Some(embed) = embed {
            self.adopt(embed)?;
        }
        let vectors = self.meta("model")?.is_some();
        let mut refreshed = Refreshed::default();
        let known = self.known()?;
        let mut seen: HashSet<String> = HashSet::new();
        let transaction = self.connection.transaction()?;
        for (path, relative) in walk(&self.root) {
            if seen.len() >= MAX_FILES {
                refreshed.capped = true;
                break;
            }
            let Ok(metadata) = std::fs::metadata(&path) else {
                continue;
            };
            let (size, modified) = (metadata.len() as i64, modified_nanos(&metadata));
            let recorded = known.get(&relative);
            if let Some(file) = recorded
                && file.size == size
                && file.modified == modified
            {
                seen.insert(relative);
                continue;
            }
            let Some(text) = readable(&path) else {
                continue;
            };
            let hash = fnv(text.as_bytes()) as i64;
            seen.insert(relative.clone());
            if let Some(file) = recorded
                && file.hash == hash
            {
                transaction.execute(
                    "UPDATE files SET size = ?2, modified = ?3 WHERE id = ?1",
                    params![file.id, size, modified],
                )?;
                continue;
            }
            if let Some(file) = recorded {
                remove_chunks(&transaction, file.id, vectors)?;
            }
            let id = transaction.query_row(
                "INSERT INTO files (path, size, modified, hash) VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT (path) DO UPDATE SET size = ?2, modified = ?3, hash = ?4
                 RETURNING id",
                params![relative, size, modified, hash],
                |row| row.get::<_, i64>(0),
            )?;
            insert_chunks(&transaction, id, &relative, &text)?;
            refreshed.changed += 1;
        }
        for (path, file) in &known {
            if !seen.contains(path) {
                remove_chunks(&transaction, file.id, vectors)?;
                transaction.execute("DELETE FROM files WHERE id = ?1", [file.id])?;
                refreshed.removed += 1;
            }
        }
        transaction.commit()?;
        refreshed.files = seen.len();
        if let Some(embed) = embed {
            refreshed.embedded = self.embed_missing(embed)?;
        }
        Ok(refreshed)
    }

    /// The files the index holds, by path.
    ///
    /// # Errors
    ///
    /// Returns [`IndexError::Database`] when the query fails.
    fn known(&self) -> Result<HashMap<String, Known>, IndexError> {
        let mut statement = self
            .connection
            .prepare("SELECT path, id, size, modified, hash FROM files")?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                Known {
                    id: row.get(1)?,
                    size: row.get(2)?,
                    modified: row.get(3)?,
                    hash: row.get(4)?,
                },
            ))
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Embeds with `embed` every chunk that has no vector yet, in batches, and returns how many it embedded.
    ///
    /// # Errors
    ///
    /// Returns [`IndexError::Database`] when the database cannot be read or written.
    fn embed_missing(&mut self, embed: &dyn Embed) -> Result<usize, IndexError> {
        let mut done = 0;
        loop {
            let batch: Vec<(i64, String)> = {
                let mut statement = self.connection.prepare(
                    "SELECT c.id, f.path, c.text FROM chunks c JOIN files f ON f.id = c.file
                     WHERE c.embedded = 0 LIMIT ?1",
                )?;
                let rows = statement.query_map([EMBED_BATCH as i64], |row| {
                    let path: String = row.get(1)?;
                    let text: String = row.get(2)?;
                    Ok((row.get(0)?, format!("{path}\n{text}")))
                })?;
                rows.collect::<Result<_, _>>()?
            };
            if batch.is_empty() {
                return Ok(done);
            }
            let texts: Vec<String> = batch.iter().map(|(_, text)| text.clone()).collect();
            let vectors = embed.embed(&texts);
            let transaction = self.connection.transaction()?;
            for ((id, _), vector) in batch.iter().zip(&vectors) {
                transaction.execute(
                    "INSERT OR REPLACE INTO chunks_vec (rowid, embedding) VALUES (?1, ?2)",
                    params![id, bytes_of(vector)],
                )?;
                transaction.execute("UPDATE chunks SET embedded = 1 WHERE id = ?1", [id])?;
            }
            transaction.commit()?;
            done += batch.len();
        }
    }

    /// The `limit` chunks that best match `query`, fusing the ranking of their words with the ranking of their closeness to `vector` when it is given and the index holds vectors, and keeping only files that `path` matches when it is given.
    ///
    /// `path` is a glob relative to the project, matching at any depth when it holds no `/`, or a folder or file named as it is.
    ///
    /// # Errors
    ///
    /// Returns [`IndexError::Glob`] when `path` is not a valid glob, and [`IndexError::Database`] when a query fails.
    pub fn search(
        &self,
        query: &str,
        vector: Option<&[f32]>,
        path: Option<&str>,
        limit: usize,
    ) -> Result<Vec<Hit>, IndexError> {
        let filter = PathFilter::new(path)?;
        self.connection.create_scalar_function(
            "path_matches",
            1,
            FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DETERMINISTIC,
            move |context| Ok(filter.matches(&context.get::<String>(0)?)),
        )?;
        let wanted = (limit * 4).max(CANDIDATES) as i64;
        let mut rankings: Vec<Vec<i64>> = Vec::new();
        if let Some(expression) = match_expression(&query_terms(query)) {
            let mut statement = self.connection.prepare(
                "SELECT c.id FROM chunks_fts
                 JOIN chunks c ON c.id = chunks_fts.rowid
                 JOIN files f ON f.id = c.file
                 WHERE chunks_fts MATCH ?1 AND path_matches(f.path)
                 ORDER BY bm25(chunks_fts, 2.0, 1.0, 1.0) LIMIT ?2",
            )?;
            let ids = statement.query_map(params![expression, wanted], |row| row.get(0))?;
            rankings.push(ids.collect::<Result<_, _>>()?);
        }
        if let Some(vector) = vector
            && self.meta("model")?.is_some()
        {
            rankings.push(self.nearest(vector, path.is_some(), wanted)?);
        }
        let fused = fuse(&rankings);
        let mut hits = Vec::new();
        for (id, score) in fused.into_iter().take(limit) {
            let hit = self.connection.query_row(
                "SELECT f.path, c.start, c.end, c.text FROM chunks c
                 JOIN files f ON f.id = c.file WHERE c.id = ?1",
                [id],
                |row| {
                    Ok(Hit {
                        path: row.get(0)?,
                        start: row.get::<_, i64>(1)? as usize,
                        end: row.get::<_, i64>(2)? as usize,
                        text: row.get(3)?,
                        score,
                    })
                },
            )?;
            hits.push(hit);
        }
        Ok(hits)
    }

    /// The ids of the `wanted` chunks closest to `vector`, closest first; when `filtered`, only of files `path_matches` lets through, measured one by one.
    ///
    /// # Errors
    ///
    /// Returns [`IndexError::Database`] when the query fails.
    fn nearest(&self, vector: &[f32], filtered: bool, wanted: i64) -> Result<Vec<i64>, IndexError> {
        let sql = if filtered {
            "SELECT c.id FROM chunks c
             JOIN files f ON f.id = c.file
             JOIN chunks_vec v ON v.rowid = c.id
             WHERE path_matches(f.path)
             ORDER BY vec_distance_cosine(v.embedding, ?1) LIMIT ?2"
        } else {
            "SELECT rowid FROM chunks_vec WHERE embedding MATCH ?1 AND k = ?2 ORDER BY distance"
        };
        let mut statement = self.connection.prepare(sql)?;
        let ids = statement.query_map(params![bytes_of(vector), wanted], |row| row.get(0))?;
        Ok(ids.collect::<Result<_, _>>()?)
    }
}

/// What the index recorded of a file.
struct Known {
    /// Its row.
    id: i64,
    /// Its size in bytes.
    size: i64,
    /// When it last changed, in nanoseconds since 1970.
    modified: i64,
    /// The hash of its bytes.
    hash: i64,
}

/// Which files a search keeps.
struct PathFilter {
    /// The glob, when one was given that holds wildcards.
    glob: Option<GlobMatcher>,
    /// The folder or file, when one was given without wildcards.
    prefix: Option<String>,
}

impl PathFilter {
    /// The filter for `path`, keeping every file when it is not given.
    ///
    /// # Errors
    ///
    /// Returns [`IndexError::Glob`] when `path` is not a valid glob.
    fn new(path: Option<&str>) -> Result<Self, IndexError> {
        let Some(path) = path.map(str::trim).filter(|path| !path.is_empty()) else {
            return Ok(Self {
                glob: None,
                prefix: None,
            });
        };
        let path = path.trim_start_matches("./");
        if !path.contains(['*', '?', '[', '{']) {
            return Ok(Self {
                glob: None,
                prefix: Some(path.trim_end_matches('/').to_owned()),
            });
        }
        let pattern = if path.contains('/') {
            path.to_owned()
        } else {
            format!("**/{path}")
        };
        let glob = GlobBuilder::new(&pattern)
            .literal_separator(true)
            .build()
            .map_err(|error| IndexError::Glob(error.to_string()))?
            .compile_matcher();
        Ok(Self {
            glob: Some(glob),
            prefix: None,
        })
    }

    /// Whether the file at `path`, relative to the project, is kept.
    fn matches(&self, path: &str) -> bool {
        if let Some(glob) = &self.glob {
            return glob.is_match(path);
        }
        match &self.prefix {
            Some(prefix) => {
                path == prefix
                    || path
                        .strip_prefix(prefix.as_str())
                        .is_some_and(|rest| rest.starts_with('/'))
            }
            None => true,
        }
    }
}

/// Registers sqlite-vec's functions and `vec0` table on `connection`.
///
/// # Errors
///
/// Returns [`IndexError::Database`] when sqlite-vec refuses to start.
fn load_vectors(connection: &Connection) -> Result<(), IndexError> {
    type Init = unsafe extern "C" fn(
        *mut ffi::sqlite3,
        *mut *mut c_char,
        *const ffi::sqlite3_api_routines,
    ) -> c_int;
    let code = unsafe {
        let init =
            std::mem::transmute::<*const (), Init>(sqlite_vec::sqlite3_vec_init as *const ());
        init(connection.handle(), std::ptr::null_mut(), std::ptr::null())
    };
    if code == ffi::SQLITE_OK {
        Ok(())
    } else {
        Err(IndexError::Database(rusqlite::Error::SqliteFailure(
            ffi::Error::new(code),
            Some("sqlite-vec failed to start".to_owned()),
        )))
    }
}

/// Removes the chunks of the file `id`, with their words and, when the index holds them, their vectors.
///
/// # Errors
///
/// Returns [`rusqlite::Error`] when a statement fails.
fn remove_chunks(transaction: &Transaction<'_>, id: i64, vectors: bool) -> rusqlite::Result<()> {
    transaction.execute(
        "DELETE FROM chunks_fts WHERE rowid IN (SELECT id FROM chunks WHERE file = ?1)",
        [id],
    )?;
    if vectors {
        transaction.execute(
            "DELETE FROM chunks_vec WHERE rowid IN (SELECT id FROM chunks WHERE file = ?1)",
            [id],
        )?;
    }
    transaction.execute("DELETE FROM chunks WHERE file = ?1", [id])?;
    Ok(())
}

/// Cuts `text`, the text of the file `id` at `path`, into chunks and stores them with their words.
///
/// # Errors
///
/// Returns [`rusqlite::Error`] when a statement fails.
fn insert_chunks(
    transaction: &Transaction<'_>,
    id: i64,
    path: &str,
    text: &str,
) -> rusqlite::Result<()> {
    for chunk in chunks(Path::new(path), text) {
        let chunk_id: i64 = transaction.query_row(
            "INSERT INTO chunks (file, start, end, text) VALUES (?1, ?2, ?3, ?4) RETURNING id",
            params![id, chunk.start as i64, chunk.end as i64, chunk.text],
            |row| row.get(0),
        )?;
        transaction.execute(
            "INSERT INTO chunks_fts (rowid, path, body, parts) VALUES (?1, ?2, ?3, ?4)",
            params![
                chunk_id,
                path,
                chunk.text,
                identifier_parts(&format!("{path}\n{}", chunk.text))
            ],
        )?;
    }
    Ok(())
}

/// The files of the project in `root` worth indexing, each with its path relative to `root` written with `/`: what `.gitignore` and the other ignore files leave, hidden files included, without `.git`, lockfiles or files over [`MAX_FILE_BYTES`].
fn walk(root: &Path) -> impl Iterator<Item = (PathBuf, String)> + '_ {
    WalkBuilder::new(root)
        .hidden(false)
        .require_git(false)
        .max_filesize(Some(MAX_FILE_BYTES))
        .filter_entry(|entry| entry.file_name() != ".git")
        .build()
        .flatten()
        .filter(|entry| entry.file_type().is_some_and(|kind| kind.is_file()))
        .filter(|entry| {
            let name = entry.file_name().to_string_lossy();
            !SKIPPED_NAMES.contains(&name.as_ref()) && !name.ends_with(".min.js") && !secret(&name)
        })
        .filter_map(move |entry| {
            let relative = entry.path().strip_prefix(root).ok()?;
            let parts: Vec<String> = relative
                .components()
                .map(|part| part.as_os_str().to_string_lossy().into_owned())
                .collect();
            Some((entry.path().to_path_buf(), parts.join("/")))
        })
}

/// Whether a file called `name` commonly holds secrets: its name is one of [`SECRET_FILES`] or [`SECRET_NAMES`], or one of the latter followed by `.` or `_` and more, or its extension is one of [`SECRET_EXTENSIONS`]; case is ignored.
fn secret(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    let named = SECRET_FILES.contains(&name.as_str())
        || SECRET_NAMES.iter().any(|secret| {
            name == *secret
                || name
                    .strip_prefix(secret)
                    .is_some_and(|rest| rest.starts_with(['.', '_']))
        });
    let extension = Path::new(&name)
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| SECRET_EXTENSIONS.contains(&extension));
    named || extension
}

/// The text of the file at `path`, when it can be read and is UTF-8 text without NUL bytes.
fn readable(path: &Path) -> Option<String> {
    let bytes = std::fs::read(path).ok()?;
    if bytes.contains(&0) {
        return None;
    }
    String::from_utf8(bytes).ok()
}

/// When the file described by `metadata` last changed, in nanoseconds since 1970, or 0 when that is unknown.
fn modified_nanos(metadata: &std::fs::Metadata) -> i64 {
    metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |since| since.as_nanos() as i64)
}

/// The 64-bit FNV-1a hash of `bytes`.
fn fnv(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

/// The bytes sqlite-vec reads a vector from: each number as a little-endian 32-bit float.
fn bytes_of(vector: &[f32]) -> Vec<u8> {
    vector
        .iter()
        .flat_map(|value| value.to_le_bytes())
        .collect()
}

#[cfg(test)]
/// Tests for building and searching the index.
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// An embedder for tests: each word of a text, lowercased, adds one to a slot picked by its hash, so texts that share words point the same way.
    struct Words {
        /// How many texts it embedded.
        calls: AtomicUsize,
    }

    impl Words {
        /// A fresh embedder.
        fn new() -> Self {
            Self {
                calls: AtomicUsize::new(0),
            }
        }
    }

    impl Embed for Words {
        /// Names the test model.
        fn model(&self) -> &str {
            "words"
        }

        /// Uses 64 slots.
        fn dimensions(&self) -> usize {
            64
        }

        /// Counts words into slots and scales each vector to length one.
        fn embed(&self, texts: &[String]) -> Vec<Vec<f32>> {
            self.calls.fetch_add(texts.len(), Ordering::SeqCst);
            texts
                .iter()
                .map(|text| {
                    let mut vector = vec![0.0f32; 64];
                    for word in text.split(|c: char| !c.is_alphanumeric()) {
                        if !word.is_empty() {
                            vector[(fnv(word.to_lowercase().as_bytes()) % 64) as usize] += 1.0;
                        }
                    }
                    let norm = vector.iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-6);
                    vector.iter().map(|x| x / norm).collect()
                })
                .collect()
        }
    }

    /// An empty project folder named `name` and the database beside it, under the workspace's git-ignored `build/tests/omc-index`.
    fn project(name: &str) -> (PathBuf, PathBuf) {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../build/tests/omc-index")
            .join(name);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("project/src")).unwrap();
        let root = dir.join("project").canonicalize().unwrap();
        (root, dir.join("index.sqlite3"))
    }

    /// Writes `text` to `relative` under `root`, making its folders.
    fn put(root: &Path, relative: &str, text: &str) {
        let path = root.join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    /// The paths of `hits`, in order.
    fn paths(hits: &[Hit]) -> Vec<&str> {
        hits.iter().map(|hit| hit.path.as_str()).collect()
    }

    /// Keyword search finds chunks by their words and identifier pieces, honours `.gitignore` and the path filter, and gives line ranges.
    #[test]
    fn keyword_search_without_vectors() {
        let (root, database) = project("index-keywords");
        put(
            &root,
            "src/config.rs",
            "/// Reads the settings.\nfn loadConfigFile(path: &str) -> String {\n    std::fs::read_to_string(path).unwrap()\n}\n",
        );
        put(&root, "src/net.rs", "fn open_socket() {}\n");
        put(
            &root,
            "docs/guide.md",
            "# Guide\n\nHow to load the config file.\n",
        );
        put(&root, "gen/config.rs", "fn loadConfigFile() {}\n");
        put(&root, ".gitignore", "gen/\n");
        put(&root, "Cargo.lock", "config config config\n");
        let mut index = Index::open(&database, &root).unwrap();
        let refreshed = index.refresh(None).unwrap();
        assert_eq!(refreshed.files, 4);
        let hits = index.search("config file", None, None, 10).unwrap();
        let mut found = paths(&hits);
        found.sort_unstable();
        assert_eq!(found, ["docs/guide.md", "src/config.rs"]);
        let named = index.search("loadConfigFile", None, None, 10).unwrap();
        assert_eq!(paths(&named)[0], "src/config.rs");
        assert_eq!((named[0].start, named[0].end), (1, 4));
        let only_docs = index.search("config", None, Some("*.md"), 10).unwrap();
        assert_eq!(paths(&only_docs), ["docs/guide.md"]);
        let folder = index.search("config", None, Some("src"), 10).unwrap();
        assert_eq!(paths(&folder), ["src/config.rs"]);
        assert!(index.search("the of", None, None, 10).unwrap().is_empty());
        assert!(matches!(
            index.search("x", None, Some("[z"), 10),
            Err(IndexError::Glob(_))
        ));
    }

    /// Only changed files are cut and embedded again, removed files leave the index, and a reopened index keeps its vectors.
    #[test]
    fn refreshes_incrementally() {
        let (root, database) = project("index-incremental");
        put(&root, "src/a.rs", "fn alpha() {}\n");
        put(&root, "src/b.rs", "fn beta() {}\n");
        let words = Words::new();
        let mut index = Index::open(&database, &root).unwrap();
        let first = index.refresh(Some(&words)).unwrap();
        assert_eq!((first.files, first.changed, first.embedded), (2, 2, 2));
        let again = index.refresh(Some(&words)).unwrap();
        assert_eq!((again.changed, again.embedded, again.removed), (0, 0, 0));
        put(&root, "src/a.rs", "fn alpha_changed() {}\n");
        std::fs::remove_file(root.join("src/b.rs")).unwrap();
        put(&root, "src/c.rs", "fn gamma() {}\n");
        let changed = index.refresh(Some(&words)).unwrap();
        assert_eq!(
            (
                changed.files,
                changed.changed,
                changed.removed,
                changed.embedded
            ),
            (2, 2, 1, 2)
        );
        assert!(index.search("beta", None, None, 10).unwrap().is_empty());
        drop(index);
        let mut reopened = Index::open(&database, &root).unwrap();
        assert!(reopened.has_vectors_of(&words).unwrap());
        assert_eq!(reopened.refresh(Some(&words)).unwrap().embedded, 0);
        assert_eq!(words.calls.load(Ordering::SeqCst), 4);
    }

    /// Vector search finds a chunk that shares no keyword term with the query, and the filtered and unfiltered nearest-neighbour queries agree.
    #[test]
    fn vectors_find_what_words_miss() {
        let (root, database) = project("index-vectors");
        put(
            &root,
            "src/a.rs",
            "fn retry_with_backoff() { sleep(delay); }\n",
        );
        put(&root, "src/b.rs", "fn render_page() { html(body); }\n");
        let words = Words::new();
        let mut index = Index::open(&database, &root).unwrap();
        index.refresh(Some(&words)).unwrap();
        let vector = words.embed(&["src/a.rs\nsleep delay".to_owned()]).remove(0);
        let hits = index.search("zzz", Some(&vector), None, 1).unwrap();
        assert_eq!(paths(&hits), ["src/a.rs"]);
        let filtered = index
            .search("zzz", Some(&vector), Some("src/*.rs"), 2)
            .unwrap();
        assert_eq!(paths(&filtered), ["src/a.rs", "src/b.rs"]);
        let keyword_only = index.search("render", None, None, 5).unwrap();
        assert_eq!(paths(&keyword_only), ["src/b.rs"]);
    }

    /// Indexing first without a model and then with one embeds what was already cut; a different model replaces every vector.
    #[test]
    fn embeds_later_and_swaps_models() {
        let (root, database) = project("index-later");
        put(&root, "src/a.rs", "fn alpha() {}\n");
        let mut index = Index::open(&database, &root).unwrap();
        assert_eq!(index.refresh(None).unwrap().embedded, 0);
        let words = Words::new();
        assert!(!index.has_vectors_of(&words).unwrap());
        assert_eq!(index.refresh(Some(&words)).unwrap().embedded, 1);

        /// The same embedder under another name.
        struct Other(Words);
        impl Embed for Other {
            /// Names the other model.
            fn model(&self) -> &str {
                "other"
            }
            /// Uses as many slots as [`Words`].
            fn dimensions(&self) -> usize {
                64
            }
            /// Embeds like [`Words`].
            fn embed(&self, texts: &[String]) -> Vec<Vec<f32>> {
                self.0.embed(texts)
            }
        }
        let other = Other(Words::new());
        assert_eq!(index.refresh(Some(&other)).unwrap().embedded, 1);
        assert!(!index.has_vectors_of(&words).unwrap());
    }

    /// Files that commonly hold secrets are never indexed, and one indexed before is dropped; look-alike names are kept.
    #[test]
    fn leaves_secret_files_out() {
        for name in [
            ".env",
            ".env.local",
            ".ENV.production",
            "id_rsa",
            "id_ed25519.pub",
            "server.pem",
            "tls.key",
            "cert.P12",
            ".npmrc",
            "credentials",
            "prod.tfvars",
        ] {
            assert!(secret(name), "{name}");
        }
        for name in [
            "environment.rs",
            "keys.rs",
            "envelope.ts",
            "main.rs",
            "credentials.rs",
        ] {
            assert!(!secret(name), "{name}");
        }
        let (root, database) = project("index-secrets");
        put(&root, ".env", "API_TOKEN=hunter2\n");
        put(
            &root,
            "certs/server.pem",
            "-----BEGIN PRIVATE KEY----- hunter2\n",
        );
        put(
            &root,
            "src/app.rs",
            "fn token() -> &'static str { \"none\" }\n",
        );
        let mut index = Index::open(&database, &root).unwrap();
        assert_eq!(index.refresh(None).unwrap().files, 1);
        assert!(index.search("hunter2", None, None, 10).unwrap().is_empty());
        assert_eq!(
            paths(&index.search("token", None, None, 10).unwrap()),
            ["src/app.rs"]
        );
    }

    /// Each project gets a database of its own, named after its folder.
    #[test]
    fn locations_differ_by_project() {
        let a = Index::location(Path::new("/c"), Path::new("/work/app"));
        let b = Index::location(Path::new("/c"), Path::new("/other/app"));
        assert_ne!(a, b);
        assert!(a.starts_with("/c/projects"));
        assert!(a.file_name().unwrap().to_string_lossy().starts_with("app-"));
    }
}
