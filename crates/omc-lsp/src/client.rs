//! One running language server, spoken to over its standard input and output.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use omc_tools::{Diagnostic, Severity};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::{Notify, oneshot};
use tokio::time::Instant;

use crate::spec::Spec;
use crate::text::{column, file_uri, offset};

/// How long a server may take to answer the request that starts it.
const INITIALIZE_WAIT: Duration = Duration::from_secs(30);

/// How long a server that has settled must stay quiet before what it said is taken as its answer, since one that has just been told a file was saved may begin checking the project a moment after saying what it found without checking.
const QUIET: Duration = Duration::from_millis(400);

/// What a server last said is wrong with one file.
#[derive(Clone, Debug, Default)]
struct Published {
    /// The version of the file it is about, when the server says.
    version: Option<i64>,
    /// How many times the server had said what is wrong with a file or begun some work before this, counting this, so that a newer word can be told from an older one and from work begun since.
    generation: u64,
    /// What it said, as it said it.
    diagnostics: Vec<Value>,
}

/// A file as the server was last told it.
struct Document {
    /// The version it was told.
    version: i64,
    /// The text it was told.
    text: String,
}

/// What a server has said and waits on, which the task reading it fills in.
#[derive(Default)]
struct Heard {
    /// The answers still awaited, by request id.
    pending: Mutex<HashMap<i64, oneshot::Sender<Result<Value, String>>>>,
    /// What the server last said is wrong with each file, by address.
    published: Mutex<HashMap<String, Published>>,
    /// How many times the server has said what is wrong with a file or begun some work, so that what it says or does can be put in order.
    generation: AtomicU64,
    /// The generation at which the server last began some work, such as loading the project or checking it.
    began: AtomicU64,
    /// The work the server has begun and not yet ended, by the token it reports progress under.
    working: Mutex<HashSet<String>>,
    /// Woken each time the server says what is wrong with a file.
    news: Notify,
    /// Whether the server has stopped talking, having exited or closed its output.
    stopped: AtomicBool,
}

/// A language server that has been started and told what it works on.
pub(crate) struct Client {
    /// Where messages to the server go.
    writer: Arc<tokio::sync::Mutex<ChildStdin>>,
    /// What the server has said and waits on.
    heard: Arc<Heard>,
    /// The id the next request gets.
    next: AtomicI64,
    /// The files the server has been told about, by address.
    documents: Mutex<HashMap<String, Document>>,
    /// Whether the server formats whole files.
    formats: bool,
    /// The server's process, which is killed when the client is dropped.
    _child: Child,
}

impl Client {
    /// Starts the server `spec` describes for the project at `root`, and waits until it is ready to be told about files.
    ///
    /// # Errors
    ///
    /// Returns why when no program of the spec is found, it cannot be started, or it does not answer the starting request.
    pub(crate) async fn start(spec: &Spec, root: &Path) -> Result<Self, String> {
        let (program, arguments) = spec
            .command()
            .ok_or_else(|| format!("no program for the {} language server was found", spec.id))?;
        let mut child = Command::new(&program)
            .args(&arguments)
            .current_dir(root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .map_err(|error| format!("{} could not be started: {error}", program.display()))?;
        let stdin = child.stdin.take().ok_or("the server has no input")?;
        let stdout = child.stdout.take().ok_or("the server has no output")?;
        let writer = Arc::new(tokio::sync::Mutex::new(stdin));
        let heard = Arc::new(Heard::default());
        tokio::spawn(listen(stdout, writer.clone(), heard.clone()));
        let mut client = Self {
            writer,
            heard,
            next: AtomicI64::new(1),
            documents: Mutex::new(HashMap::new()),
            formats: false,
            _child: child,
        };
        let answer = client
            .request("initialize", initialize_params(root), INITIALIZE_WAIT)
            .await?;
        let provider = &answer["capabilities"]["documentFormattingProvider"];
        client.formats = provider.as_bool().unwrap_or(provider.is_object());
        client.notify("initialized", json!({})).await?;
        Ok(client)
    }

    /// Whether the server has said what is wrong with any file yet, which it does once it has loaded the project.
    pub(crate) fn has_spoken(&self) -> bool {
        !lock(&self.heard.published).is_empty()
    }

    /// Whether the server is still running, as far as can be told: it has not closed its output.
    pub(crate) fn is_running(&self) -> bool {
        !self.heard.stopped.load(Ordering::SeqCst)
    }

    /// Sends the server the request `method` with `params` and waits up to `wait` for its answer.
    ///
    /// # Errors
    ///
    /// Returns why when the request cannot be sent, the server answers with an error, stops, or does not answer in time.
    async fn request(&self, method: &str, params: Value, wait: Duration) -> Result<Value, String> {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let (answer, answered) = oneshot::channel();
        lock(&self.heard.pending).insert(id, answer);
        let sent = send(
            &self.writer,
            &json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}),
        )
        .await;
        if let Err(error) = sent {
            lock(&self.heard.pending).remove(&id);
            return Err(error);
        }
        match tokio::time::timeout(wait, answered).await {
            Ok(Ok(answer)) => answer,
            Ok(Err(_)) => Err("the language server stopped".to_owned()),
            Err(_) => {
                lock(&self.heard.pending).remove(&id);
                Err(format!(
                    "the language server did not answer {method} in time"
                ))
            }
        }
    }

    /// Tells the server `method` with `params`, which it does not answer.
    ///
    /// # Errors
    ///
    /// Returns why when the message cannot be sent.
    async fn notify(&self, method: &str, params: Value) -> Result<(), String> {
        send(
            &self.writer,
            &json!({"jsonrpc": "2.0", "method": method, "params": params}),
        )
        .await
    }

    /// Tells the server that the file at `path`, in the language `language`, now holds `text`, opening it on the server the first time and saving it, so that servers that check on save check it too.
    ///
    /// Returns the file's address and version, whether the text differs from what the server was last told, and how many times the server had said what is wrong with a file by then.
    ///
    /// # Errors
    ///
    /// Returns why when the server cannot be told.
    async fn sync(
        &self,
        path: &Path,
        language: &str,
        text: &str,
    ) -> Result<(String, i64, bool, u64), String> {
        let uri = file_uri(path);
        let generation = self.heard.generation.load(Ordering::SeqCst);
        let told = {
            let mut documents = lock(&self.documents);
            match documents.get_mut(&uri) {
                Some(document) if document.text == text => None,
                Some(document) => {
                    document.version += 1;
                    document.text = text.to_owned();
                    Some((
                        document.version,
                        "textDocument/didChange",
                        json!({
                            "textDocument": {"uri": uri, "version": document.version},
                            "contentChanges": [{"text": text}],
                        }),
                    ))
                }
                None => {
                    documents.insert(
                        uri.clone(),
                        Document {
                            version: 1,
                            text: text.to_owned(),
                        },
                    );
                    Some((
                        1,
                        "textDocument/didOpen",
                        json!({
                            "textDocument": {
                                "uri": uri,
                                "languageId": language,
                                "version": 1,
                                "text": text,
                            },
                        }),
                    ))
                }
            }
        };
        let Some((version, method, params)) = told else {
            let version = lock(&self.documents)
                .get(&uri)
                .map_or(1, |document| document.version);
            return Ok((uri, version, false, generation));
        };
        self.notify(method, params).await?;
        self.notify(
            "textDocument/didSave",
            json!({"textDocument": {"uri": uri}}),
        )
        .await?;
        Ok((uri, version, true, generation))
    }

    /// `text`, the text of the file at `path` in the language `language`, formatted by the server; nothing when the server does not format, fails to, or leaves it as it is.
    pub(crate) async fn format(
        &self,
        path: &Path,
        language: &str,
        text: &str,
        wait: Duration,
    ) -> Option<String> {
        if !self.formats {
            return None;
        }
        let (uri, ..) = self.sync(path, language, text).await.ok()?;
        let edits = self
            .request(
                "textDocument/formatting",
                json!({
                    "textDocument": {"uri": uri},
                    "options": {
                        "tabSize": 4,
                        "insertSpaces": true,
                        "trimTrailingWhitespace": true,
                        "insertFinalNewline": true,
                    },
                }),
                wait,
            )
            .await
            .ok()?;
        let formatted = apply_edits(text, edits.as_array()?)?;
        (formatted != text).then_some(formatted)
    }

    /// Tells the server that the file at `path`, in the language `language`, holds `text`, and waits until `deadline` for it to say what is wrong with that text; nothing when it says nothing new by then.
    pub(crate) async fn diagnostics(
        &self,
        path: &Path,
        language: &str,
        text: &str,
        deadline: Instant,
    ) -> Vec<Diagnostic> {
        let Ok((uri, version, changed, since)) = self.sync(path, language, text).await else {
            return Vec::new();
        };
        let fresh = |published: &Published| {
            (!changed || published.generation > since)
                && published.version.is_none_or(|said| said >= version)
        };
        let read = |published: Published| -> Vec<Diagnostic> {
            published
                .diagnostics
                .iter()
                .filter_map(|raw| diagnostic(raw, path, text))
                .collect()
        };
        loop {
            let news = self.heard.news.notified();
            tokio::pin!(news);
            news.as_mut().enable();
            let latest = lock(&self.heard.published).get(&uri).cloned().filter(fresh);
            if let Some(published) = latest
                .clone()
                .filter(|published| self.is_settled(published))
            {
                let quiet_until = deadline.min(Instant::now() + QUIET);
                if tokio::time::timeout_at(quiet_until, news).await.is_err() {
                    return read(published);
                }
                continue;
            }
            if tokio::time::timeout_at(deadline, news).await.is_err() {
                return latest.map(read).unwrap_or_default();
            }
        }
    }

    /// Whether `published` is the server's last word for now: it is not working on anything, and said `published` after it last began some work, so that what it said while loading or before checking is not taken for what it finds once done.
    fn is_settled(&self, published: &Published) -> bool {
        is_last_word(
            published,
            &lock(&self.heard.working),
            self.heard.began.load(Ordering::SeqCst),
        )
    }
}

/// Whether `published` is a server's last word for now, given the work it is still doing, `working`, and the generation at which it last `began` some: it is doing nothing, and said `published` after it began its last work.
fn is_last_word(published: &Published, working: &HashSet<String>, began: u64) -> bool {
    working.is_empty() && published.generation > began
}

/// The parameters of the request that starts a server on the project at `root`: who the client is and what it understands.
fn initialize_params(root: &Path) -> Value {
    let uri = file_uri(root);
    let name = root.file_name().map_or_else(
        || "root".to_owned(),
        |name| name.to_string_lossy().into_owned(),
    );
    json!({
        "processId": std::process::id(),
        "clientInfo": {"name": "crab"},
        "rootUri": uri,
        "rootPath": root,
        "workspaceFolders": [{"uri": uri, "name": name}],
        "capabilities": {
            "general": {"positionEncodings": ["utf-16"]},
            "textDocument": {
                "synchronization": {"didSave": true, "dynamicRegistration": false},
                "publishDiagnostics": {"versionSupport": true, "relatedInformation": false},
                "formatting": {"dynamicRegistration": false},
            },
            "workspace": {"workspaceFolders": true, "configuration": true},
            "window": {"workDoneProgress": true},
        },
    })
}

/// Reads what the server on `stdout` says until it stops: hands each answer to whoever waits on it, keeps what it says is wrong with files in `heard`, and answers its own requests through `writer` as a client that has nothing to tell; the requests still waiting when it stops are failed.
async fn listen(
    stdout: ChildStdout,
    writer: Arc<tokio::sync::Mutex<ChildStdin>>,
    heard: Arc<Heard>,
) {
    let mut reader = BufReader::new(stdout);
    while let Some(message) = read_message(&mut reader).await {
        let method = message["method"].as_str();
        match (method, message.get("id")) {
            (Some(method), Some(id)) => {
                let result = match method {
                    "workspace/configuration" => Value::Array(vec![
                        Value::Null;
                        message["params"]["items"]
                            .as_array()
                            .map_or(0, Vec::len)
                    ]),
                    _ => Value::Null,
                };
                let _ = send(
                    &writer,
                    &json!({"jsonrpc": "2.0", "id": id, "result": result}),
                )
                .await;
            }
            (Some("textDocument/publishDiagnostics"), None) => {
                let params = &message["params"];
                let Some(uri) = params["uri"].as_str() else {
                    continue;
                };
                let published = Published {
                    version: params["version"].as_i64(),
                    generation: heard.generation.fetch_add(1, Ordering::SeqCst) + 1,
                    diagnostics: params["diagnostics"]
                        .as_array()
                        .cloned()
                        .unwrap_or_default(),
                };
                lock(&heard.published).insert(uri.to_owned(), published);
                heard.news.notify_waiters();
            }
            (Some("$/progress"), None) => {
                let params = &message["params"];
                let token = match &params["token"] {
                    Value::String(token) => token.clone(),
                    other => other.to_string(),
                };
                match params["value"]["kind"].as_str() {
                    Some("begin") => {
                        lock(&heard.working).insert(token);
                        heard.began.store(
                            heard.generation.fetch_add(1, Ordering::SeqCst) + 1,
                            Ordering::SeqCst,
                        );
                    }
                    Some("end") => {
                        lock(&heard.working).remove(&token);
                    }
                    _ => continue,
                }
                heard.news.notify_waiters();
            }
            (None, Some(id)) => {
                let Some(id) = id.as_i64() else {
                    continue;
                };
                let Some(waiting) = lock(&heard.pending).remove(&id) else {
                    continue;
                };
                let answer = match message.get("error") {
                    Some(error) => Err(error["message"]
                        .as_str()
                        .unwrap_or("the language server failed")
                        .to_owned()),
                    None => Ok(message.get("result").cloned().unwrap_or(Value::Null)),
                };
                let _ = waiting.send(answer);
            }
            _ => {}
        }
    }
    heard.stopped.store(true, Ordering::SeqCst);
    lock(&heard.working).clear();
    lock(&heard.pending).clear();
    heard.news.notify_waiters();
}

/// The next message the server sends on `reader`, framed by its `Content-Length` header; nothing once the server stops or sends something that is not a message.
async fn read_message(reader: &mut BufReader<ChildStdout>) -> Option<Value> {
    let mut length = None;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).await.ok()? == 0 {
            return None;
        }
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':')
            && name.trim().eq_ignore_ascii_case("content-length")
        {
            length = value.trim().parse::<usize>().ok();
        }
    }
    let mut body = vec![0; length?];
    reader.read_exact(&mut body).await.ok()?;
    serde_json::from_slice(&body).ok()
}

/// Sends `message` to the server through `writer`, framed by its `Content-Length` header.
///
/// # Errors
///
/// Returns why when it cannot be written.
async fn send(writer: &tokio::sync::Mutex<ChildStdin>, message: &Value) -> Result<(), String> {
    let body = message.to_string();
    let mut writer = writer.lock().await;
    let framed = format!("Content-Length: {}\r\n\r\n{body}", body.len());
    writer
        .write_all(framed.as_bytes())
        .await
        .map_err(|error| format!("the language server could not be written to: {error}"))?;
    writer
        .flush()
        .await
        .map_err(|error| format!("the language server could not be written to: {error}"))
}

/// `text` with the server's text edits `edits` made to it, each placed by where it falls in `text`; nothing when one of them is not an edit or two overlap.
fn apply_edits(text: &str, edits: &[Value]) -> Option<String> {
    let mut placed = Vec::with_capacity(edits.len());
    for edit in edits {
        let range = &edit["range"];
        let at = |end: &str| {
            let place = &range[end];
            Some(offset(
                text,
                u32::try_from(place["line"].as_u64()?).ok()?,
                u32::try_from(place["character"].as_u64()?).ok()?,
            ))
        };
        let (start, end) = (at("start")?, at("end")?);
        if start > end {
            return None;
        }
        placed.push((start, end, edit["newText"].as_str()?));
    }
    placed.sort_by_key(|(start, end, _)| (*start, *end));
    if placed.windows(2).any(|pair| pair[0].1 > pair[1].0) {
        return None;
    }
    let mut formatted = text.to_owned();
    for (start, end, new) in placed.into_iter().rev() {
        formatted.replace_range(start..end, new);
    }
    Some(formatted)
}

/// The diagnostic the server said as `raw` about the file at `path`, which holds `text`; nothing when `raw` is not one.
fn diagnostic(raw: &Value, path: &Path, text: &str) -> Option<Diagnostic> {
    let start = &raw["range"]["start"];
    let line = u32::try_from(start["line"].as_u64()?).ok()?;
    let character = u32::try_from(start["character"].as_u64()?).ok()?;
    let severity = match raw["severity"].as_u64() {
        Some(2) => Severity::Warning,
        Some(3) => Severity::Information,
        Some(4) => Severity::Hint,
        _ => Severity::Error,
    };
    let code = match &raw["code"] {
        Value::String(code) => Some(code.clone()),
        Value::Number(code) => Some(code.to_string()),
        _ => None,
    };
    Some(Diagnostic {
        path: path.to_path_buf(),
        line: line + 1,
        column: column(text, line, character),
        severity,
        message: raw["message"].as_str()?.to_owned(),
        source: raw["source"].as_str().map(str::to_owned),
        code,
    })
}

/// Holds `mutex`, whether or not a holder panicked.
fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|held| held.into_inner())
}

#[cfg(test)]
/// Tests for reading what servers say.
mod tests {
    use super::*;

    /// Edits are placed by their positions in the original text, last first, and overlapping ones are refused.
    #[test]
    fn applies_edits_to_the_original_text() {
        let text = "fn  main( ){\n}\n";
        let edits = json!([
            {"range": {"start": {"line": 0, "character": 2}, "end": {"line": 0, "character": 4}}, "newText": " "},
            {"range": {"start": {"line": 0, "character": 9}, "end": {"line": 0, "character": 10}}, "newText": ""},
            {"range": {"start": {"line": 0, "character": 11}, "end": {"line": 1, "character": 1}}, "newText": " {}"},
        ]);
        assert_eq!(
            apply_edits(text, edits.as_array().unwrap()).as_deref(),
            Some("fn main() {}\n")
        );
        let overlapping = json!([
            {"range": {"start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 5}}, "newText": ""},
            {"range": {"start": {"line": 0, "character": 3}, "end": {"line": 0, "character": 6}}, "newText": ""},
        ]);
        assert_eq!(apply_edits(text, overlapping.as_array().unwrap()), None);
    }

    /// What a server says while it works, or before work it began since, is not its last word; what it says after its work ended is.
    #[test]
    fn waits_for_the_server_to_finish_its_work() {
        let said_at = |generation| Published {
            generation,
            ..Published::default()
        };
        let indexing = HashSet::from(["indexing".to_owned()]);
        assert!(!is_last_word(&said_at(3), &indexing, 2));
        assert!(!is_last_word(&said_at(3), &HashSet::new(), 4));
        assert!(is_last_word(&said_at(5), &HashSet::new(), 4));
        assert!(is_last_word(&said_at(1), &HashSet::new(), 0));
    }

    /// A diagnostic is placed by line and character, both counted from one, with its severity, code and source.
    #[test]
    fn reads_a_diagnostic() {
        let raw = json!({
            "range": {"start": {"line": 1, "character": 4}, "end": {"line": 1, "character": 7}},
            "severity": 2,
            "code": 6133,
            "source": "ts",
            "message": "'x' is declared but never used."
        });
        let found = diagnostic(&raw, Path::new("/w/a.ts"), "let a;\nlet x = 1;\n").unwrap();
        assert_eq!((found.line, found.column), (2, 5));
        assert_eq!(found.severity, Severity::Warning);
        assert_eq!(found.code.as_deref(), Some("6133"));
        assert_eq!(found.source.as_deref(), Some("ts"));
    }
}
