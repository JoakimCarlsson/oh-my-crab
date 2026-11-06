//! The `read` tool: numbered lines of a text file or an outline of a large
//! code file, a directory listing, an image, the text of a PDF, a web page,
//! the tables of a SQLite database, the members of an archive, or a path
//! query on a JSON file.

mod archive;
mod container;
mod guard;
mod json;
mod lines;
mod listing;
mod media;
mod outline;
mod selector;
mod shrink;
mod sqlite;
mod web;

use std::io::{BufReader, Read as _};
use std::path::{Path, PathBuf};

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};

use super::snapshot::tag_file;
use super::{conflict, lookalike};
use crate::tool::parse_args;
use crate::{Sandbox, Tool, ToolError, ToolOutput};
use media::Media;
pub(crate) use outline::{Fold, folds};
use selector::Selector;

/// Bytes at the start of a file inspected to tell text, images, PDFs and other
/// binary files apart.
const SNIFF_BYTES: usize = 8192;

/// Reads files, directories, images, PDFs and web pages.
pub struct Read {
    /// The directory paths are confined to.
    sandbox: Sandbox,
    /// Whether a bare read of a large code file gives an outline rather than its text.
    outline: bool,
}

impl Read {
    /// Creates the tool confined to `sandbox`, giving outlines of large code files.
    pub fn new(sandbox: Sandbox) -> Self {
        Self::with_outline(sandbox, true)
    }

    /// Creates the tool confined to `sandbox`, giving outlines of large code
    /// files read without a selector when `outline` is set and their text,
    /// like any other file, when it is not.
    pub fn with_outline(sandbox: Sandbox, outline: bool) -> Self {
        Self { sandbox, outline }
    }

    /// Resolves the `path` argument to a file inside the sandbox, the name it is
    /// shown by, and the selector after it.
    ///
    /// A file whose whole name exists wins over reading a `:` suffix as a
    /// selector, so names that contain colons stay readable. Files the tools
    /// saved long output in are readable wherever they are.
    ///
    /// # Errors
    ///
    /// Returns [`ToolError::OutsidePath`] when the path leaves the sandbox and
    /// is not saved output.
    fn locate(&self, raw: &str) -> Result<(PathBuf, String, Selector), ToolError> {
        let whole = self.sandbox.resolve_readable(raw);
        if let Ok(path) = &whole
            && path.exists()
        {
            return Ok((path.clone(), raw.to_string(), Selector::default()));
        }
        if let Some((name, selector)) = selector::split(raw) {
            return Ok((
                self.sandbox.resolve_readable(name)?,
                name.to_string(),
                selector,
            ));
        }
        Ok((whole?, raw.to_string(), Selector::default()))
    }
}

/// Arguments of a `read` call.
#[derive(Deserialize)]
struct Args {
    /// File, directory or URL to read, optionally followed by a selector such
    /// as `:50-200`.
    path: String,
    /// First line to return, 1-indexed; kept for callers that predate selectors.
    offset: Option<usize>,
    /// Most lines to return; kept for callers that predate selectors.
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

    /// Fetching a URL reaches the network, so the user approves it, per origin:
    /// allowing one site always never allows another. Reading files does not
    /// ask.
    fn approval_scope(&self, args: &Value) -> Option<String> {
        let path = args["path"].as_str().filter(|path| web::is_url(path))?;
        Some(format!("read {}", web::scope(path)))
    }

    /// Describes the tool to the model.
    fn description(&self) -> &'static str {
        "Read a file, directory, image, PDF, SQLite database, archive or web page.\n\
         - Text files: lines prefixed with line numbers, under a header with the file's snapshot \
         tag; pass that tag and those line numbers to edit.\n\
         - Pick lines with a suffix on the path: file:50 or file:50- (from line 50), \
         file:50-200 (inclusive), file:50+150 (150 lines from 50), file:-60 (last 60), \
         file:5-16,960-973 (several ranges). Reads stop at 2000 lines or 50 KB with a note \
         saying how to continue.\n\
         - Code files of 200 lines or more (Rust, Go, TypeScript, JavaScript, Python, C, C++, \
         Java, Lua) read without a selector come back as an outline: signatures, types, \
         imports and comments keep their real line numbers, and each long function body is \
         folded to one unnumbered line such as `… 30 lines folded (120-149)`. A footer lists \
         the folded ranges. Never guess what a fold holds: read it, e.g. file:120-149, before \
         editing inside it; an edit with the outline's tag that touches a folded line is \
         refused. file:1- reads the file as it is.\n\
         - Merge conflicts: when lines with conflict markers are shown, a note lists every \
         conflict of the file by number with its lines and side labels; file:conflicts shows \
         only the conflicts. Resolve one with edit's resolve operation and the read's tag.\n\
         - A path that does not exist but is the end of exactly one file's path (or matches it \
         ignoring case) reads that file, and the result says so; several candidates are \
         listed in the error.\n\
         - Directories: entries, directories first and ending in /, honoring .gitignore.\n\
         - Images (PNG, JPEG, GIF, WebP): returned for you to look at. One over 3.75 MB or \
         2000 px on a side is shrunk first; the header then gives the original size and \
         the factor to multiply coordinates by to map them back to it. Images over 12000 px \
         on a side or 256 MiB of pixels cannot be shrunk and are refused.\n\
         - PDFs: extracted text, a [page N] line before each page; line suffixes work.\n\
         - SQLite databases: db.sqlite lists tables with row counts and schemas; \
         db.sqlite:users shows rows (50 at a time; db.sqlite:users:51-100 or :-20 for \
         others); db.sqlite:SELECT ... runs a read-only query (SELECT, WITH or PRAGMA \
         table_info only), paged the same way.\n\
         - Archives (.zip, .tar, .tar.gz, .tgz): a.zip lists members with sizes; \
         a.zip:dir/file.txt reads a member as numbered text, with line suffixes such as \
         a.zip:dir/file.txt:10-40; a.zip:dir lists a directory. Members have no tag and \
         cannot be edited.\n\
         - JSON: data.json:<query> gives the values a query picks, pretty-printed and \
         numbered. A query is a small path language, not jq: a path such as .items[0].name, \
         .items[].id, .[\"a key\"], .[-1] or .[2:5], piped with | through more paths or the \
         builtins keys, length, type, first, last and select(.field == literal) (or !=), as \
         in data.json:.items[] | select(.kind == \"x\") | .id. Nothing else is supported. Add \
         a line suffix to page the output (data.json:.items:100-200). data.json:50-80 reads \
         lines as text.\n\
         - http:// and https:// URLs: the page as Markdown with numbered lines; url:raw for \
         the original HTML. The user approves fetches per site. Redirects to another site \
         are not followed: read the URL the error names. A host name that resolves to a \
         local or private address is refused; only read localhost or a local address when \
         the user asks for one."
    }

    /// Returns the argument schema.
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "File or directory path relative to the working directory, or an http(s) URL, optionally with a selector suffix: src/main.rs:120-180, log.txt:-50, src/lib.rs:conflicts, https://example.com/docs:raw, app.db:users, src.zip:lib/a.rs, data.json:.items[0]."
                }
            },
            "required": ["path"]
        })
    }

    /// Shows the path or URL being read.
    fn describe(&self, args: &Value) -> String {
        let path = args["path"].as_str().unwrap_or("?");
        match web::is_url(path).then(|| web::target(path)) {
            Some(Ok((url, _))) => url.to_string(),
            _ => path.to_string(),
        }
    }

    /// Fetches a URL, or reads a path on a blocking thread: inside a
    /// database, archive or JSON file when the path names one, as a file or
    /// directory otherwise.
    async fn call(&self, args: Value) -> Result<ToolOutput, ToolError> {
        let args: Args = parse_args(args)?;
        if web::is_url(&args.path) {
            let (url, selector) = web::target(&args.path)?;
            let selector = selector.windowed(args.offset, args.limit)?;
            return web::read(url.as_str(), selector).await;
        }
        if let Some(target) = container::locate(&self.sandbox, &args.path) {
            return container::read_bounded(target, args.offset, args.limit).await;
        }
        let (path, label, selector) = self.locate(&args.path)?;
        let selector = selector.windowed(args.offset, args.limit)?;
        let outline = self.outline;
        let sandbox = self.sandbox.clone();
        tokio::task::spawn_blocking(move || {
            let (path, label, note) = lookalike::redirect(&sandbox, path, label)?;
            read_path(&path, &label, &selector, outline)
                .map(|output| lookalike::noted(output, note))
        })
        .await
        .map_err(|e| ToolError::Failed(e.to_string()))?
    }
}

/// Reads the file or directory at `path`, shown as `label`, as `selector` asks;
/// a code file read without a selector comes back as an outline when `outline`
/// is set and the file is large enough for one.
///
/// # Errors
///
/// Returns [`ToolError`] when the path does not exist or cannot be read, holds
/// binary data `read` cannot show, or the selector does not apply to it.
fn read_path(
    path: &Path,
    label: &str,
    selector: &Selector,
    outline: bool,
) -> Result<ToolOutput, ToolError> {
    if !path.exists() {
        return Err(ToolError::Failed(format!("{label} does not exist")));
    }
    if selector.raw {
        return Err(ToolError::InvalidArguments(
            "the raw selector applies to URLs, not files".to_string(),
        ));
    }
    if path.is_dir() {
        if selector.picks_lines() {
            return Err(ToolError::InvalidArguments(format!(
                "{label} is a directory; line selectors apply to files"
            )));
        }
        return Ok(ToolOutput::new(listing::list(path, label)));
    }
    let mut head = Vec::with_capacity(SNIFF_BYTES);
    std::fs::File::open(path)?
        .take(SNIFF_BYTES as u64)
        .read_to_end(&mut head)?;
    match media::sniff(&head) {
        Some(Media::Image(media_type)) => {
            if selector.picks_lines() {
                return Err(ToolError::InvalidArguments(format!(
                    "{label} is an image; line selectors apply to text"
                )));
            }
            media::check_image_size(file_size(path)?, label)?;
            media::image(&std::fs::read(path)?, media_type, label)
        }
        Some(Media::Pdf) => {
            if selector.conflicts {
                return Err(ToolError::InvalidArguments(format!(
                    "{label} is a PDF; the conflicts selector applies to text files"
                )));
            }
            media::check_pdf_size(file_size(path)?, label)?;
            let (text, pages) = media::pdf_text(&std::fs::read(path)?, label)?;
            let numbered = lines::number(
                || Ok(std::io::Cursor::new(text.as_bytes())),
                selector,
                label,
            )?;
            Ok(ToolOutput::new(format!(
                "[pdf {label}, {}]\n{numbered}",
                media::pages(pages)
            )))
        }
        None if head.contains(&0) => Err(lines::not_text(label)),
        None => {
            let (conflicts, text) = conflict::scan_file(path);
            if selector.conflicts {
                let header = format!("[tag {}]\n", tag_file(path)?);
                return Ok(ToolOutput::new(conflict::listing(
                    &header, label, &text, &conflicts,
                )));
            }
            if outline
                && conflicts.is_empty()
                && !selector.picks_lines()
                && let Some(text) = outline::read(path, label)?
            {
                return Ok(ToolOutput::new(text));
            }
            let header = format!("[tag {}]\n", tag_file(path)?);
            let mut numbered = lines::number(
                || std::fs::File::open(path).map(BufReader::new),
                selector,
                label,
            )?;
            if !conflicts.is_empty() && conflict::shows_markers(&numbered) {
                numbered.push_str(&conflict::note(label, &conflicts));
            }
            Ok(ToolOutput::new(header + &numbered))
        }
    }
}

/// The size of the file at `path` in bytes, saturating on systems where it
/// does not fit a `usize`.
///
/// # Errors
///
/// Returns the I/O error when the file's metadata cannot be read.
fn file_size(path: &Path) -> std::io::Result<usize> {
    Ok(usize::try_from(std::fs::metadata(path)?.len()).unwrap_or(usize::MAX))
}

#[cfg(test)]
mod tests {
    use super::super::snapshot::tag;
    use super::*;
    use crate::testutil::scratch;

    /// Builds a `read` tool rooted at the scratch directory `name`.
    fn tool(name: &str) -> (Read, PathBuf) {
        let dir = scratch(name);
        (Read::new(Sandbox::new(&dir).unwrap()), dir)
    }

    /// Reads `path` with `tool` and returns the text, panicking on failure.
    async fn text(tool: &Read, path: &str) -> String {
        tool.call(json!({ "path": path })).await.unwrap().text
    }

    /// Lines come back numbered under the tag, and offset and limit still pick a window.
    #[tokio::test]
    async fn numbers_lines_and_honors_offset_and_limit() {
        let (tool, dir) = tool("read-window");
        std::fs::write(dir.join("f.txt"), "a\nb\nc\nd\n").unwrap();
        let header = format!("[tag {}]\n", tag(b"a\nb\nc\nd\n"));
        assert_eq!(
            text(&tool, "f.txt").await,
            format!("{header}     1\ta\n     2\tb\n     3\tc\n     4\td\n")
        );
        let part = tool
            .call(json!({ "path": "f.txt", "offset": 2, "limit": 2 }))
            .await
            .unwrap();
        assert_eq!(part.text, format!("{header}     2\tb\n     3\tc\n"));
    }

    /// Selector suffixes pick lines and keep the tag of the whole file.
    #[tokio::test]
    async fn honors_selector_suffixes() {
        let (tool, dir) = tool("read-selectors");
        let content = "1\n2\n3\n4\n5\n6\n7\n8\n";
        std::fs::write(dir.join("f.txt"), content).unwrap();
        let header = format!("[tag {}]\n", tag(content.as_bytes()));
        assert_eq!(
            text(&tool, "f.txt:7").await,
            format!("{header}     7\t7\n     8\t8\n")
        );
        assert_eq!(
            text(&tool, "f.txt:2-3").await,
            format!("{header}     2\t2\n     3\t3\n")
        );
        assert_eq!(
            text(&tool, "f.txt:4+1").await,
            format!("{header}     4\t4\n")
        );
        assert_eq!(
            text(&tool, "f.txt:-1").await,
            format!("{header}     8\t8\n")
        );
        assert_eq!(
            text(&tool, "f.txt:1-1,8-").await,
            format!("{header}     1\t1\n…\n     8\t8\n")
        );
        let both = tool.call(json!({ "path": "f.txt:1-2", "offset": 3 })).await;
        assert!(matches!(both, Err(ToolError::InvalidArguments(_))));
    }

    /// A file whose name ends in what looks like a selector is read whole.
    ///
    /// Unix only: on Windows a colon in a name opens an alternate data stream, so such a file cannot exist.
    #[cfg(unix)]
    #[tokio::test]
    async fn prefers_existing_names_with_colons() {
        let (tool, dir) = tool("read-colon");
        std::fs::write(dir.join("notes:2"), "x\ny\n").unwrap();
        std::fs::write(dir.join("notes"), "p\nq\n").unwrap();
        assert!(
            text(&tool, "notes:2")
                .await
                .ends_with("     1\tx\n     2\ty\n")
        );
        assert!(text(&tool, "notes:2-2").await.ends_with("     2\tq\n"));
    }

    /// A missing file with a selector is reported by its name without the selector.
    #[tokio::test]
    async fn names_a_missing_file_without_its_selector() {
        let (tool, _dir) = tool("read-missing");
        let missing = tool.call(json!({ "path": "gone.txt:5" })).await;
        assert!(
            matches!(missing, Err(ToolError::Failed(m)) if m.contains("gone.txt does not exist"))
        );
    }

    /// Lines showing conflict markers get a note listing every conflict of the
    /// file; lines without them do not; `:conflicts` shows only the blocks.
    #[tokio::test]
    async fn notes_and_lists_merge_conflicts() {
        use super::super::conflict::tests::TWO;
        let (tool, dir) = tool("read-conflicts");
        std::fs::write(dir.join("f.txt"), TWO).unwrap();
        let whole = text(&tool, "f.txt").await;
        assert!(whole.contains("[2 merge conflicts in f.txt:\n#1 lines 2-7: ours \"HEAD\" (line 3), theirs \"feature\" (lines 5-6)\n#2 lines 9-14:"), "{whole}");
        assert!(whole.contains("\"op\": \"resolve\""), "{whole}");
        assert!(!text(&tool, "f.txt:1-1").await.contains("merge conflict"));
        let listed = text(&tool, "f.txt:conflicts").await;
        assert!(
            listed.starts_with(&format!(
                "[tag {}]\n[2 merge conflicts in f.txt]\n\n#1 lines 2-7",
                tag(TWO.as_bytes())
            )),
            "{listed}"
        );
        assert!(listed.contains("     2\t<<<<<<< HEAD\n"), "{listed}");
        assert!(
            !listed.contains("\ttop\n") && !listed.contains("\tmiddle\n"),
            "{listed}"
        );
        std::fs::write(dir.join("clean.txt"), "a\n").unwrap();
        assert!(
            text(&tool, "clean.txt:conflicts")
                .await
                .ends_with("[no merge conflicts in clean.txt]\n")
        );
        let windowed = tool
            .call(json!({ "path": "f.txt:conflicts", "offset": 2 }))
            .await;
        assert!(matches!(windowed, Err(ToolError::InvalidArguments(_))));
    }

    /// A large code file with conflicts is read as text, not as an outline.
    #[tokio::test]
    async fn reads_conflicted_code_as_text() {
        let (tool, dir) = tool("read-conflicts-outline");
        let code = outline::tests::rust_source(20, 10);
        let conflicted = format!("<<<<<<< HEAD\n// a\n=======\n// b\n>>>>>>> other\n{code}");
        std::fs::write(dir.join("a.rs"), &conflicted).unwrap();
        let out = text(&tool, "a.rs").await;
        assert!(!out.contains("[outline"), "{out}");
        assert!(out.contains("[1 merge conflict in a.rs:"), "{out}");
    }

    /// A missing path that ends exactly one file's path reads that file and
    /// says so; several candidates are listed in the error.
    #[tokio::test]
    async fn reads_the_one_file_a_mistyped_path_means() {
        let (tool, dir) = tool("read-lookalike");
        std::fs::create_dir_all(dir.join("crates/a/src")).unwrap();
        std::fs::create_dir_all(dir.join("crates/b/src")).unwrap();
        std::fs::write(dir.join("crates/a/src/lib.rs"), "a\nb\n").unwrap();
        std::fs::write(dir.join("crates/b/src/lib.rs"), "c\n").unwrap();
        let out = text(&tool, "a/src/lib.rs:2").await;
        assert!(
            out.starts_with("[a/src/lib.rs does not exist; showing crates/a/src/lib.rs, the only file whose path ends with it]\n[tag "),
            "{out}"
        );
        assert!(out.ends_with("     2\tb\n"), "{out}");
        assert!(
            text(&tool, "CRATES/B/SRC/LIB.RS")
                .await
                .contains("crates/b/src/lib.rs")
        );
        let several = tool.call(json!({ "path": "src/lib.rs" })).await;
        assert!(
            matches!(&several, Err(ToolError::Failed(m)) if m.contains("crates/a/src/lib.rs, crates/b/src/lib.rs")),
            "{several:?}"
        );
    }

    /// Binary files are rejected.
    #[tokio::test]
    async fn rejects_binary_files() {
        let (tool, dir) = tool("read-binary");
        std::fs::write(dir.join("b.bin"), [0u8, 1, 2, 3]).unwrap();
        let result = tool.call(json!({ "path": "b.bin" })).await;
        assert!(matches!(result, Err(ToolError::Failed(m)) if m.contains("binary")));
    }

    /// Paths outside the working directory are rejected, with or without a selector.
    #[tokio::test]
    async fn rejects_path_escape() {
        let (tool, _dir) = tool("read-escape");
        for path in ["../../Cargo.toml", "../../Cargo.toml:1-5", "/etc/passwd:1"] {
            let result = tool.call(json!({ "path": path })).await;
            assert!(matches!(result, Err(ToolError::OutsidePath(_))), "{path}");
        }
    }

    /// Output is capped at the byte limit with a continuation note.
    #[tokio::test]
    async fn caps_output_size() {
        let (tool, dir) = tool("read-cap");
        let line = "x".repeat(100);
        std::fs::write(dir.join("big.txt"), format!("{line}\n").repeat(2000)).unwrap();
        let out = text(&tool, "big.txt").await;
        assert!(out.len() < lines::MAX_BYTES + 1000);
        assert!(out.contains("more lines remain; continue with big.txt:"));
    }

    /// A bare read of a large code file gives an outline under its outline tag;
    /// a selector, a small file or a tool built without outlines gives the text.
    #[tokio::test]
    async fn outlines_large_code_files_read_bare() {
        let (tool, dir) = tool("read-outline");
        let code = outline::tests::rust_source(20, 10);
        std::fs::write(dir.join("a.rs"), &code).unwrap();
        std::fs::write(dir.join("a.txt"), &code).unwrap();
        let out = text(&tool, "a.rs").await;
        let header = format!(
            "[tag {}]\n[outline of a.rs, 302 lines: 20 function bodies folded, 220 lines hidden]\n",
            super::super::snapshot::outline_tag(code.as_bytes())
        );
        assert!(out.starts_with(&header), "{out}");
        assert!(
            out.contains("      \t    … 11 lines folded (5-15)\n"),
            "{out}"
        );
        assert!(out.ends_with("a.rs:1- reads the whole text.]\n"), "{out}");
        let whole = format!("[tag {}]\n", tag(code.as_bytes()));
        let ranged = text(&tool, "a.rs:5-6").await;
        assert_eq!(
            ranged,
            format!("{whole}     5\t    let v0 = x + 0;\n     6\t    let v1 = x + 1;\n")
        );
        assert!(text(&tool, "a.rs:1-").await.contains("let v3 = x + 3;"));
        assert!(text(&tool, "a.txt").await.starts_with(&whole));
        let verbatim = Read::with_outline(Sandbox::new(&dir).unwrap(), false);
        assert!(text(&verbatim, "a.rs").await.starts_with(&whole));
        std::fs::write(dir.join("small.rs"), outline::tests::rust_source(3, 10)).unwrap();
        assert!(!text(&tool, "small.rs").await.contains("[outline"));
    }

    /// A directory is listed; line selectors on it are refused.
    #[tokio::test]
    async fn lists_directories() {
        let (tool, dir) = tool("read-dir");
        std::fs::create_dir(dir.join("d")).unwrap();
        std::fs::create_dir(dir.join("d/sub")).unwrap();
        std::fs::write(dir.join("d/f.txt"), "").unwrap();
        assert_eq!(
            text(&tool, "d").await,
            "[directory d, 2 entries]\nsub/\nf.txt\n"
        );
        assert!(
            text(&tool, ".")
                .await
                .starts_with("[directory ., 1 entry]\nd/\n")
        );
        assert!(tool.call(json!({ "path": "d:1-2" })).await.is_err());
    }

    /// An image comes back as an image whatever its extension.
    #[tokio::test]
    async fn returns_images() {
        let (tool, dir) = tool("read-image");
        let gif = b"GIF89a\x01\0\x01\0\0\0\0;";
        std::fs::write(dir.join("pic.dat"), gif).unwrap();
        let output = tool.call(json!({ "path": "pic.dat" })).await.unwrap();
        assert_eq!(output.text, "[image pic.dat, image/gif, 14 B]");
        assert_eq!(output.images.len(), 1);
        assert_eq!(output.images[0].media_type, "image/gif");
        assert!(tool.call(json!({ "path": "pic.dat:1" })).await.is_err());
    }

    /// A PDF gives its text, and selectors pick from it.
    #[tokio::test]
    async fn extracts_pdfs() {
        let (tool, dir) = tool("read-pdf");
        std::fs::write(dir.join("doc.pdf"), media::tests::pdf("Quarterly report")).unwrap();
        let out = text(&tool, "doc.pdf").await;
        assert!(
            out.starts_with("[pdf doc.pdf, 1 page]\n     1\t[page 1]\n"),
            "{out}"
        );
        assert!(out.contains("Quarterly report"), "{out}");
        let first = text(&tool, "doc.pdf:1").await;
        assert!(first.contains("[page 1]"), "{first}");
    }

    /// The raw selector is for URLs only.
    #[tokio::test]
    async fn refuses_raw_for_files() {
        let (tool, dir) = tool("read-raw");
        std::fs::write(dir.join("f.html"), "<p>x</p>").unwrap();
        let result = tool.call(json!({ "path": "f.html:raw" })).await;
        assert!(matches!(result, Err(ToolError::InvalidArguments(_))));
    }

    /// Only URL reads ask for approval, scoped to the URL's origin.
    #[test]
    fn asks_approval_for_urls_only_per_origin() {
        let (tool, _dir) = tool("read-approval");
        let scope = |path: &str| tool.approval_scope(&json!({ "path": path }));
        assert_eq!(
            scope("https://example.com/a/b:10-20").as_deref(),
            Some("read https://example.com")
        );
        assert_eq!(
            scope("http://localhost:8080/").as_deref(),
            Some("read http://localhost:8080")
        );
        assert_ne!(scope("https://example.com"), scope("http://example.com"));
        assert_eq!(
            scope("http://user@127.0.0.1:8080/x:raw").as_deref(),
            Some("read http://127.0.0.1:8080")
        );
        assert_eq!(
            scope("http:///evil.example/").as_deref(),
            Some("read http:///evil.example/")
        );
        assert_eq!(scope("src/main.rs"), None);
        assert!(tool.read_only());
    }

    /// A URL is fetched over HTTP and its HTML read as Markdown or, with `raw`, as source.
    #[tokio::test]
    async fn fetches_urls() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let body =
            "<html><head><title>Local</title></head><body><p>Served <i>here</i></p></body></html>";
        let server = tokio::spawn(async move {
            for _ in 0..2 {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut request = vec![0u8; 4096];
                let mut read = 0;
                while !request[..read].windows(4).any(|w| w == b"\r\n\r\n") {
                    read += stream.read(&mut request[read..]).await.unwrap();
                }
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\n\
                     Connection: close\r\n\r\n{body}",
                    body.len()
                );
                stream.write_all(response.as_bytes()).await.unwrap();
            }
        });
        let (tool, _dir) = tool("read-url");
        let url = format!("http://{address}/page");
        let markdown = text(&tool, &url).await;
        assert!(
            markdown.starts_with(&format!("[url {url}, text/html, converted to Markdown]\n")),
            "{markdown}"
        );
        assert!(markdown.contains("Title: Local\n"), "{markdown}");
        assert!(
            markdown.contains("Served _here_") || markdown.contains("Served *here*"),
            "{markdown}"
        );
        let raw = text(&tool, &format!("{url}:raw")).await;
        assert!(raw.contains("<i>here</i>"), "{raw}");
        server.await.unwrap();
    }

    /// Answers one HTTP request on `stream`: a redirect to `/page` for `/same`,
    /// a redirect to `elsewhere` for `/away`, and a small HTML page otherwise.
    async fn answer_redirects(mut stream: tokio::net::TcpStream, elsewhere: &str) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let mut request = vec![0u8; 4096];
        let mut read = 0;
        while !request[..read].windows(4).any(|w| w == b"\r\n\r\n") {
            read += stream.read(&mut request[read..]).await.unwrap();
        }
        let head = String::from_utf8_lossy(&request[..read]).into_owned();
        let response = if head.starts_with("GET /same ") {
            "HTTP/1.1 302 Found\r\nLocation: /page\r\nContent-Length: 0\r\n\
             Connection: close\r\n\r\n"
                .to_owned()
        } else if head.starts_with("GET /away ") {
            format!(
                "HTTP/1.1 302 Found\r\nLocation: {elsewhere}\r\nContent-Length: 0\r\n\
                 Connection: close\r\n\r\n"
            )
        } else {
            let body = "<html><body><p>Landed</p></body></html>";
            format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\n\
                 Connection: close\r\n\r\n{body}",
                body.len()
            )
        };
        stream.write_all(response.as_bytes()).await.unwrap();
    }

    /// Same-origin redirects are followed; a redirect to another origin is not
    /// followed but reported, so that reading its target is approved afresh.
    #[tokio::test]
    async fn follows_only_same_origin_redirects() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let elsewhere = format!("http://localhost:{}/page", address.port());
        let target = elsewhere.clone();
        let server = tokio::spawn(async move {
            for _ in 0..3 {
                let (stream, _) = listener.accept().await.unwrap();
                answer_redirects(stream, &target).await;
            }
        });
        let (tool, _dir) = tool("read-redirect");
        let followed = text(&tool, &format!("http://{address}/same")).await;
        assert!(followed.contains("Landed"), "{followed}");
        assert!(
            followed.contains(&format!("redirected from http://{address}/same")),
            "{followed}"
        );
        let refused = tool
            .call(json!({ "path": format!("http://{address}/away") }))
            .await
            .unwrap_err()
            .to_string();
        assert!(refused.contains(&elsewhere), "{refused}");
        assert!(refused.contains("another origin"), "{refused}");
        server.await.unwrap();
    }
}
