//! Querying a JSON file: `data.json:.items[0].name`.
//!
//! A query is a small path language over the parsed document, not full jq: a
//! pipeline of stages separated by `|`, where each stage is a path such as
//! `.items[0].name`, `.items[].id`, `.["a key"]`, `.[-1]` or `.[2:5]`, or one
//! of the builtins `keys`, `length`, `type`, `first`, `last` and
//! `select(<path> == <literal>)`. Each stage maps every value the stage
//! before it yields, and each value the last stage yields is pretty-printed;
//! the result is then numbered and paged like any text, so
//! `data.json:.items:50-100` reads lines 50 to 100 of the output.
//!
//! Evaluation streams one value at a time through the stages, recursing only
//! as deep as the query has stages and steps, never as deep as the document
//! nests, and the document itself is parsed with `serde_json`'s nesting limit.

use std::borrow::Cow;
use std::io::Write as _;
use std::ops::ControlFlow;

use serde_json::Value;

use super::container::{self, Target};
use super::{lines, media};
use crate::{ToolError, ToolOutput};

/// Largest JSON file that is parsed for a query, in bytes.
const MAX_JSON_BYTES: usize = 64 << 20;

/// Most bytes of output built before the query is stopped.
const MAX_OUTPUT_BYTES: usize = 4 << 20;

/// Most values a query yields before it is stopped.
const MAX_RESULTS: usize = 100_000;

/// Longest query accepted, in characters.
const MAX_QUERY_CHARS: usize = 4096;

/// How deep `serde_json` lets a document nest before it refuses it.
const MAX_DEPTH: usize = 128;

/// One step of a path.
#[derive(Debug, Clone, PartialEq)]
enum Step {
    /// `.name`, `."name"` or `.["name"]`: the value of an object's key.
    Field(String),
    /// `.[n]`: an array element, counted from the end when negative.
    Index(i64),
    /// `.[a:b]`: part of an array or string, either end counted from the end
    /// when negative and open when left out.
    Slice(Option<i64>, Option<i64>),
    /// `.[]`: every element of an array or value of an object.
    Iterate,
}

/// How `select` compares a path's value with its literal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Comparison {
    /// `==`.
    Equal,
    /// `!=`.
    NotEqual,
}

/// One stage of the pipeline.
#[derive(Debug, Clone, PartialEq)]
enum Stage {
    /// A path, `.` alone being the value itself.
    Path(Vec<Step>),
    /// `keys`: an object's keys, sorted, or an array's indexes.
    Keys,
    /// `length`: the size of an array, object or string, the absolute value
    /// of a number, or 0 for null.
    Length,
    /// `type`: the name of the value's type.
    Type,
    /// `first`: an array's first element.
    First,
    /// `last`: an array's last element.
    Last,
    /// `select(path op literal)`: the value itself when the path yields a
    /// value that compares as asked with the literal, nothing otherwise.
    Select(Vec<Step>, Comparison, Value),
}

/// What the builtins are called, for error messages.
const BUILTINS: &str = "keys, length, type, first, last, select(.field == value)";

/// Runs the query `target` names on the JSON file it points at and returns
/// the values it yields, pretty-printed and numbered.
///
/// # Errors
///
/// Returns [`ToolError::InvalidArguments`] when the query does not parse,
/// and [`ToolError::Failed`] when the file is too large, not JSON or nested
/// too deeply, or the query fails on it.
pub(super) fn read(
    target: &Target,
    offset: Option<usize>,
    limit: Option<usize>,
) -> Result<ToolOutput, ToolError> {
    let (query, selector) = container::split_window(&target.inner, offset, limit)?;
    let query = query.trim();
    let label = &target.label;
    let stages = parse(query)?;
    let size = super::file_size(&target.path)?;
    if size > MAX_JSON_BYTES {
        return Err(ToolError::Failed(format!(
            "{label} is {}, larger than the {} read parses for a query",
            media::size(size),
            media::size(MAX_JSON_BYTES)
        )));
    }
    let bytes = std::fs::read(&target.path)?;
    let document = parse_document(strip_bom(&bytes), label)?;
    let output = run(query, &stages, &document)?;
    let shown = format!("{label}:{query}");
    let mut header = format!("[json {label}, query {query}, {}", count(output.results));
    if let Some(reason) = output.stopped {
        header.push_str(&format!(", stopped at {reason}; narrow the query"));
    }
    header.push_str("]\n");
    if output.text.is_empty() {
        return Ok(ToolOutput::new(header));
    }
    let numbered = lines::number(
        || Ok(std::io::Cursor::new(output.text.as_bytes())),
        &selector,
        &shown,
    )?;
    Ok(ToolOutput::new(header + &numbered))
}

/// Parses the JSON document `bytes` of the file called `label`.
///
/// # Errors
///
/// Returns [`ToolError::Failed`] when it is not JSON or nests deeper than
/// [`MAX_DEPTH`] levels.
fn parse_document(bytes: &[u8], label: &str) -> Result<Value, ToolError> {
    serde_json::from_slice(bytes).map_err(|error| {
        if error.to_string().contains("recursion limit exceeded") {
            ToolError::Failed(format!(
                "{label} nests arrays and objects more than {MAX_DEPTH} levels deep, deeper than \
                 read parses ({error}); read it as text with a line selector such as {label}:1-100"
            ))
        } else {
            ToolError::Failed(format!("{label} is not valid JSON: {error}"))
        }
    })
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

/// What running a query produced.
#[derive(Debug, Default)]
struct Output {
    /// The values, each pretty-printed with two-space indents and ending in
    /// a newline.
    text: String,
    /// How many values were yielded.
    results: usize,
    /// Why the query stopped before yielding everything, when it did.
    stopped: Option<String>,
}

/// Collects the values a query yields, up to [`MAX_OUTPUT_BYTES`] bytes and
/// [`MAX_RESULTS`] values.
struct Sink {
    /// The pretty-printed values so far.
    bytes: Vec<u8>,
    /// How many values were taken.
    results: usize,
    /// Why taking values stopped, once it has.
    stopped: Option<String>,
}

impl Sink {
    /// Takes `value`, pretty-printed, and says whether to go on.
    fn take(&mut self, value: &Value) -> ControlFlow<()> {
        if self.results >= MAX_RESULTS {
            self.stopped = Some(format!("{MAX_RESULTS} values"));
            return ControlFlow::Break(());
        }
        self.results += 1;
        let left = MAX_OUTPUT_BYTES.saturating_sub(self.bytes.len());
        let mut writer = Bounded {
            bytes: &mut self.bytes,
            left,
        };
        let written = serde_json::to_writer_pretty(&mut writer, value)
            .ok()
            .and_then(|()| writer.write_all(b"\n").ok());
        if written.is_none() {
            self.stopped = Some(media::size(MAX_OUTPUT_BYTES));
            return ControlFlow::Break(());
        }
        ControlFlow::Continue(())
    }
}

/// A writer that appends to a buffer and fails once a set number of bytes
/// has been written, so that one huge value cannot outgrow the output cap.
struct Bounded<'a> {
    /// The buffer written to.
    bytes: &'a mut Vec<u8>,
    /// How many more bytes may be written.
    left: usize,
}

impl std::io::Write for Bounded<'_> {
    /// Appends what fits of `buf`, failing when it does not all fit.
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let fits = buf.len().min(self.left);
        self.bytes.extend_from_slice(&buf[..fits]);
        self.left -= fits;
        if fits < buf.len() {
            return Err(std::io::Error::other("the output is full"));
        }
        Ok(fits)
    }

    /// Does nothing; the buffer needs no flushing.
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Runs the parsed query `stages`, written as `query`, on `input` and
/// returns the values it yields.
///
/// # Errors
///
/// Returns [`ToolError::Failed`] when a stage does not apply to a value it
/// is given, such as indexing a number.
fn run(query: &str, stages: &[Stage], input: &Value) -> Result<Output, ToolError> {
    let mut sink = Sink {
        bytes: Vec::new(),
        results: 0,
        stopped: None,
    };
    let _ = pipe(stages, Cow::Borrowed(input), &mut sink)
        .map_err(|error| ToolError::Failed(format!("the query {query} failed: {error}")))?;
    Ok(Output {
        text: String::from_utf8_lossy(&sink.bytes).into_owned(),
        results: sink.results,
        stopped: sink.stopped,
    })
}

/// What a stage or step returns: whether to go on to the next value, or a
/// description of what failed.
type Flow = Result<ControlFlow<()>, String>;

/// Passes `value` through `stages` in turn and hands each value the last
/// one yields to `sink`.
///
/// # Errors
///
/// Returns a description of the first stage that does not apply to a value
/// it is given.
fn pipe(stages: &[Stage], value: Cow<'_, Value>, sink: &mut Sink) -> Flow {
    let Some((stage, rest)) = stages.split_first() else {
        return Ok(sink.take(&value));
    };
    match stage {
        Stage::Path(steps) => walk(steps, value, &mut |found| pipe(rest, found, sink)),
        Stage::Select(steps, comparison, literal) => {
            let mut keep = false;
            let _ = walk(steps, Cow::Borrowed(&*value), &mut |found| {
                let equal = same(&found, literal);
                keep = equal == (*comparison == Comparison::Equal);
                Ok(if keep {
                    ControlFlow::Break(())
                } else {
                    ControlFlow::Continue(())
                })
            })?;
            if keep {
                pipe(rest, value, sink)
            } else {
                Ok(ControlFlow::Continue(()))
            }
        }
        Stage::Keys => pipe(rest, Cow::Owned(keys(&value)?), sink),
        Stage::Length => pipe(rest, Cow::Owned(length(&value)?), sink),
        Stage::Type => pipe(rest, Cow::Owned(Value::from(type_name(&value))), sink),
        Stage::First => walk(&[Step::Index(0)], value, &mut |found| {
            pipe(rest, found, sink)
        }),
        Stage::Last => walk(&[Step::Index(-1)], value, &mut |found| {
            pipe(rest, found, sink)
        }),
    }
}

/// Follows `steps` from `value` and hands each value they reach to `each`,
/// stopping when it says to.
///
/// # Errors
///
/// Returns a description of the first step that does not apply to the value
/// it meets, or what `each` returns.
fn walk(
    steps: &[Step],
    value: Cow<'_, Value>,
    each: &mut dyn FnMut(Cow<'_, Value>) -> Flow,
) -> Flow {
    let Some((step, rest)) = steps.split_first() else {
        return each(value);
    };
    match step {
        Step::Iterate => match value {
            Cow::Borrowed(Value::Array(items)) => {
                for item in items {
                    if walk(rest, Cow::Borrowed(item), each)?.is_break() {
                        return Ok(ControlFlow::Break(()));
                    }
                }
                Ok(ControlFlow::Continue(()))
            }
            Cow::Borrowed(Value::Object(fields)) => {
                for item in fields.values() {
                    if walk(rest, Cow::Borrowed(item), each)?.is_break() {
                        return Ok(ControlFlow::Break(()));
                    }
                }
                Ok(ControlFlow::Continue(()))
            }
            Cow::Owned(Value::Array(items)) => {
                for item in items {
                    if walk(rest, Cow::Owned(item), each)?.is_break() {
                        return Ok(ControlFlow::Break(()));
                    }
                }
                Ok(ControlFlow::Continue(()))
            }
            Cow::Owned(Value::Object(fields)) => {
                for (_, item) in fields {
                    if walk(rest, Cow::Owned(item), each)?.is_break() {
                        return Ok(ControlFlow::Break(()));
                    }
                }
                Ok(ControlFlow::Continue(()))
            }
            other => Err(format!("cannot iterate over {}", type_name(&other))),
        },
        step => {
            let next = match value {
                Cow::Borrowed(value) => index(value, step)?,
                Cow::Owned(value) => Cow::Owned(index(&value, step)?.into_owned()),
            };
            walk(rest, next, each)
        }
    }
}

/// The value `step`, which is not [`Step::Iterate`], reaches from `value`:
/// `null` for a missing key or an index past the end, as jq gives.
///
/// # Errors
///
/// Returns a description of the problem when `step` does not apply to a
/// value of this type.
fn index<'a>(value: &'a Value, step: &Step) -> Result<Cow<'a, Value>, String> {
    match (value, step) {
        (Value::Null, _) => Ok(Cow::Owned(Value::Null)),
        (Value::Object(fields), Step::Field(key)) => Ok(fields
            .get(key)
            .map_or(Cow::Owned(Value::Null), Cow::Borrowed)),
        (Value::Array(items), Step::Index(at)) => Ok(position(*at, items.len())
            .and_then(|at| items.get(at))
            .map_or(Cow::Owned(Value::Null), Cow::Borrowed)),
        (Value::Array(items), Step::Slice(from, to)) => {
            let (from, to) = bounds(*from, *to, items.len());
            Ok(Cow::Owned(Value::Array(items[from..to].to_vec())))
        }
        (Value::String(text), Step::Slice(from, to)) => {
            let chars: Vec<char> = text.chars().collect();
            let (from, to) = bounds(*from, *to, chars.len());
            Ok(Cow::Owned(Value::String(chars[from..to].iter().collect())))
        }
        (value, Step::Field(key)) => Err(format!(
            "cannot index {} with {}",
            type_name(value),
            Value::from(key.as_str())
        )),
        (value, Step::Index(_)) => Err(format!("cannot index {} with a number", type_name(value))),
        (value, Step::Slice(..)) => Err(format!("cannot slice {}", type_name(value))),
        (value, Step::Iterate) => Err(format!("cannot iterate over {}", type_name(value))),
    }
}

/// The position in a sequence of `len` items that `at` names, counting from
/// the end when it is negative; `None` when that is before the start.
fn position(at: i64, len: usize) -> Option<usize> {
    if at >= 0 {
        usize::try_from(at).ok()
    } else {
        len.checked_sub(usize::try_from(at.unsigned_abs()).ok()?)
    }
}

/// The start and end positions a slice from `from` to `to` covers in a
/// sequence of `len` items, clamped to it and never crossing.
fn bounds(from: Option<i64>, to: Option<i64>, len: usize) -> (usize, usize) {
    let clamp = |at: i64| {
        if at >= 0 {
            usize::try_from(at).unwrap_or(usize::MAX).min(len)
        } else {
            usize::try_from(at.unsigned_abs())
                .ok()
                .and_then(|back| len.checked_sub(back))
                .unwrap_or(0)
        }
    };
    let start = from.map_or(0, clamp);
    let end = to.map_or(len, clamp);
    (start, end.max(start))
}

/// The keys of an object, sorted, or the indexes of an array.
///
/// # Errors
///
/// Returns a description of the problem for any other value.
fn keys(value: &Value) -> Result<Value, String> {
    match value {
        Value::Object(fields) => {
            let mut names: Vec<&String> = fields.keys().collect();
            names.sort();
            Ok(Value::Array(
                names
                    .into_iter()
                    .map(|name| Value::from(name.as_str()))
                    .collect(),
            ))
        }
        Value::Array(items) => Ok(Value::Array((0..items.len()).map(Value::from).collect())),
        other => Err(format!("{} has no keys", type_name(other))),
    }
}

/// The length of `value`: elements, keys or characters, the absolute value
/// of a number, 0 for null.
///
/// # Errors
///
/// Returns a description of the problem for a boolean.
fn length(value: &Value) -> Result<Value, String> {
    Ok(match value {
        Value::Null => Value::from(0),
        Value::Array(items) => Value::from(items.len()),
        Value::Object(fields) => Value::from(fields.len()),
        Value::String(text) => Value::from(text.chars().count()),
        Value::Number(number) => match (number.as_u64(), number.as_i64()) {
            (Some(n), _) => Value::from(n),
            (None, Some(n)) => Value::from(n.unsigned_abs()),
            _ => Value::from(number.as_f64().unwrap_or_default().abs()),
        },
        Value::Bool(_) => return Err("boolean has no length".to_string()),
    })
}

/// The name of the type of `value`, as jq names it.
fn type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

/// Whether `a` and `b` are equal, numbers being compared by value so that
/// `1` equals `1.0`.
fn same(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Number(a), Value::Number(b)) => a.as_f64() == b.as_f64(),
        (a, b) => a == b,
    }
}

/// Parses the query `query` into its stages.
///
/// # Errors
///
/// Returns [`ToolError::InvalidArguments`] saying what is wrong and at which
/// character, when the query is empty, too long or not in the supported
/// syntax.
fn parse(query: &str) -> Result<Vec<Stage>, ToolError> {
    if query.chars().count() > MAX_QUERY_CHARS {
        return Err(ToolError::InvalidArguments(format!(
            "the query is longer than {MAX_QUERY_CHARS} characters"
        )));
    }
    let mut parser = Parser { text: query, at: 0 };
    parser.pipeline().map_err(|(at, problem)| {
        let place = if at >= query.len() {
            "at the end".to_string()
        } else {
            format!("at character {}", query[..at].chars().count() + 1)
        };
        ToolError::InvalidArguments(format!(
            "the query {query} is not valid: {problem} {place}. A query is a path such as \
             .items[0].name, .items[].id, .[\"a key\"] or .[2:5], optionally piped through | to \
             {BUILTINS}"
        ))
    })
}

/// A problem found while parsing: the byte position and what was wrong.
type Problem = (usize, String);

/// Reads a query from left to right.
struct Parser<'a> {
    /// The whole query.
    text: &'a str,
    /// The byte position reached.
    at: usize,
}

impl<'a> Parser<'a> {
    /// The text not yet read.
    fn rest(&self) -> &'a str {
        let text = self.text;
        &text[self.at..]
    }

    /// The next character, if any.
    fn peek(&self) -> Option<char> {
        self.rest().chars().next()
    }

    /// Skips whitespace.
    fn skip_space(&mut self) {
        let trimmed = self.rest().trim_start();
        self.at = self.text.len() - trimmed.len();
    }

    /// Reads `expected` when the text goes on with it and says whether it did.
    fn eat(&mut self, expected: &str) -> bool {
        if self.rest().starts_with(expected) {
            self.at += expected.len();
            true
        } else {
            false
        }
    }

    /// A problem at the current position: `wanted` was expected but
    /// something else, or nothing, came.
    fn expected(&self, wanted: &str) -> Problem {
        let found = match self.peek() {
            Some(c) => format!("found {c:?}"),
            None => "found the end".to_string(),
        };
        (self.at, format!("expected {wanted}, {found}"))
    }

    /// Reads the whole query: stages separated by `|`.
    ///
    /// # Errors
    ///
    /// Returns the first problem found.
    fn pipeline(&mut self) -> Result<Vec<Stage>, Problem> {
        let mut stages = vec![self.stage()?];
        loop {
            self.skip_space();
            if self.peek().is_none() {
                return Ok(stages);
            }
            if !self.eat("|") {
                return Err(self.expected("| or the end of the query"));
            }
            stages.push(self.stage()?);
        }
    }

    /// Reads one stage: a path or a builtin.
    ///
    /// # Errors
    ///
    /// Returns the problem when neither comes, or a builtin is unknown.
    fn stage(&mut self) -> Result<Stage, Problem> {
        self.skip_space();
        if self.peek() == Some('.') {
            return Ok(Stage::Path(self.path()?));
        }
        let start = self.at;
        let Some(name) = self.identifier() else {
            return Err(self.expected(&format!("a path starting with . or one of {BUILTINS}")));
        };
        Ok(match name {
            "keys" => Stage::Keys,
            "length" => Stage::Length,
            "type" => Stage::Type,
            "first" => Stage::First,
            "last" => Stage::Last,
            "select" => self.select()?,
            other => {
                return Err((
                    start,
                    format!("unknown function {other}; the functions are {BUILTINS}"),
                ));
            }
        })
    }

    /// Reads the parenthesized condition of a `select`.
    ///
    /// # Errors
    ///
    /// Returns the problem when it is not `(<path> == <literal>)` or
    /// `(<path> != <literal>)`.
    fn select(&mut self) -> Result<Stage, Problem> {
        self.skip_space();
        if !self.eat("(") {
            return Err(self.expected("( after select"));
        }
        self.skip_space();
        if self.peek() != Some('.') {
            return Err(self.expected("a path starting with ."));
        }
        let steps = self.path()?;
        self.skip_space();
        let comparison = if self.eat("==") {
            Comparison::Equal
        } else if self.eat("!=") {
            Comparison::NotEqual
        } else {
            return Err(self.expected("== or !="));
        };
        self.skip_space();
        let literal = self.literal()?;
        self.skip_space();
        if !self.eat(")") {
            return Err(self.expected(")"));
        }
        Ok(Stage::Select(steps, comparison, literal))
    }

    /// Reads a path: `.` followed by any number of steps.
    ///
    /// # Errors
    ///
    /// Returns the problem with the first malformed step.
    fn path(&mut self) -> Result<Vec<Step>, Problem> {
        let mut steps = Vec::new();
        self.eat(".");
        match self.peek() {
            Some('[') => steps.push(self.bracket()?),
            Some('"') => steps.push(Step::Field(self.string()?)),
            Some(c) if c.is_ascii_alphabetic() || c == '_' => {
                let name = self.identifier().unwrap_or_default();
                steps.push(Step::Field(name.to_string()));
            }
            _ => {}
        }
        loop {
            match self.peek() {
                Some('[') => steps.push(self.bracket()?),
                Some('.') => {
                    self.at += 1;
                    match self.peek() {
                        Some('[') => steps.push(self.bracket()?),
                        Some('"') => steps.push(Step::Field(self.string()?)),
                        _ => {
                            let Some(name) = self.identifier() else {
                                return Err(self.expected("a key name, a quoted key or ["));
                            };
                            steps.push(Step::Field(name.to_string()));
                        }
                    }
                }
                _ => return Ok(steps),
            }
        }
    }

    /// Reads a bracketed step: `[]`, `["key"]`, `[n]` or `[a:b]`.
    ///
    /// # Errors
    ///
    /// Returns the problem when the brackets hold anything else.
    fn bracket(&mut self) -> Result<Step, Problem> {
        self.eat("[");
        self.skip_space();
        let step = if self.peek() == Some(']') {
            Step::Iterate
        } else if self.peek() == Some('"') {
            Step::Field(self.string()?)
        } else {
            let from = self.integer()?;
            self.skip_space();
            if self.eat(":") {
                self.skip_space();
                let to = if self.peek() == Some(']') {
                    None
                } else {
                    Some(
                        self.integer()?
                            .ok_or_else(|| self.expected("a whole number or ]"))?,
                    )
                };
                Step::Slice(from, to)
            } else {
                Step::Index(
                    from.ok_or_else(|| self.expected("a whole number, a quoted key, : or ]"))?,
                )
            }
        };
        self.skip_space();
        if !self.eat("]") {
            return Err(self.expected("]"));
        }
        Ok(step)
    }

    /// Reads a whole number, possibly negative; `None` when none comes.
    ///
    /// # Errors
    ///
    /// Returns the problem when the number does not fit 64 bits.
    fn integer(&mut self) -> Result<Option<i64>, Problem> {
        let start = self.at;
        let rest = self.rest();
        let sign = usize::from(rest.starts_with('-'));
        let digits = rest[sign..].bytes().take_while(u8::is_ascii_digit).count();
        if digits == 0 {
            return Ok(None);
        }
        let text = &rest[..sign + digits];
        let number = text
            .parse()
            .map_err(|_| (start, format!("the number {text} is too large")))?;
        self.at += sign + digits;
        Ok(Some(number))
    }

    /// Reads a name: a letter or `_` followed by letters, digits and `_`.
    fn identifier(&mut self) -> Option<&'a str> {
        let rest = self.rest();
        let len = rest
            .char_indices()
            .find(|(index, c)| {
                !(c.is_ascii_alphabetic() || *c == '_' || (*index > 0 && c.is_ascii_digit()))
            })
            .map_or(rest.len(), |(index, _)| index);
        if len == 0 {
            return None;
        }
        self.at += len;
        Some(&rest[..len])
    }

    /// Reads a double-quoted JSON string.
    ///
    /// # Errors
    ///
    /// Returns the problem when it is not closed or has a bad escape.
    fn string(&mut self) -> Result<String, Problem> {
        let start = self.at;
        let rest = self.rest();
        let mut escaped = false;
        let close = rest
            .char_indices()
            .skip(1)
            .find(|(_, c)| {
                let closes = !escaped && *c == '"';
                escaped = !escaped && *c == '\\';
                closes
            })
            .map(|(index, _)| index)
            .ok_or_else(|| (start, "this string is not closed".to_string()))?;
        let literal = &rest[..=close];
        let text = serde_json::from_str(literal)
            .map_err(|error| (start, format!("this string is not valid: {error}")))?;
        self.at += close + 1;
        Ok(text)
    }

    /// Reads a JSON literal: a string, number, `true`, `false` or `null`.
    ///
    /// # Errors
    ///
    /// Returns the problem when none comes.
    fn literal(&mut self) -> Result<Value, Problem> {
        if self.peek() == Some('"') {
            return self.string().map(Value::String);
        }
        for (word, value) in [
            ("true", Value::Bool(true)),
            ("false", Value::Bool(false)),
            ("null", Value::Null),
        ] {
            if self.eat(word) {
                return Ok(value);
            }
        }
        let start = self.at;
        let len = self
            .rest()
            .find(|c: char| !(c.is_ascii_digit() || matches!(c, '-' | '+' | '.' | 'e' | 'E')))
            .unwrap_or(self.rest().len());
        let text = &self.rest()[..len];
        match serde_json::from_str::<Value>(text) {
            Ok(number @ Value::Number(_)) => {
                self.at += len;
                Ok(number)
            }
            _ => Err((
                start,
                "expected a string, number, true, false or null".to_string(),
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Runs `query` on the JSON `input` and returns the output text.
    fn query(query: &str, input: &str) -> Result<String, ToolError> {
        let input: Value = serde_json::from_str(input).unwrap();
        run(query, &parse(query)?, &input).map(|output| output.text)
    }

    /// Paths, indexes, slices, iteration, pipes and the builtins work.
    #[test]
    fn runs_queries() {
        let doc =
            r#"{"items":[{"n":"a","v":1},{"n":"b","v":2},{"n":"c","v":3}],"x":{"y":true},"a b":5}"#;
        assert_eq!(query(".", "5").unwrap(), "5\n");
        assert_eq!(query(".x.y", doc).unwrap(), "true\n");
        assert_eq!(query(".items[1].n", doc).unwrap(), "\"b\"\n");
        assert_eq!(query(".items[-1].v", doc).unwrap(), "3\n");
        assert_eq!(query(".items[-9]", doc).unwrap(), "null\n");
        assert_eq!(query(".items[1:].[] | .v", doc).unwrap(), "2\n3\n");
        assert_eq!(query(".items[:-2] | length", doc).unwrap(), "1\n");
        assert_eq!(query(".items[].n", doc).unwrap(), "\"a\"\n\"b\"\n\"c\"\n");
        assert_eq!(query(".[\"a b\"]", doc).unwrap(), "5\n");
        assert_eq!(query(".\"a b\"", doc).unwrap(), "5\n");
        assert_eq!(
            query("keys", doc).unwrap(),
            "[\n  \"a b\",\n  \"items\",\n  \"x\"\n]\n"
        );
        assert_eq!(query(".items | length", doc).unwrap(), "3\n");
        assert_eq!(query(".items | first | .n", doc).unwrap(), "\"a\"\n");
        assert!(query(".items|last.n", doc).is_err());
        assert_eq!(query(".items | last | .n", doc).unwrap(), "\"c\"\n");
        assert_eq!(query(".x | type", doc).unwrap(), "\"object\"\n");
        assert_eq!(
            query(".items[] | select(.v == 2) | .n", doc).unwrap(),
            "\"b\"\n"
        );
        assert_eq!(
            query(".items[] | select(.n != \"b\") | .v", doc).unwrap(),
            "1\n3\n"
        );
        assert_eq!(
            query(".items[] | select(.v == 3.0) | .n", doc).unwrap(),
            "\"c\"\n"
        );
        assert_eq!(query(".missing", doc).unwrap(), "null\n");
        assert_eq!(query(".missing.deeper[0]", doc).unwrap(), "null\n");
        assert_eq!(query(".x | keys | .[0]", doc).unwrap(), "\"y\"\n");
        assert_eq!(query(".items[0].n[0:1]", doc).unwrap(), "\"a\"\n");
    }

    /// Queries that do not parse, or name functions that do not exist, are
    /// refused with the position; failures on the data say what failed.
    #[test]
    fn reports_bad_queries() {
        let bad = query(".a[", "{}").unwrap_err();
        assert!(matches!(bad, ToolError::InvalidArguments(_)), "{bad}");
        assert!(bad.to_string().contains("at the end"), "{bad}");
        let unknown = query(".a | nosuchfn", "{}").unwrap_err().to_string();
        assert!(
            unknown.contains("unknown function nosuchfn") && unknown.contains("at character 6"),
            "{unknown}"
        );
        let env = query("env", "{}").unwrap_err().to_string();
        assert!(env.contains("unknown function env"), "{env}");
        let recursive = query("def f: 1 + f; f", "{}").unwrap_err().to_string();
        assert!(recursive.contains("unknown function def"), "{recursive}");
        let range = query("[range(1e9)]", "{}").unwrap_err().to_string();
        assert!(range.contains("is not valid"), "{range}");
        let trailing = query(".a b", "{}").unwrap_err().to_string();
        assert!(trailing.contains("at character 4"), "{trailing}");
        let failed = query("keys", "5").unwrap_err().to_string();
        assert!(failed.contains("failed: number has no keys"), "{failed}");
        let index = query(".a.b", "{\"a\":1}").unwrap_err().to_string();
        assert!(index.contains("cannot index number with \"b\""), "{index}");
    }

    /// A document nested deeper than the parser allows is refused with a
    /// clear message instead of overflowing the stack.
    #[test]
    fn refuses_documents_nested_too_deep() {
        let deep = "[".repeat(1 << 20);
        let error = parse_document(deep.as_bytes(), "deep.json")
            .unwrap_err()
            .to_string();
        assert!(error.contains("more than 128 levels deep"), "{error}");
        let fine = format!("{}{}", "[".repeat(100), "]".repeat(100));
        assert!(parse_document(fine.as_bytes(), "ok.json").is_ok());
    }

    /// Output stops at the byte cap, even inside one huge value, and at the
    /// value cap.
    #[test]
    fn bounds_output() {
        let big = Value::String("x".repeat(MAX_OUTPUT_BYTES + 10));
        let output = run(".", &parse(".").unwrap(), &big).unwrap();
        assert!(output.text.len() <= MAX_OUTPUT_BYTES);
        assert!(output.stopped.is_some());
        let many = Value::Array(vec![Value::Null; MAX_RESULTS + 10]);
        let output = run(".[]", &parse(".[]").unwrap(), &many).unwrap();
        assert_eq!(output.results, MAX_RESULTS);
        assert_eq!(output.stopped.as_deref(), Some("100000 values"));
    }
}
