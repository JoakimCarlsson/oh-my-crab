//! The `write` tool: create or overwrite a file.

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};

use super::atomic::write_atomic;
use super::snapshot::tag;
use super::{generated, lookalike};
use crate::language::{Languages, Written, settle};
use crate::tool::{marked_lines, parse_args};
use crate::{Sandbox, Tool, ToolError, ToolOutput};

/// Creates a file or replaces its whole content.
pub struct Write {
    /// The directory paths are confined to.
    sandbox: Sandbox,
    /// The language service that formats the written file and says what is wrong with it.
    languages: Languages,
}

impl Write {
    /// Creates the tool confined to `sandbox`, with no language service.
    pub fn new(sandbox: Sandbox) -> Self {
        Self::with_languages(sandbox, Languages::default())
    }

    /// Creates the tool confined to `sandbox`, which has the files it writes formatted by `languages` and reports what it says is wrong with them.
    pub fn with_languages(sandbox: Sandbox, languages: Languages) -> Self {
        Self { sandbox, languages }
    }
}

/// Arguments of a `write` call.
#[derive(Deserialize)]
struct Args {
    /// File to write, relative to the working directory.
    path: String,
    /// The complete new content.
    content: String,
    /// Whether the user explicitly asked for this file to be overwritten although it is generated.
    #[serde(default)]
    generated: bool,
    /// Whether to create the file even though its directory is missing and files with a path ending like it exist.
    #[serde(default)]
    create: bool,
}

#[async_trait]
impl Tool for Write {
    /// Returns `write`.
    fn name(&self) -> &'static str {
        "write"
    }

    /// Describes the tool to the model.
    fn description(&self) -> &'static str {
        "Create a file, or overwrite it completely, with the given content. Missing parent \
         directories are created. Prefer edit for changing part of an existing file. The user \
         is asked to approve each write.\n\
         Overwriting a generated file (a generator's banner near the top, generated names like \
         *.pb.go, or a lockfile) is refused: change its source or generator instead, and set \
         generated: true only when the user explicitly asked to change that file itself.\n\
         When the parent directory does not exist but files whose path ends with the given \
         path do, the write is refused and names them, since the path is likely mistyped; set \
         create: true to create the new file anyway."
    }

    /// Returns the argument schema.
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "File to write, relative to the working directory." },
                "content": { "type": "string", "description": "The complete file content." },
                "generated": { "type": "boolean", "description": "Allow overwriting a generated file or lockfile. Only when the user explicitly asked to change this file itself." },
                "create": { "type": "boolean", "description": "Create the file even though files whose path ends with this path exist elsewhere." }
            },
            "required": ["path", "content"]
        })
    }

    /// Writes change files, so they need approval.
    fn needs_approval(&self) -> bool {
        true
    }

    /// Shows the file, the size of the content and all of it.
    fn describe(&self, args: &Value) -> String {
        let path = self.sandbox.describe(args["path"].as_str().unwrap_or("?"));
        let content = args["content"].as_str().unwrap_or_default();
        format!(
            "{path} ({} lines, {} bytes)\n{}",
            content.lines().count(),
            content.len(),
            marked_lines('+', content)
        )
    }

    /// Writes the file on a blocking thread, then has the language service format it and say what is wrong with it.
    async fn call(&self, args: Value) -> Result<ToolOutput, ToolError> {
        let args: Args = parse_args(args)?;
        let path = self.sandbox.resolve(&args.path)?;
        let mut file = Written {
            path: path.clone(),
            shown: args.path.clone(),
            text: args.content.clone(),
            formatted: false,
        };
        let sandbox = self.sandbox.clone();
        let mut text = tokio::task::spawn_blocking(move || {
            check(&sandbox, &path, &args)?;
            write(&path, &args)
        })
        .await
        .map_err(|e| ToolError::Failed(e.to_string()))??;
        let diagnostics = settle(&self.languages, std::slice::from_mut(&mut file)).await;
        if file.formatted {
            text.push_str(&format!(
                "; formatted by the language server to {} bytes, tag {}",
                file.text.len(),
                tag(file.text.as_bytes())
            ));
        }
        if !diagnostics.is_empty() {
            text.push_str("\n\n");
            text.push_str(&diagnostics);
        }
        Ok(ToolOutput::new(text))
    }
}

/// Refuses a write of `args` to `path` that would overwrite a generated file,
/// or create a file in a missing directory when files whose path ends like
/// it exist, unless the arguments say to go ahead.
///
/// # Errors
///
/// Returns [`ToolError::Failed`] naming the generated file's evidence, or the
/// existing files the path probably meant.
fn check(sandbox: &Sandbox, path: &std::path::Path, args: &Args) -> Result<(), ToolError> {
    generated::refuse_existing(path, &args.path, args.generated, "\"generated\": true")?;
    if args.create || path.exists() || path.parent().is_none_or(std::path::Path::exists) {
        return Ok(());
    }
    let candidates = lookalike::lookalikes(sandbox, &args.path);
    if candidates.is_empty() {
        return Ok(());
    }
    Err(ToolError::Failed(format!(
        "{} Nothing was written. To create a new file at {} anyway, call write again with \
         \"create\": true.",
        lookalike::missing(&args.path, &candidates),
        args.path
    )))
}

/// Writes `args.content` to `path`, creating missing parent directories.
///
/// # Errors
///
/// Returns [`ToolError::Failed`] when `path` is a directory and
/// [`ToolError::Io`] when the write fails.
fn write(path: &std::path::Path, args: &Args) -> Result<String, ToolError> {
    if path.is_dir() {
        return Err(ToolError::Failed(format!(
            "{} is a directory, not a file",
            args.path
        )));
    }
    let existed = path.exists();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    write_atomic(path, args.content.as_bytes())?;
    let verb = if existed { "Overwrote" } else { "Created" };
    Ok(format!(
        "{verb} {} ({} bytes)",
        args.path,
        args.content.len()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::scratch;

    /// Builds a `write` tool rooted at the scratch directory `name`.
    fn tool(name: &str) -> (Write, std::path::PathBuf) {
        let dir = scratch(name);
        (Write::new(Sandbox::new(&dir).unwrap()), dir)
    }

    /// A new file is created with its parent directories, then overwritten.
    #[tokio::test]
    async fn creates_then_overwrites() {
        let (tool, dir) = tool("write-basic");
        let created = tool
            .call(json!({ "path": "a/b/c.txt", "content": "one" }))
            .await
            .unwrap();
        assert!(created.text.starts_with("Created"));
        let overwritten = tool
            .call(json!({ "path": "a/b/c.txt", "content": "two" }))
            .await
            .unwrap();
        assert!(overwritten.text.starts_with("Overwrote"));
        assert_eq!(
            std::fs::read_to_string(dir.join("a/b/c.txt")).unwrap(),
            "two"
        );
    }

    /// Paths outside the working directory and directories are rejected.
    #[tokio::test]
    async fn rejects_escapes_and_directories() {
        let (tool, dir) = tool("write-reject");
        std::fs::create_dir(dir.join("d")).unwrap();
        let escape = tool
            .call(json!({ "path": "../outside.txt", "content": "x" }))
            .await;
        assert!(matches!(escape, Err(ToolError::OutsidePath(_))));
        let directory = tool.call(json!({ "path": "d", "content": "x" })).await;
        assert!(matches!(directory, Err(ToolError::Failed(_))));
    }

    /// A dangling symlink pointing outside cannot be used to write outside.
    #[cfg(unix)]
    #[tokio::test]
    async fn rejects_writing_through_a_dangling_symlink() {
        let (tool, dir) = tool("write-dangling");
        let outside = scratch("write-dangling-target");
        std::os::unix::fs::symlink(outside.join("new.txt"), dir.join("link")).unwrap();
        let result = tool
            .call(json!({ "path": "link", "content": "escaped" }))
            .await;
        assert!(matches!(result, Err(ToolError::OutsidePath(_))));
        assert!(!outside.join("new.txt").exists());
    }

    /// A language service formats the written file, and the result gives the tag of what is on disk and what is wrong with it; without one, the result is as it always was.
    #[tokio::test]
    async fn formats_and_reports_diagnostics() {
        use crate::language::tests::{Fake, diagnostic};
        use crate::{Languages, Severity};
        let dir = scratch("write-languages");
        let fake = Fake {
            formats: true,
            ..Fake::default()
        };
        fake.diagnostics.lock().unwrap().push(diagnostic(
            &dir.join("m.rs"),
            1,
            Severity::Warning,
            "unused",
        ));
        let tool = Write::with_languages(
            Sandbox::new(&dir).unwrap(),
            Languages::new(Some(std::sync::Arc::new(fake))),
        );
        let out = tool
            .call(json!({ "path": "m.rs", "content": "fn x() {}\n" }))
            .await
            .unwrap();
        let on_disk = std::fs::read_to_string(dir.join("m.rs")).unwrap();
        assert_eq!(on_disk, "FN X() {}\n");
        assert_eq!(
            out.text,
            format!(
                "Created m.rs (10 bytes); formatted by the language server to 10 bytes, tag {}\n\nDiagnostics: 0 error(s), 1 warning(s)\nm.rs:1:5 warning: unused (rustc)",
                tag(on_disk.as_bytes())
            )
        );
        let (plain, _) = tool_without_languages(&dir);
        let out = plain
            .call(json!({ "path": "m.rs", "content": "fn x() {}\n" }))
            .await
            .unwrap();
        assert_eq!(out.text, "Overwrote m.rs (10 bytes)");
    }

    /// Overwriting a generated file is refused unless allowed; creating a file
    /// with a lockfile's name is not overwriting one.
    #[tokio::test]
    async fn refuses_to_overwrite_generated_files() {
        let (tool, dir) = tool("write-generated");
        std::fs::write(dir.join("Cargo.lock"), "# lock\n").unwrap();
        let refused = tool
            .call(json!({ "path": "Cargo.lock", "content": "x" }))
            .await;
        assert!(
            matches!(&refused, Err(ToolError::Failed(m)) if m.contains("Cargo.lock is a generated file") && m.contains("lockfile")),
            "{refused:?}"
        );
        assert_eq!(
            std::fs::read_to_string(dir.join("Cargo.lock")).unwrap(),
            "# lock\n"
        );
        tool.call(json!({ "path": "Cargo.lock", "content": "x", "generated": true }))
            .await
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.join("Cargo.lock")).unwrap(),
            "x"
        );
        tool.call(json!({ "path": "sub/go.sum", "content": "y" }))
            .await
            .unwrap();
    }

    /// A new file in a missing directory is refused when its path is the end
    /// of an existing file's path, unless create is set; in an existing
    /// directory it is created.
    #[tokio::test]
    async fn refuses_likely_mistyped_new_paths() {
        let (tool, dir) = tool("write-lookalike");
        std::fs::create_dir_all(dir.join("crates/a/src")).unwrap();
        std::fs::write(dir.join("crates/a/src/lib.rs"), "old\n").unwrap();
        let refused = tool
            .call(json!({ "path": "a/src/lib.rs", "content": "new\n" }))
            .await;
        assert!(
            matches!(&refused, Err(ToolError::Failed(m)) if m.contains("crates/a/src/lib.rs") && m.contains("\"create\": true")),
            "{refused:?}"
        );
        assert!(!dir.join("a").exists());
        tool.call(json!({ "path": "a/src/lib.rs", "content": "new\n", "create": true }))
            .await
            .unwrap();
        assert!(dir.join("a/src/lib.rs").exists());
        std::fs::remove_file(dir.join("a/src/lib.rs")).unwrap();
        tool.call(json!({ "path": "a/src/lib.rs", "content": "new\n" }))
            .await
            .unwrap();
    }

    /// A `write` tool rooted at `dir` with no language service.
    fn tool_without_languages(dir: &std::path::Path) -> (Write, std::path::PathBuf) {
        (Write::new(Sandbox::new(dir).unwrap()), dir.to_path_buf())
    }
}
