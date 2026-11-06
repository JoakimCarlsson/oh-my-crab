//! Reading SQLite databases: `app.db` lists the tables with their row counts
//! and schemas, `app.db:users` shows a table's rows, and
//! `app.db:SELECT ...` runs a read-only query.
//!
//! Rows are numbered from 1 and paged with the usual line selectors, so
//! `app.db:users:51-100` shows rows 51 to 100. The database is opened
//! read-only with writes refused, and only `SELECT`, `WITH` and
//! `PRAGMA table_info` statements run.

use std::time::{Duration, Instant};

use rusqlite::types::ValueRef;
use rusqlite::{Connection, OpenFlags, Statement};

use super::container::{self, Target};
use super::selector::{self, Range, Selector, Span};
use super::{lines, media};
use crate::{ToolError, ToolOutput};

/// Rows shown when a read names no last row.
const PAGE_ROWS: usize = 50;

/// Most rows one read shows.
const MAX_ROWS: usize = 500;

/// Longest cell shown, in characters; longer values are cut.
const MAX_CELL_CHARS: usize = 200;

/// Rows counted for a table in the table list before it is called larger.
const COUNT_CAP: usize = 1_000_000;

/// Longest a query may run before it is interrupted.
const QUERY_TIME: Duration = Duration::from_secs(10);

/// A table or view of the database.
struct Table {
    /// Its name.
    name: String,
    /// Whether it is a view rather than a table.
    view: bool,
    /// The statement that created it.
    sql: String,
}

/// Reads what `target` asks for in the SQLite database it points at: the
/// table list, a page of one table's rows, or a page of a query's rows.
///
/// # Errors
///
/// Returns [`ToolError::InvalidArguments`] for a statement that is not a
/// read-only query, or a selector that does not apply, and
/// [`ToolError::Failed`] when the database cannot be opened, no table has
/// the name given, or the query fails.
pub(super) fn read(
    target: &Target,
    offset: Option<usize>,
    limit: Option<usize>,
) -> Result<ToolOutput, ToolError> {
    let label = &target.label;
    let mut conn = open(target).map_err(|error| failed(label, error))?;
    let tables = tables(&conn).map_err(|error| failed(label, error))?;
    if target.inner.trim().is_empty() {
        if offset.is_some() || limit.is_some() {
            return Err(ToolError::InvalidArguments(format!(
                "{label} is a SQLite database; offset and limit page a table or query, \
                 such as {label}:<table>"
            )));
        }
        return Ok(ToolOutput::new(table_list(&conn, label, &tables)));
    }
    let (name, selector) = container::split_window(&target.inner, offset, limit)?;
    let name = name.trim();
    if let Some(table) = tables.iter().find(|table| table.name == name) {
        return table_rows(&conn, label, table, &selector);
    }
    if is_query(name) {
        check_query(name)?;
        return query_rows(&mut conn, label, name, &selector);
    }
    let names: Vec<&str> = tables.iter().map(|table| table.name.as_str()).collect();
    Err(ToolError::Failed(format!(
        "{label} has no table or view {name}; it has: {}. A query starts with SELECT or WITH",
        if names.is_empty() {
            "none".to_string()
        } else {
            names.join(", ")
        }
    )))
}

/// Builds the error for a database operation on `label` that failed.
fn failed(label: &str, error: rusqlite::Error) -> ToolError {
    if let rusqlite::Error::SqliteFailure(code, _) = &error
        && code.code == rusqlite::ErrorCode::OperationInterrupted
    {
        return ToolError::Failed(format!(
            "the query on {label} ran past {} s and was stopped",
            QUERY_TIME.as_secs()
        ));
    }
    ToolError::Failed(format!("SQLite error on {label}: {error}"))
}

/// Opens the database `target` points at read-only, with writes refused even
/// to temporary tables.
///
/// # Errors
///
/// Returns the SQLite error when the file cannot be opened as a database.
fn open(target: &Target) -> rusqlite::Result<Connection> {
    let conn = Connection::open_with_flags(
        &target.path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    conn.busy_timeout(Duration::from_secs(2))?;
    conn.pragma_update(None, "query_only", true)?;
    Ok(conn)
}

/// Lists the tables and views of `conn`, by name, leaving out SQLite's own.
///
/// # Errors
///
/// Returns the SQLite error when the schema cannot be read.
fn tables(conn: &Connection) -> rusqlite::Result<Vec<Table>> {
    let mut statement = conn.prepare(
        "SELECT name, type, coalesce(sql, '') FROM sqlite_master \
         WHERE type IN ('table', 'view') AND name NOT LIKE 'sqlite\\_%' ESCAPE '\\' \
         ORDER BY name COLLATE NOCASE",
    )?;
    statement
        .query_map([], |row| {
            Ok(Table {
                name: row.get(0)?,
                view: row.get::<_, String>(1)? == "view",
                sql: row.get(2)?,
            })
        })?
        .collect()
}

/// Quotes `name` as an SQL identifier.
fn quote(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

/// Counts the rows of the table `name`, stopping past `cap` when one is
/// given; returns `None` when the count fails, as it can for virtual tables.
fn count(conn: &Connection, name: &str, cap: Option<usize>) -> Option<usize> {
    let sql = match cap {
        Some(cap) => format!(
            "SELECT COUNT(*) FROM (SELECT 1 FROM {} LIMIT {})",
            quote(name),
            cap + 1
        ),
        None => format!("SELECT COUNT(*) FROM {}", quote(name)),
    };
    conn.query_row(&sql, [], |row| row.get::<_, i64>(0))
        .ok()
        .and_then(|count| usize::try_from(count).ok())
}

/// Says how many rows a table has, such as `1 row` or `12 rows`.
fn rows(count: usize) -> String {
    if count == 1 {
        "1 row".to_string()
    } else {
        format!("{count} rows")
    }
}

/// Lists `tables` of the database `conn`, called `label`, with their row
/// counts and the statements that created them.
fn table_list(conn: &Connection, label: &str, tables: &[Table]) -> String {
    let views = tables.iter().filter(|table| table.view).count();
    let mut out = format!(
        "[sqlite {label}, {} tables, {views} views; read {label}:<table> for rows or \
         {label}:SELECT ... for a read-only query]\n",
        tables.len() - views
    );
    if tables.is_empty() {
        out.push_str("(no tables)\n");
    }
    for table in tables {
        let size = if table.view {
            "view".to_string()
        } else {
            match count(conn, &table.name, Some(COUNT_CAP)) {
                Some(n) if n > COUNT_CAP => format!("over {}", rows(COUNT_CAP)),
                Some(n) => rows(n),
                None => "rows not countable".to_string(),
            }
        };
        out.push_str(&format!("{}: {size}\n", table.name));
        for line in table.sql.lines() {
            out.push_str(&format!("  {}\n", line.trim_end()));
        }
    }
    out
}

/// Fits `selector` to a result of `total` rows, when known, as the first row
/// to show, 1-indexed, and the last, if it names one.
///
/// # Errors
///
/// Returns [`ToolError::InvalidArguments`] when the selector has several
/// ranges, or counts from the end of a result whose length is unknown.
fn page(selector: &Selector, total: Option<usize>) -> Result<Range, ToolError> {
    if selector.spans.len() > 1 {
        return Err(ToolError::InvalidArguments(
            "rows take one range, such as :51-100".to_string(),
        ));
    }
    if total.is_none() && selector.needs_total() {
        return Err(ToolError::InvalidArguments(
            "counting rows from the end applies to tables, not queries".to_string(),
        ));
    }
    Ok(match selector.spans.first() {
        None => (1, None),
        Some(Span::From(first)) => (*first, None),
        Some(_) => selector
            .ranges(total)
            .first()
            .copied()
            .unwrap_or((1, Some(0))),
    })
}

/// Shows the rows of `table` in the database `conn`, called `label`, that
/// `selector` picks, under its schema.
///
/// # Errors
///
/// Returns [`ToolError`] when the selector does not apply or the rows cannot
/// be read.
fn table_rows(
    conn: &Connection,
    label: &str,
    table: &Table,
    selector: &Selector,
) -> Result<ToolOutput, ToolError> {
    let total = count(conn, &table.name, None);
    let (first, last) = page(selector, total.or(Some(0)))?;
    let shown = format!("{label}:{}", table.name);
    let mut out = format!(
        "[sqlite {label}, {} {}, {}]\n",
        if table.view { "view" } else { "table" },
        table.name,
        total.map_or_else(|| "rows not countable".to_string(), rows)
    );
    for line in table.sql.lines() {
        out.push_str(&format!("{}\n", line.trim_end()));
    }
    let sql = format!(
        "SELECT * FROM {} LIMIT -1 OFFSET {}",
        quote(&table.name),
        first.saturating_sub(1)
    );
    let mut statement = conn.prepare(&sql).map_err(|error| failed(label, error))?;
    let body = render(&mut statement, &shown, (first, last), total, first - 1)
        .map_err(|error| failed(label, error))?;
    out.push_str(&body);
    Ok(ToolOutput::new(out))
}

/// Whether `text` reads as an SQL statement rather than a table name.
fn is_query(text: &str) -> bool {
    let word: String = text
        .chars()
        .take_while(char::is_ascii_alphabetic)
        .collect::<String>()
        .to_ascii_lowercase();
    matches!(word.as_str(), "select" | "with" | "pragma" | "values")
        || text.contains(char::is_whitespace)
}

/// Checks that `sql` is a statement `read` runs: a `SELECT`, a `WITH` query
/// or `PRAGMA table_info`.
///
/// # Errors
///
/// Returns [`ToolError::InvalidArguments`] for any other statement.
fn check_query(sql: &str) -> Result<(), ToolError> {
    let lower = sql.trim_start().to_ascii_lowercase();
    let mut words = lower.split(|c: char| !c.is_ascii_alphanumeric() && c != '_');
    let allowed = match words.next() {
        Some("select" | "with") => true,
        Some("pragma") => {
            let mut rest = words.filter(|word| !word.is_empty());
            match rest.next() {
                Some("table_info") => true,
                Some("main") => rest.next() == Some("table_info"),
                _ => false,
            }
        }
        _ => false,
    };
    if allowed {
        return Ok(());
    }
    Err(ToolError::InvalidArguments(
        "read runs read-only queries only: SELECT, WITH ... SELECT, or PRAGMA table_info(<table>)"
            .to_string(),
    ))
}

/// Runs the read-only query `sql` on `conn`, called `label`, and shows the
/// rows `selector` picks, stopping it after [`QUERY_TIME`].
///
/// # Errors
///
/// Returns [`ToolError`] when the statement writes, does not prepare or
/// fails, or the selector does not apply.
fn query_rows(
    conn: &mut Connection,
    label: &str,
    sql: &str,
    selector: &Selector,
) -> Result<ToolOutput, ToolError> {
    let range = page(selector, None)?;
    let started = Instant::now();
    conn.progress_handler(10_000, Some(move || started.elapsed() > QUERY_TIME))
        .map_err(|error| failed(label, error))?;
    let mut statement = conn.prepare(sql).map_err(|error| failed(label, error))?;
    if !statement.readonly() && !sql.trim_start().to_ascii_lowercase().starts_with("pragma") {
        return Err(ToolError::InvalidArguments(
            "read runs read-only queries only; this statement would write".to_string(),
        ));
    }
    let shown = format!("{label}:{sql}");
    let body =
        render(&mut statement, &shown, range, None, 0).map_err(|error| failed(label, error))?;
    Ok(ToolOutput::new(format!("[sqlite {label}, query]\n{body}")))
}

/// Runs `statement` and renders the rows in `range` of its result, numbered
/// under an unnumbered line of column names.
///
/// The statement's first row is row `skipped + 1` of the result, as when an
/// `OFFSET` already skipped rows. `total` is the result's length when known;
/// a note at the end says how to continue when rows remain past those shown.
///
/// # Errors
///
/// Returns the SQLite error when the statement fails.
fn render(
    statement: &mut Statement<'_>,
    shown: &str,
    (first, last): Range,
    total: Option<usize>,
    skipped: usize,
) -> rusqlite::Result<String> {
    let columns: Vec<String> = statement.column_names().map(str::to_string).collect();
    let width = columns.len();
    let wanted = last.map_or(PAGE_ROWS, |last| {
        (last + 1).saturating_sub(first).min(MAX_ROWS)
    });
    let mut out = format!("      \t{}\n", columns.join(" | "));
    let mut result = statement.query([])?;
    let mut position = skipped;
    let mut shown_rows = 0usize;
    while let Some(row) = result.next()? {
        position += 1;
        if position < first {
            continue;
        }
        if shown_rows >= wanted
            || out.len() >= lines::MAX_BYTES
            || last.is_some_and(|last| position > last)
        {
            if last.is_none_or(|last| position <= last) {
                let rest = [(position, last)];
                out.push_str(&format!(
                    "[more rows remain; continue with {shown}:{}]\n",
                    selector::render(&rest)
                ));
            }
            return Ok(out);
        }
        let cells: Vec<String> = (0..width)
            .map(|index| row.get_ref(index).map(cell))
            .collect::<rusqlite::Result<_>>()?;
        out.push_str(&format!("{position:>6}\t{}\n", cells.join(" | ")));
        shown_rows += 1;
    }
    if shown_rows == 0 && wanted > 0 {
        let count = total.or((position > skipped).then_some(position));
        out.push_str(&match count {
            Some(0) => "(no rows)\n".to_string(),
            Some(count) => format!("[row {first} is past the end; there are {}]\n", rows(count)),
            None if position == 0 => "(no rows)\n".to_string(),
            None => format!("[row {first} is past the end]\n"),
        });
    }
    Ok(out)
}

/// Shows one value: `NULL`, a number, text on one line and cut to
/// [`MAX_CELL_CHARS`], or the size of a blob.
fn cell(value: ValueRef<'_>) -> String {
    match value {
        ValueRef::Null => "NULL".to_string(),
        ValueRef::Integer(n) => n.to_string(),
        ValueRef::Real(x) => x.to_string(),
        ValueRef::Text(bytes) => {
            let text = String::from_utf8_lossy(bytes)
                .replace('\\', "\\\\")
                .replace('\n', "\\n")
                .replace('\r', "\\r")
                .replace('\t', "\\t");
            match text.char_indices().nth(MAX_CELL_CHARS) {
                Some((end, _)) => format!("{}…", &text[..end]),
                None => text,
            }
        }
        ValueRef::Blob(bytes) => format!("<blob {}>", media::size(bytes.len())),
    }
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;

    /// Writes a database at `path` with a `users` table of `rows` rows, a
    /// `notes` table and a view.
    pub(in super::super) fn database(path: &std::path::Path, rows: usize) {
        let conn = Connection::open(path).unwrap();
        conn.execute_batch(
            "CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT, bio BLOB);
             CREATE TABLE notes (body TEXT);
             CREATE VIEW names AS SELECT name FROM users;
             INSERT INTO notes VALUES ('line one\nline two');",
        )
        .unwrap();
        for id in 1..=rows {
            conn.execute(
                "INSERT INTO users VALUES (?1, ?2, ?3)",
                rusqlite::params![id as i64, format!("user{id}"), vec![0u8; 3]],
            )
            .unwrap();
        }
    }

    /// Only read-only statements pass the check.
    #[test]
    fn accepts_only_read_only_statements() {
        for sql in [
            "SELECT 1",
            "  select * from t",
            "WITH x AS (SELECT 1) SELECT * FROM x",
            "PRAGMA table_info(users)",
            "pragma main.table_info('users')",
        ] {
            assert!(check_query(sql).is_ok(), "{sql}");
        }
        for sql in [
            "DELETE FROM users",
            "PRAGMA journal_mode=delete",
            "ATTACH 'x' AS y",
            "VACUUM",
        ] {
            assert!(check_query(sql).is_err(), "{sql}");
        }
    }

    /// Cells show on one line, with blobs and nulls named.
    #[test]
    fn renders_cells() {
        assert_eq!(cell(ValueRef::Null), "NULL");
        assert_eq!(cell(ValueRef::Text(b"a\nb")), "a\\nb");
        assert_eq!(cell(ValueRef::Blob(&[1, 2])), "<blob 2 B>");
        assert_eq!(cell(ValueRef::Real(1.5)), "1.5");
    }
}
