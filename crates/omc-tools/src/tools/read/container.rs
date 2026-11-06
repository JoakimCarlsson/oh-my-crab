//! Files `read` looks inside rather than reading as text: SQLite databases,
//! zip and tar archives, and JSON files queried with a jq filter.
//!
//! The part of a `read` path after the file names what to read inside it:
//! `app.db:users`, `src.zip:lib/a.rs:10-20`, `data.json:.items[0]`. Such a
//! path names no file of its own, so it is recognized here, before the usual
//! path and line-selector handling, by finding the file the path starts with.

use std::io::Read as _;
use std::path::{Path, PathBuf};

use super::selector::Selector;
use super::{archive, json, sqlite};
use crate::{Sandbox, ToolError, ToolOutput};

/// Bytes read from the start of a file to tell its kind.
const SNIFF_BYTES: u64 = 512;

/// The first bytes of every SQLite 3 database.
const SQLITE_MAGIC: &[u8] = b"SQLite format 3\0";

/// A kind of file whose contents `read` addresses with a suffix.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Kind {
    /// A SQLite database.
    Sqlite,
    /// An archive in one of the formats [`archive::Format`] names.
    Archive(archive::Format),
    /// A JSON document, queried with a jq filter.
    Json,
}

/// A read inside a container file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Target {
    /// The container file.
    pub(super) path: PathBuf,
    /// The name the container is shown by, as the caller wrote it.
    pub(super) label: String,
    /// What kind of container it is.
    pub(super) kind: Kind,
    /// What to read inside it, the text after the container's name and its
    /// `:`; empty for the container as a whole.
    pub(super) inner: String,
}

/// Finds the container file the `read` path `raw` names, and what it asks for
/// inside it.
///
/// A path that names an existing file is a container read when that file is
/// a database or an archive. A path that names no file is one when the part
/// before one of its colons is such a file, or a JSON file followed by
/// something other than a line selector. Returns `None` for every other path,
/// which `read` then handles as usual.
pub(super) fn locate(sandbox: &Sandbox, raw: &str) -> Option<Target> {
    if let Ok(whole) = sandbox.resolve_readable(raw)
        && whole.exists()
    {
        return match whole.is_file().then(|| kind(&whole)).flatten()? {
            Kind::Json => None,
            kind => Some(Target {
                path: whole,
                label: raw.to_string(),
                kind,
                inner: String::new(),
            }),
        };
    }
    raw.match_indices(':').find_map(|(colon, _)| {
        let (name, inner) = (&raw[..colon], &raw[colon + 1..]);
        if name.is_empty() {
            return None;
        }
        let path = sandbox.resolve_readable(name).ok()?;
        if !path.is_file() {
            return None;
        }
        let kind = kind(&path)?;
        if kind == Kind::Json && Selector::parse(inner).is_some() {
            return None;
        }
        Some(Target {
            path,
            label: name.to_string(),
            kind,
            inner: inner.to_string(),
        })
    })
}

/// Tells what kind of container the file at `path` is from its first bytes
/// and, for JSON and old tar files that carry no magic bytes, its name.
fn kind(path: &Path) -> Option<Kind> {
    let mut head = Vec::with_capacity(SNIFF_BYTES as usize);
    std::fs::File::open(path)
        .ok()?
        .take(SNIFF_BYTES)
        .read_to_end(&mut head)
        .ok()?;
    if head.starts_with(SQLITE_MAGIC) {
        return Some(Kind::Sqlite);
    }
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    if let Some(format) = archive::sniff(&head, &name) {
        return Some(Kind::Archive(format));
    }
    name.ends_with(".json").then_some(Kind::Json)
}

/// Reads what `target` asks for, paging with the legacy `offset` and `limit`
/// arguments when they are given.
///
/// # Errors
///
/// Returns [`ToolError`] when the container cannot be opened or read, what
/// it asks for does not exist inside it, or the request is malformed.
pub(super) fn read(
    target: &Target,
    offset: Option<usize>,
    limit: Option<usize>,
) -> Result<ToolOutput, ToolError> {
    match target.kind {
        Kind::Sqlite => sqlite::read(target, offset, limit),
        Kind::Archive(format) => archive::read(target, format, offset, limit),
        Kind::Json => json::read(target, offset, limit),
    }
}

/// Longest a read inside a container may take before the call gives up on it.
const READ_DEADLINE: std::time::Duration = std::time::Duration::from_secs(20);

/// Stack given to the thread a container read runs on, deep enough for the
/// recursion a jq filter can nest into.
const READ_STACK_BYTES: usize = 64 << 20;

/// Reads what `target` asks for, as [`read`] does, giving up after
/// [`READ_DEADLINE`]; see [`read_within`].
///
/// # Errors
///
/// Returns what [`read_within`] returns.
pub(super) async fn read_bounded(
    target: Target,
    offset: Option<usize>,
    limit: Option<usize>,
) -> Result<ToolOutput, ToolError> {
    read_within(target, offset, limit, READ_DEADLINE).await
}

/// Reads what `target` asks for, as [`read`] does, on a thread of its own
/// with a large stack, and gives up once `deadline` has passed.
///
/// A jq filter cannot be interrupted, so one that never ends keeps its
/// thread busy after the deadline; the call still returns and the turn goes
/// on.
///
/// # Errors
///
/// Returns what [`read`] returns, and [`ToolError::Failed`] when the thread
/// cannot be started, ends without an answer, or the deadline passes.
async fn read_within(
    target: Target,
    offset: Option<usize>,
    limit: Option<usize>,
    deadline: std::time::Duration,
) -> Result<ToolOutput, ToolError> {
    let (sender, receiver) = tokio::sync::oneshot::channel();
    let label = target.label.clone();
    std::thread::Builder::new()
        .name("read-container".to_string())
        .stack_size(READ_STACK_BYTES)
        .spawn(move || {
            let _ = sender.send(read(&target, offset, limit));
        })
        .map_err(|error| ToolError::Failed(format!("cannot read {label}: {error}")))?;
    match tokio::time::timeout(deadline, receiver).await {
        Ok(Ok(result)) => result,
        Ok(Err(_)) => Err(ToolError::Failed(format!(
            "reading {label} stopped without a result"
        ))),
        Err(_) => Err(ToolError::Failed(format!(
            "reading {label} took longer than {} seconds and was abandoned; narrow the query",
            deadline.as_secs()
        ))),
    }
}

/// Splits a trailing line selector such as `:50-100` off `inner`, folding in
/// `offset` and `limit`; `inner` stays whole when it ends in no selector.
///
/// # Errors
///
/// Returns [`ToolError::InvalidArguments`] when a suffix and `offset` or
/// `limit` are both given, or the suffix is `raw`.
pub(super) fn split_window(
    inner: &str,
    offset: Option<usize>,
    limit: Option<usize>,
) -> Result<(&str, Selector), ToolError> {
    let (rest, selector) = inner
        .rsplit_once(':')
        .and_then(|(rest, suffix)| Some((rest, Selector::parse(suffix)?)))
        .unwrap_or((inner, Selector::default()));
    Ok((rest, window(selector, offset, limit)?))
}

/// Folds `offset` and `limit` into `selector`, refusing `raw`, which applies
/// to URLs only.
///
/// # Errors
///
/// Returns [`ToolError::InvalidArguments`] when the selector is `raw` or
/// already names lines and `offset` or `limit` is given too.
pub(super) fn window(
    selector: Selector,
    offset: Option<usize>,
    limit: Option<usize>,
) -> Result<Selector, ToolError> {
    if selector.raw {
        return Err(ToolError::InvalidArguments(
            "the raw selector applies to URLs, not files".to_string(),
        ));
    }
    selector.windowed(offset, limit)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::Tool as _;
    use crate::testutil::scratch;

    /// Builds a `read` tool rooted at the scratch directory `name`.
    fn tool(name: &str) -> (super::super::Read, PathBuf) {
        let dir = scratch(name);
        (super::super::Read::new(Sandbox::new(&dir).unwrap()), dir)
    }

    /// Reads `path` with `tool`, with the arguments in `extra` merged in.
    async fn call(
        tool: &super::super::Read,
        path: &str,
        extra: serde_json::Value,
    ) -> Result<String, ToolError> {
        let mut args = json!({ "path": path });
        if let (Some(args), Some(extra)) = (args.as_object_mut(), extra.as_object()) {
            args.extend(extra.clone());
        }
        tool.call(args).await.map(|output| output.text)
    }

    /// A jq filter that never ends is given up on at the deadline instead of holding the call.
    #[tokio::test]
    async fn an_endless_query_is_abandoned() {
        let dir = scratch("read-endless-json");
        std::fs::write(dir.join("data.json"), "[1]").unwrap();
        let sandbox = Sandbox::new(&dir).unwrap();
        let target = locate(&sandbox, "data.json:last(repeat(1))").unwrap();
        let started = std::time::Instant::now();
        let error = read_within(target, None, None, std::time::Duration::from_secs(1))
            .await
            .unwrap_err();
        assert!(started.elapsed() < std::time::Duration::from_secs(5));
        assert!(error.to_string().contains("was abandoned"), "{error}");
    }

    /// A database lists its tables, pages a table's rows, runs read-only
    /// queries and refuses statements that write.
    #[tokio::test]
    async fn reads_sqlite_databases() {
        let (tool, dir) = tool("read-sqlite");
        sqlite::tests::database(&dir.join("app.db"), 120);
        let read = |path: &'static str| call(&tool, path, json!({}));
        let list = read("app.db").await.unwrap();
        assert!(
            list.starts_with("[sqlite app.db, 2 tables, 1 views;"),
            "{list}"
        );
        assert!(
            list.contains("users: 120 rows\n  CREATE TABLE users"),
            "{list}"
        );
        assert!(list.contains("names: view\n"), "{list}");
        let page = read("app.db:users").await.unwrap();
        assert!(
            page.starts_with("[sqlite app.db, table users, 120 rows]\nCREATE TABLE"),
            "{page}"
        );
        assert!(
            page.contains("      \tid | name | bio\n     1\t1 | user1 | <blob 3 B>\n"),
            "{page}"
        );
        assert!(page.contains("    50\t50 | user50"), "{page}");
        assert!(!page.contains("user51"), "{page}");
        assert!(
            page.ends_with("[more rows remain; continue with app.db:users:51]\n"),
            "{page}"
        );
        let tail = read("app.db:users:-2").await.unwrap();
        assert!(tail.contains("   119\t119 |"), "{tail}");
        assert!(
            tail.ends_with("   120\t120 | user120 | <blob 3 B>\n"),
            "{tail}"
        );
        let window = call(&tool, "app.db:users", json!({ "offset": 10, "limit": 2 }))
            .await
            .unwrap();
        assert!(
            window.contains("    10\t10 |") && window.contains("    11\t11 |"),
            "{window}"
        );
        assert!(
            !window.contains("    12\t") && !window.contains("more rows"),
            "{window}"
        );
        let notes = read("app.db:notes").await.unwrap();
        assert!(notes.contains("     1\tline one\\nline two\n"), "{notes}");
        assert_eq!(
            read("app.db:SELECT name FROM users WHERE id > 117 ORDER BY id")
                .await
                .unwrap(),
            "[sqlite app.db, query]\n      \tname\n     1\tuser118\n     2\tuser119\n     3\tuser120\n"
        );
        let paged = read("app.db:SELECT id FROM users:3-4").await.unwrap();
        assert!(
            paged.ends_with("      \tid\n     3\t3\n     4\t4\n"),
            "{paged}"
        );
        for writes in [
            "app.db:DELETE FROM users",
            "app.db:WITH x AS (SELECT 1) DELETE FROM users",
            "app.db:SELECT 1; DELETE FROM users",
            "app.db:PRAGMA journal_mode=delete",
        ] {
            assert!(read(writes).await.is_err(), "{writes}");
        }
        assert!(read("app.db:users").await.unwrap().contains("120 rows"));
        let missing = read("app.db:nope").await.unwrap_err().to_string();
        assert!(
            missing.contains("has no table or view nope; it has: names, notes, users"),
            "{missing}"
        );
        let info = read("app.db:PRAGMA table_info(users)").await.unwrap();
        assert!(info.contains("name | type"), "{info}");
    }

    /// Archives list their members, read text members numbered without a
    /// tag, report binary members, and list directories inside them.
    #[tokio::test]
    async fn reads_archives() {
        let (tool, dir) = tool("read-archive");
        let members: &[(&str, &[u8])] = &[
            ("src/", b""),
            ("src/a.rs", b"fn a() {}\nfn b() {}\nfn c() {}\n"),
            ("blob.bin", b"\0\x01\x02binary"),
            ("docs/readme.md", b"# Title\n"),
        ];
        std::fs::write(dir.join("code.zip"), archive::tests::zip(members)).unwrap();
        let files = &members[1..];
        std::fs::write(dir.join("code.tar"), archive::tests::tar(files, false)).unwrap();
        std::fs::write(dir.join("code.tgz"), archive::tests::tar(files, true)).unwrap();
        let read = |path: String| {
            let tool = &tool;
            async move { call(tool, &path, json!({})).await }
        };
        let list = read("code.zip".into()).await.unwrap();
        assert!(
            list.starts_with("[archive code.zip, zip, 3 files of 4 entries, 47 B unpacked;"),
            "{list}"
        );
        assert!(list.contains("\t        -  src/\n"), "{list}");
        assert!(list.contains("\t     30 B  src/a.rs\n"), "{list}");
        for name in ["code.zip", "code.tar", "code.tgz"] {
            let text = read(format!("{name}:src/a.rs")).await.unwrap();
            assert_eq!(
                text,
                format!(
                    "[{name}:src/a.rs, 30 B; inside an archive, read-only: no tag, cannot be \
                     edited]\n     1\tfn a() {{}}\n     2\tfn b() {{}}\n     3\tfn c() {{}}\n"
                ),
                "{name}"
            );
            let line = read(format!("{name}:./src/a.rs:2-2")).await.unwrap();
            assert!(line.ends_with("]\n     2\tfn b() {}\n"), "{name}: {line}");
            let binary = read(format!("{name}:blob.bin")).await.unwrap();
            assert!(binary.contains("is binary, 9 B"), "{name}: {binary}");
            let sub = read(format!("{name}:docs")).await.unwrap();
            assert!(sub.contains("docs/readme.md"), "{name}: {sub}");
            assert!(!sub.contains("src/a.rs"), "{name}: {sub}");
            let missing = read(format!("{name}:nope.txt"))
                .await
                .unwrap_err()
                .to_string();
            assert!(
                missing.contains("has no member nope.txt"),
                "{name}: {missing}"
            );
        }
        let window = call(&tool, "code.zip", json!({ "offset": 2, "limit": 1 }))
            .await
            .unwrap();
        assert!(
            window.ends_with("     2\t     30 B  src/a.rs\n"),
            "{window}"
        );
    }

    /// A JSON file with a jq filter gives the filtered values, numbered and
    /// pageable; with a line selector it reads as text.
    #[tokio::test]
    async fn queries_json_files() {
        let (tool, dir) = tool("read-json");
        std::fs::write(
            dir.join("data.json"),
            r#"{"items":[{"n":"a"},{"n":"b"},{"n":"c"}],"meta":{"count":3}}"#,
        )
        .unwrap();
        let read = |path: &'static str| call(&tool, path, json!({}));
        assert_eq!(
            read("data.json:.items[1]").await.unwrap(),
            "[json data.json, query .items[1], 1 result]\n     1\t{\n     2\t  \"n\": \"b\"\n     3\t}\n"
        );
        assert_eq!(
            read("data.json:.items[].n:2-3").await.unwrap(),
            "[json data.json, query .items[].n, 3 results]\n     2\t\"b\"\n     3\t\"c\"\n"
        );
        let keys = read("data.json:.meta | keys").await.unwrap();
        assert!(keys.contains("\"count\""), "{keys}");
        assert!(read("data.json:1").await.unwrap().starts_with("[tag "));
        let bad = read("data.json:.items[").await.unwrap_err();
        assert!(matches!(bad, ToolError::InvalidArguments(_)), "{bad}");
    }

    /// Container paths are found by the file they start with; other paths,
    /// and line selectors on JSON files, are left to the usual handling.
    #[test]
    fn locates_containers() {
        let dir = scratch("read-container-locate");
        let sandbox = Sandbox::new(&dir).unwrap();
        std::fs::write(dir.join("data.json"), "{}").unwrap();
        std::fs::write(dir.join("db"), [SQLITE_MAGIC, &[0u8; 84]].concat()).unwrap();
        std::fs::write(dir.join("notes.txt"), "a:b").unwrap();
        let found = |raw: &str| locate(&sandbox, raw).map(|t| (t.label, t.kind, t.inner));
        assert_eq!(
            found("data.json:.a:b"),
            Some(("data.json".into(), Kind::Json, ".a:b".into()))
        );
        assert_eq!(found("data.json"), None);
        assert_eq!(found("data.json:5-9"), None);
        assert_eq!(
            found("db"),
            Some(("db".into(), Kind::Sqlite, String::new()))
        );
        assert_eq!(
            found("db:SELECT 1"),
            Some(("db".into(), Kind::Sqlite, "SELECT 1".into()))
        );
        assert_eq!(found("notes.txt:3"), None);
        assert_eq!(found("missing.zip:a"), None);
    }

    /// A trailing selector splits off; anything else stays part of the name.
    #[test]
    fn splits_trailing_windows() {
        let (rest, selector) = split_window("dir/a.rs:5-9", None, None).unwrap();
        assert_eq!((rest, selector.spans.len()), ("dir/a.rs", 1));
        let (rest, selector) = split_window(".[1:3]", None, None).unwrap();
        assert_eq!((rest, selector.spans.len()), (".[1:3]", 0));
        let (_, selector) = split_window("a", Some(4), Some(2)).unwrap();
        assert_eq!(selector.spans.len(), 1);
        assert!(split_window("a:2", Some(4), None).is_err());
        assert!(split_window("a:raw", None, None).is_err());
    }
}
