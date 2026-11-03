//! The `read` tool: numbered lines from a text file.

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Read as _};

use super::snapshot::tag_file;
use crate::tool::parse_args;
use crate::{Sandbox, Tool, ToolError, ToolOutput};

/// Lines returned when the caller gives no limit.
const DEFAULT_LIMIT: usize = 2000;

/// Most bytes of file text one call returns.
const MAX_BYTES: usize = 50_000;

/// Longest line kept, in characters; longer lines are cut.
const MAX_LINE_CHARS: usize = 2000;

/// Bytes at the start of a file inspected for NUL when detecting binary files.
const SNIFF_BYTES: usize = 8192;

/// Reads a file with line numbers.
pub struct Read {
    /// The directory paths are confined to.
    sandbox: Sandbox,
}

impl Read {
    /// Creates the tool confined to `sandbox`.
    pub fn new(sandbox: Sandbox) -> Self {
        Self { sandbox }
    }
}

/// Arguments of a `read` call.
#[derive(Deserialize)]
struct Args {
    /// File to read, relative to the working directory.
    path: String,
    /// First line to return, 1-indexed.
    offset: Option<usize>,
    /// Most lines to return.
    limit: Option<usize>,
}

#[async_trait]
impl Tool for Read {
    /// Returns `read`.
    fn name(&self) -> &'static str {
        "read"
    }

    /// Reading changes nothing.
    fn read_only(&self) -> bool {
        true
    }

    /// Describes the tool to the model.
    fn description(&self) -> &'static str {
        "Read a text file and return its lines prefixed with line numbers, under a header with \
         the file's snapshot tag. Pass that tag and those line numbers to edit. Use offset and \
         limit to page through large files. Binary files are rejected."
    }

    /// Returns the argument schema.
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "File path, relative to the working directory." },
                "offset": { "type": "integer", "minimum": 1, "description": "First line to read, 1-indexed. Defaults to 1." },
                "limit": { "type": "integer", "minimum": 1, "description": "Maximum number of lines. Defaults to 2000." }
            },
            "required": ["path"]
        })
    }

    /// Shows the path being read.
    fn describe(&self, args: &Value) -> String {
        args["path"].as_str().unwrap_or("?").to_string()
    }

    /// Reads the file on a blocking thread.
    async fn call(&self, args: Value) -> Result<ToolOutput, ToolError> {
        let args: Args = parse_args(args)?;
        let path = self.sandbox.resolve(&args.path)?;
        let offset = args.offset.unwrap_or(1).max(1);
        let limit = args.limit.unwrap_or(DEFAULT_LIMIT).max(1);
        let text = tokio::task::spawn_blocking(move || read_lines(&path, offset, limit))
            .await
            .map_err(|e| ToolError::Failed(e.to_string()))??;
        Ok(ToolOutput::new(text))
    }
}

/// Reads up to `limit` lines of `path` starting at line `offset`, numbered, under a
/// header with the snapshot tag of the whole file.
///
/// Output stops early at [`MAX_BYTES`]; a trailing note says where to continue.
///
/// # Errors
///
/// Returns [`ToolError`] when the path is a directory, cannot be read, or holds
/// binary data.
fn read_lines(path: &std::path::Path, offset: usize, limit: usize) -> Result<String, ToolError> {
    if path.is_dir() {
        return Err(ToolError::Failed(format!(
            "{} is a directory, not a file",
            path.display()
        )));
    }
    let file = std::fs::File::open(path)?;
    let mut sniff = Vec::with_capacity(SNIFF_BYTES);
    file.take(SNIFF_BYTES as u64).read_to_end(&mut sniff)?;
    if sniff.contains(&0) {
        return Err(binary_error(path));
    }
    let header = format!("[tag {}]\n", tag_file(path)?);
    let mut reader = BufReader::new(std::fs::File::open(path)?);
    let mut out = String::new();
    let mut number = 0usize;
    let mut buf = Vec::new();
    loop {
        buf.clear();
        if reader.read_until(b'\n', &mut buf)? == 0 {
            break;
        }
        number += 1;
        if number < offset {
            continue;
        }
        let line = std::str::from_utf8(&buf).map_err(|_| binary_error(path))?;
        if number >= offset + limit || out.len() >= MAX_BYTES {
            out.push_str(&format!(
                "[more lines remain; continue with offset={number}]\n"
            ));
            return Ok(header + &out);
        }
        out.push_str(&format!(
            "{number:>6}\t{}\n",
            clip(line.trim_end_matches(['\n', '\r']))
        ));
    }
    if out.is_empty() {
        return Ok(if number == 0 {
            format!("{header}[file is empty]")
        } else {
            format!(
                "{header}[offset {offset} is past the end of the file, which has {number} lines]"
            )
        });
    }
    Ok(header + &out)
}

/// Builds the error for a file that is not text.
fn binary_error(path: &std::path::Path) -> ToolError {
    ToolError::Failed(format!("{} looks like a binary file", path.display()))
}

/// Cuts `line` to [`MAX_LINE_CHARS`] characters, marking the cut.
fn clip(line: &str) -> String {
    match line.char_indices().nth(MAX_LINE_CHARS) {
        Some((end, _)) => format!("{}… [line truncated]", &line[..end]),
        None => line.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::super::snapshot::tag;
    use super::*;
    use crate::testutil::scratch;

    /// Builds a `read` tool rooted at the scratch directory `name`.
    fn tool(name: &str) -> (Read, std::path::PathBuf) {
        let dir = scratch(name);
        (Read::new(Sandbox::new(&dir).unwrap()), dir)
    }

    /// Lines come back numbered and offset and limit select a window.
    #[tokio::test]
    async fn numbers_lines_and_honors_offset_and_limit() {
        let (tool, dir) = tool("read-window");
        std::fs::write(dir.join("f.txt"), "a\nb\nc\nd\n").unwrap();
        let all = tool.call(json!({ "path": "f.txt" })).await.unwrap();
        let expected = format!(
            "[tag {}]\n     1\ta\n     2\tb\n     3\tc\n     4\td\n",
            tag(b"a\nb\nc\nd\n")
        );
        assert_eq!(all.text, expected);
        let part = tool
            .call(json!({ "path": "f.txt", "offset": 2, "limit": 2 }))
            .await
            .unwrap();
        assert!(part.text.contains("]\n     2\tb\n     3\tc\n"));
        assert!(
            part.text
                .starts_with(&format!("[tag {}]", tag(b"a\nb\nc\nd\n")))
        );
        assert!(part.text.contains("continue with offset=4"));
    }

    /// Binary files are rejected.
    #[tokio::test]
    async fn rejects_binary_files() {
        let (tool, dir) = tool("read-binary");
        std::fs::write(dir.join("b.bin"), [0u8, 1, 2, 3]).unwrap();
        let result = tool.call(json!({ "path": "b.bin" })).await;
        assert!(matches!(result, Err(ToolError::Failed(m)) if m.contains("binary")));
    }

    /// Paths outside the working directory are rejected.
    #[tokio::test]
    async fn rejects_path_escape() {
        let (tool, _dir) = tool("read-escape");
        let result = tool.call(json!({ "path": "../../Cargo.toml" })).await;
        assert!(matches!(result, Err(ToolError::OutsidePath(_))));
    }

    /// Output is capped at the byte limit with a continuation note.
    #[tokio::test]
    async fn caps_output_size() {
        let (tool, dir) = tool("read-cap");
        let line = "x".repeat(100);
        std::fs::write(dir.join("big.txt"), format!("{line}\n").repeat(2000)).unwrap();
        let out = tool.call(json!({ "path": "big.txt" })).await.unwrap();
        assert!(out.text.len() < MAX_BYTES + 1000);
        assert!(out.text.contains("more lines remain"));
    }

    /// A directory is an error rather than a panic.
    #[tokio::test]
    async fn rejects_directories() {
        let (tool, dir) = tool("read-dir");
        std::fs::create_dir(dir.join("d")).unwrap();
        assert!(tool.call(json!({ "path": "d" })).await.is_err());
    }
}
