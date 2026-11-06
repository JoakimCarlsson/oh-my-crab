//! Querying a JSON file with a jq filter: `data.json:.items[0].name`.
//!
//! The filter runs on the parsed document and each value it yields is
//! pretty-printed; the result is then numbered and paged like any text, so
//! `data.json:.items:50-100` reads lines 50 to 100 of the filtered output.

use jaq_core::load::{Arena, File, Loader, lex, parse};
use jaq_core::{Compiler, Ctx, Vars, compile, data, load, unwrap_valr};
use jaq_json::Val;

use super::container::{self, Target};
use super::{lines, media};
use crate::{ToolError, ToolOutput};

/// Largest JSON file that is parsed for a query, in bytes.
const MAX_JSON_BYTES: usize = 64 << 20;

/// Most bytes of filtered output built before the query is stopped.
const MAX_OUTPUT_BYTES: usize = 4 << 20;

/// Runs the jq filter `target` names on the JSON file it points at and
/// returns the values it yields, pretty-printed and numbered.
///
/// # Errors
///
/// Returns [`ToolError::InvalidArguments`] when the filter does not parse or
/// names something undefined, and [`ToolError::Failed`] when the file is too
/// large or not JSON, or the filter fails on it.
pub(super) fn read(
    target: &Target,
    offset: Option<usize>,
    limit: Option<usize>,
) -> Result<ToolOutput, ToolError> {
    let (query, selector) = container::split_window(&target.inner, offset, limit)?;
    let query = query.trim();
    let label = &target.label;
    let size = super::file_size(&target.path)?;
    if size > MAX_JSON_BYTES {
        return Err(ToolError::Failed(format!(
            "{label} is {}, larger than the {} read parses for a query",
            media::size(size),
            media::size(MAX_JSON_BYTES)
        )));
    }
    let bytes = std::fs::read(&target.path)?;
    let document = jaq_json::read::parse_single(strip_bom(&bytes))
        .map_err(|error| ToolError::Failed(format!("{label} is not valid JSON: {error}")))?;
    let (text, results, stopped) = run(query, document)?;
    let shown = format!("{label}:{query}");
    let mut header = format!("[json {label}, query {query}, {}", count(results));
    if stopped {
        header.push_str(&format!(
            ", stopped at {} of output; narrow the query",
            media::size(MAX_OUTPUT_BYTES)
        ));
    }
    header.push_str("]\n");
    if text.is_empty() {
        return Ok(ToolOutput::new(header));
    }
    let numbered = lines::number(
        || Ok(std::io::Cursor::new(text.as_bytes())),
        &selector,
        &shown,
    )?;
    Ok(ToolOutput::new(header + &numbered))
}

/// Drops a UTF-8 byte order mark from the start of `bytes`.
fn strip_bom(bytes: &[u8]) -> &[u8] {
    bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(bytes)
}

/// Says how many values a query yielded, such as `1 result`.
fn count(results: usize) -> String {
    if results == 1 {
        "1 result".to_string()
    } else {
        format!("{results} results")
    }
}

/// Runs the jq filter `query` on `input` and returns the values it yields,
/// each pretty-printed with two-space indents and ending in a newline, with
/// how many there were and whether output stopped at [`MAX_OUTPUT_BYTES`].
///
/// # Errors
///
/// Returns [`ToolError::InvalidArguments`] when `query` does not parse or
/// names something undefined, and [`ToolError::Failed`] when it fails on
/// `input`.
pub(super) fn run(query: &str, input: Val) -> Result<(String, usize, bool), ToolError> {
    let defs = jaq_core::defs()
        .chain(jaq_std::defs())
        .chain(jaq_json::defs());
    let funs = jaq_core::funs()
        .chain(jaq_std::funs())
        .chain(jaq_json::funs());
    let arena = Arena::default();
    let modules = Loader::new(defs)
        .load(
            &arena,
            File {
                code: query,
                path: (),
            },
        )
        .map_err(|errors| invalid_query(query, errors.iter().map(|(_, e)| load_error(query, e))))?;
    let filter = Compiler::default()
        .with_funs(funs)
        .compile(modules)
        .map_err(|errors| {
            invalid_query(
                query,
                errors
                    .iter()
                    .flat_map(|(_, list)| list.iter().map(undefined)),
            )
        })?;
    let ctx = Ctx::<data::JustLut<Val>>::new(&filter.lut, Vars::new([]));
    let pretty = jaq_json::write::Pp {
        indent: Some("  ".to_string()),
        sep_space: true,
        ..Default::default()
    };
    let mut out = Vec::new();
    let mut results = 0usize;
    for value in filter.id.run((ctx, input)).map(unwrap_valr) {
        let value = value
            .map_err(|error| ToolError::Failed(format!("the query {query} failed: {error}")))?;
        jaq_json::write::write(&mut out, &pretty, 0, &value)?;
        out.push(b'\n');
        results += 1;
        if out.len() > MAX_OUTPUT_BYTES {
            return Ok((String::from_utf8_lossy(&out).into_owned(), results, true));
        }
    }
    Ok((String::from_utf8_lossy(&out).into_owned(), results, false))
}

/// Builds the error for a filter that does not parse or compile, from the
/// descriptions of what went wrong.
fn invalid_query(query: &str, problems: impl Iterator<Item = String>) -> ToolError {
    let problems: Vec<String> = problems.collect();
    ToolError::InvalidArguments(format!(
        "the jq filter {query} is not valid: {}",
        problems.join("; ")
    ))
}

/// Describes an error found while lexing or parsing `code`.
fn load_error(code: &str, error: &load::Error<&str>) -> String {
    match error {
        load::Error::Lex(errors) => errors
            .iter()
            .map(|(expect, at): &lex::Error<&str>| {
                format!("expected {} {}", expect.as_str(), place(code, at))
            })
            .collect::<Vec<_>>()
            .join("; "),
        load::Error::Parse(errors) => errors
            .iter()
            .map(|(expect, found): &parse::Error<&str>| {
                format!("expected {} {}", expect.as_str(), place(code, found))
            })
            .collect::<Vec<_>>()
            .join("; "),
        load::Error::Io(errors) => errors
            .iter()
            .map(|(path, message)| format!("cannot load {path}: {message}"))
            .collect::<Vec<_>>()
            .join("; "),
    }
}

/// Says where in `code` the slice `at` of it starts: at a character
/// position, or at the end.
fn place(code: &str, at: &str) -> String {
    let start = (at.as_ptr() as usize).wrapping_sub(code.as_ptr() as usize);
    if at.is_empty() || start >= code.len() {
        return "at the end".to_string();
    }
    format!("at character {}", code[..start].chars().count() + 1)
}

/// Describes a name the filter uses but nothing defines.
fn undefined((name, kind): &compile::Error<&str>) -> String {
    format!("undefined {} {name}", kind.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Runs `query` on the JSON `input` and returns the output text.
    fn query(query: &str, input: &str) -> Result<String, ToolError> {
        let input = jaq_json::read::parse_single(input.as_bytes()).unwrap();
        run(query, input).map(|(text, _, _)| text)
    }

    /// Paths, indexes, slices, iteration and the common built-ins work.
    #[test]
    fn runs_jq_filters() {
        let doc = r#"{"items":[{"n":"a","v":1},{"n":"b","v":2},{"n":"c","v":3}],"x":{"y":true}}"#;
        assert_eq!(query(".x.y", doc).unwrap(), "true\n");
        assert_eq!(query(".items[1].n", doc).unwrap(), "\"b\"\n");
        assert_eq!(query(".items[-1].v", doc).unwrap(), "3\n");
        assert_eq!(query(".items[1:].[] | .v", doc).unwrap(), "2\n3\n");
        assert_eq!(query(".items[].n", doc).unwrap(), "\"a\"\n\"b\"\n\"c\"\n");
        assert_eq!(query("keys", doc).unwrap(), "[\n  \"items\",\n  \"x\"\n]\n");
        assert_eq!(query(".items | length", doc).unwrap(), "3\n");
        assert_eq!(
            query("[.items[] | select(.v > 1) | .n]", doc).unwrap(),
            "[\n  \"b\",\n  \"c\"\n]\n"
        );
        assert_eq!(query(".items | map(.v) | add", doc).unwrap(), "6\n");
        assert_eq!(query(".missing", doc).unwrap(), "null\n");
    }

    /// Filters that do not parse or name unknown functions are refused with
    /// a reason; runtime failures say what failed.
    #[test]
    fn reports_bad_filters() {
        let bad = query(".a[", "{}").unwrap_err().to_string();
        assert!(bad.contains("is not valid"), "{bad}");
        let unknown = query("nosuchfn", "{}").unwrap_err().to_string();
        assert!(unknown.contains("undefined filter nosuchfn"), "{unknown}");
        let failed = query("keys", "5").unwrap_err().to_string();
        assert!(failed.contains("failed"), "{failed}");
    }
}
