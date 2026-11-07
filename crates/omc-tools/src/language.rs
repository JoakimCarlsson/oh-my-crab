//! Language services: formatting the files the tools write, and saying what is wrong with them.
//!
//! The tools never talk to a language server themselves. They ask a [`LanguageService`], which whoever runs the agent provides: the client the harness talks to when it runs language servers of its own, servers the harness starts itself, or a program embedding the crates with one of its own. When there is none, the tools work as they would without language services at all.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use async_trait::async_trait;
pub use omc_wire::{Diagnostic, Severity};

use crate::navigation::{LanguageError, Location, Lookup, Renamed, Spot, Symbol, cannot_navigate};
use crate::tools::write_atomic;

/// How long a write waits for fresh diagnostics of the files it changed.
pub const DIAGNOSTICS_WAIT: Duration = Duration::from_secs(3);

/// How long a write waits for a file to be formatted.
pub const FORMAT_WAIT: Duration = Duration::from_secs(3);

/// How much longer than it was asked to wait a service is given to answer before it is given up on.
pub(crate) const GRACE: Duration = Duration::from_millis(500);

/// The most diagnostics a tool result shows.
const MAX_SHOWN: usize = 20;

/// The most bytes of a diagnostic's message a tool result shows.
const MAX_MESSAGE: usize = 300;

/// Formats files and says what is wrong with them, as the language servers of their languages do.
#[async_trait]
pub trait LanguageService: Send + Sync {
    /// The whole text of the file at `path`, as it is on disk now, formatted as its language says; nothing when it cannot be formatted, has no formatter or is formatted already.
    async fn format(&self, path: &Path) -> Option<String>;

    /// What is wrong with the files at `paths`, as they are on disk now, having waited up to `wait` for the servers to say what they find in their text; whatever is known by then, which may be nothing.
    async fn diagnostics(&self, paths: &[PathBuf], wait: Duration) -> Vec<Diagnostic>;

    /// How long to wait for the diagnostics of the files at `paths`: [`DIAGNOSTICS_WAIT`], unless a service whose servers are still loading the project, and so would say nothing that soon, asks for longer.
    fn patience(&self, _paths: &[PathBuf]) -> Duration {
        DIAGNOSTICS_WAIT
    }

    /// Whether a language server reads the file at `path`, so that a question about it can be answered at all; by default every file is taken to have one.
    ///
    /// # Errors
    ///
    /// Returns [`LanguageError::NoServer`], saying why, when no server reads such files, or the one that would is not installed or could not be started.
    async fn serves(&self, _path: &Path) -> Result<(), LanguageError> {
        Ok(())
    }

    /// The places `lookup` finds from the symbol at `at`, as the files are on disk now; empty when there are none.
    ///
    /// # Errors
    ///
    /// Returns [`LanguageError::NoServer`] when no server reads the file, [`LanguageError::Unsupported`] when its server or these services do not answer such questions, and [`LanguageError::Failed`] when the server fails. A service that does not navigate code fails this way by default.
    async fn locate(&self, _lookup: Lookup, _at: &Spot) -> Result<Vec<Location>, LanguageError> {
        Err(cannot_navigate())
    }

    /// What the symbol at `at` is, its type and documentation, as plain text or Markdown; nothing when the server has nothing to say about it.
    ///
    /// # Errors
    ///
    /// As [`LanguageService::locate`].
    async fn hover(&self, _at: &Spot) -> Result<Option<String>, LanguageError> {
        Err(cannot_navigate())
    }

    /// The symbols of the file at `path`, outermost first and each followed by those inside it.
    ///
    /// # Errors
    ///
    /// As [`LanguageService::locate`].
    async fn document_symbols(&self, _path: &Path) -> Result<Vec<Symbol>, LanguageError> {
        Err(cannot_navigate())
    }

    /// The symbols of the project whose names match `query`, as the servers of its languages find them, or only the server of the file at `near` when it is given.
    ///
    /// # Errors
    ///
    /// As [`LanguageService::locate`]; [`LanguageError::NoServer`] when no server of the project runs.
    async fn workspace_symbols(
        &self,
        _query: &str,
        _near: Option<&Path>,
    ) -> Result<Vec<Symbol>, LanguageError> {
        Err(cannot_navigate())
    }

    /// The files renaming the symbol at `at` to `new_name` changes, each with its whole new text, as the files are on disk now; nothing is written.
    ///
    /// # Errors
    ///
    /// As [`LanguageService::locate`]; [`LanguageError::Failed`] as well when the server refuses the rename or it would create, move or delete files.
    async fn rename(&self, _at: &Spot, _new_name: &str) -> Result<Vec<Renamed>, LanguageError> {
        Err(cannot_navigate())
    }

    /// Everything the servers of the project have said is wrong with any of its files, having waited up to `wait` for servers still loading or checking it.
    ///
    /// # Errors
    ///
    /// As [`LanguageService::workspace_symbols`].
    async fn all_diagnostics(&self, _wait: Duration) -> Result<Vec<Diagnostic>, LanguageError> {
        Err(cannot_navigate())
    }
}

/// The language service the tools ask, which can be set or taken away while they are in use; none by default.
#[derive(Clone, Default)]
pub struct Languages {
    /// The service, when there is one.
    service: Arc<RwLock<Option<Arc<dyn LanguageService>>>>,
}

impl Languages {
    /// Language services provided by `service`, or none.
    pub fn new(service: Option<Arc<dyn LanguageService>>) -> Self {
        Self {
            service: Arc::new(RwLock::new(service)),
        }
    }

    /// Has the tools ask `service` from now on, or no service at all.
    pub fn set(&self, service: Option<Arc<dyn LanguageService>>) {
        *self
            .service
            .write()
            .unwrap_or_else(|held| held.into_inner()) = service;
    }

    /// The service the tools ask now, when there is one.
    pub fn get(&self) -> Option<Arc<dyn LanguageService>> {
        self.service
            .read()
            .unwrap_or_else(|held| held.into_inner())
            .clone()
    }
}

impl std::fmt::Debug for Languages {
    /// Says whether there is a service.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Languages")
            .field("service", &self.get().is_some())
            .finish()
    }
}

/// A file a tool has just written.
#[derive(Debug)]
pub(crate) struct Written {
    /// Where it is.
    pub(crate) path: PathBuf,
    /// The name it is shown by.
    pub(crate) shown: String,
    /// What it holds now.
    pub(crate) text: String,
    /// Whether the language service formatted it after the tool wrote it.
    pub(crate) formatted: bool,
}

/// Has the files a tool has just written formatted, all at once, writing back each one the formatting changed, and returns what is wrong with them as a block to add to the tool's result; empty when there is no service or nothing is wrong.
///
/// Each file's `text` and `formatted` are brought up to date. A formatted text that cannot be written, or that does not come within [`FORMAT_WAIT`], is left unwritten, and the file stays as the tool wrote it.
pub(crate) async fn settle(languages: &Languages, files: &mut [Written]) -> String {
    let Some(service) = languages.get() else {
        return String::new();
    };
    let formatting = files
        .iter()
        .map(|file| tokio::time::timeout(FORMAT_WAIT + GRACE, service.format(&file.path)));
    let formatted = futures_util::future::join_all(formatting).await;
    for (file, formatted) in files.iter_mut().zip(formatted) {
        if let Some(text) = formatted.ok().flatten().filter(|text| *text != file.text)
            && write_atomic(&file.path, text.as_bytes()).is_ok()
        {
            file.text = text;
            file.formatted = true;
        }
    }
    let paths: Vec<PathBuf> = files.iter().map(|file| file.path.clone()).collect();
    let wait = service.patience(&paths);
    let found = tokio::time::timeout(wait + GRACE, service.diagnostics(&paths, wait))
        .await
        .unwrap_or_default();
    report(files, found)
}

/// The errors and warnings among `found` that are about `files`, worst first, as a block that names each file as it is shown; empty when there are none.
fn report(files: &[Written], found: Vec<Diagnostic>) -> String {
    let mut kept: Vec<(&str, Diagnostic)> = found
        .into_iter()
        .filter(|diagnostic| diagnostic.severity <= Severity::Warning)
        .filter_map(|diagnostic| {
            files
                .iter()
                .find(|file| file.path == diagnostic.path)
                .map(|file| (file.shown.as_str(), diagnostic))
        })
        .collect();
    if kept.is_empty() {
        return String::new();
    }
    kept.sort_by(|(left_file, left), (right_file, right)| {
        left.severity
            .cmp(&right.severity)
            .then_with(|| left_file.cmp(right_file))
            .then_with(|| (left.line, left.column).cmp(&(right.line, right.column)))
    });
    kept.dedup_by(|(left_file, left), (right_file, right)| {
        left_file == right_file
            && left.line == right.line
            && left.column == right.column
            && left.message == right.message
    });
    let errors = kept
        .iter()
        .filter(|(_, diagnostic)| diagnostic.severity == Severity::Error)
        .count();
    let mut out = format!(
        "Diagnostics: {errors} error(s), {} warning(s)",
        kept.len() - errors
    );
    for (shown, diagnostic) in kept.iter().take(MAX_SHOWN) {
        let _ = write!(
            out,
            "\n{shown}:{}:{} {}: {}",
            diagnostic.line,
            diagnostic.column,
            diagnostic.severity.as_str(),
            message(diagnostic)
        );
    }
    if kept.len() > MAX_SHOWN {
        let _ = write!(out, "\n[{} more not shown]", kept.len() - MAX_SHOWN);
    }
    out
}

/// The first line of `diagnostic`'s message, cut to [`MAX_MESSAGE`] bytes, followed by its code and what said it when those are known.
pub(crate) fn message(diagnostic: &Diagnostic) -> String {
    let mut text = diagnostic
        .message
        .lines()
        .next()
        .unwrap_or_default()
        .trim()
        .to_owned();
    if text.len() > MAX_MESSAGE {
        let mut end = MAX_MESSAGE;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
        text.push_str("...");
    }
    let origin: Vec<&str> = [diagnostic.source.as_deref(), diagnostic.code.as_deref()]
        .into_iter()
        .flatten()
        .collect();
    if !origin.is_empty() {
        let _ = write!(text, " ({})", origin.join(" "));
    }
    text
}

#[cfg(test)]
/// A language service for tests, and tests for reporting what it says.
pub(crate) mod tests {
    use std::sync::Mutex;

    use super::*;

    /// A language service that formats by making text upper case, and says what it was told to.
    #[derive(Default)]
    pub(crate) struct Fake {
        /// What it says is wrong, with any file.
        pub(crate) diagnostics: Mutex<Vec<Diagnostic>>,
        /// Whether it formats at all.
        pub(crate) formats: bool,
        /// How long it takes to answer a question about diagnostics.
        pub(crate) slow: Option<Duration>,
        /// How long it asks to be waited for, when not the usual time.
        pub(crate) patience: Option<Duration>,
        /// The paths it was asked about diagnostics for.
        pub(crate) asked: Mutex<Vec<PathBuf>>,
    }

    #[async_trait]
    impl LanguageService for Fake {
        /// The file's text upper cased, when it formats.
        async fn format(&self, path: &Path) -> Option<String> {
            if !self.formats {
                return None;
            }
            std::fs::read_to_string(path)
                .ok()
                .map(|text| text.to_uppercase())
        }

        /// The diagnostics it was told to give, about the files asked, after its delay.
        async fn diagnostics(&self, paths: &[PathBuf], _wait: Duration) -> Vec<Diagnostic> {
            self.asked.lock().unwrap().extend(paths.iter().cloned());
            if let Some(slow) = self.slow {
                tokio::time::sleep(slow).await;
            }
            self.diagnostics
                .lock()
                .unwrap()
                .iter()
                .filter(|diagnostic| paths.contains(&diagnostic.path))
                .cloned()
                .collect()
        }

        /// The patience it was given, or the usual.
        fn patience(&self, _paths: &[PathBuf]) -> Duration {
            self.patience.unwrap_or(DIAGNOSTICS_WAIT)
        }
    }

    /// A diagnostic of `severity` saying `message` at line `line` of `path`.
    pub(crate) fn diagnostic(
        path: &Path,
        line: u32,
        severity: Severity,
        message: &str,
    ) -> Diagnostic {
        Diagnostic {
            path: path.to_path_buf(),
            line,
            column: 5,
            severity,
            message: message.to_owned(),
            source: Some("rustc".to_owned()),
            code: None,
        }
    }

    /// Errors come before warnings, hints and other files are left out, and long lists are cut.
    #[test]
    fn reports_errors_then_warnings_of_the_files_written() {
        let files = [Written {
            path: PathBuf::from("/w/a.rs"),
            shown: "a.rs".to_owned(),
            text: String::new(),
            formatted: false,
        }];
        let path = Path::new("/w/a.rs");
        let block = report(
            &files,
            vec![
                diagnostic(path, 9, Severity::Warning, "unused variable"),
                diagnostic(path, 3, Severity::Error, "mismatched types\nexpected u32"),
                diagnostic(path, 4, Severity::Hint, "consider this"),
                diagnostic(Path::new("/w/other.rs"), 1, Severity::Error, "elsewhere"),
            ],
        );
        assert_eq!(
            block,
            "Diagnostics: 1 error(s), 1 warning(s)\na.rs:3:5 error: mismatched types (rustc)\na.rs:9:5 warning: unused variable (rustc)"
        );
        let many: Vec<Diagnostic> = (1..=25)
            .map(|line| diagnostic(path, line, Severity::Error, "bad"))
            .collect();
        assert!(report(&files, many).ends_with("[5 more not shown]"));
        assert_eq!(report(&files, Vec::new()), "");
    }

    /// With no service nothing is formatted or reported.
    #[tokio::test]
    async fn no_service_changes_nothing() {
        let mut files = [Written {
            path: PathBuf::from("/nowhere/a.rs"),
            shown: "a.rs".to_owned(),
            text: "x".to_owned(),
            formatted: false,
        }];
        assert_eq!(settle(&Languages::default(), &mut files).await, "");
        assert!(!files[0].formatted);
    }

    /// Files are formatted all at once, so many slow ones take no longer than one.
    #[tokio::test(start_paused = true)]
    async fn files_are_formatted_together() {
        /// A service that takes two seconds to format any file.
        struct Slow;

        #[async_trait]
        impl LanguageService for Slow {
            /// The file's name, after two seconds.
            async fn format(&self, path: &Path) -> Option<String> {
                tokio::time::sleep(Duration::from_secs(2)).await;
                Some(path.display().to_string())
            }

            /// Nothing.
            async fn diagnostics(&self, _paths: &[PathBuf], _wait: Duration) -> Vec<Diagnostic> {
                Vec::new()
            }
        }

        let dir =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../build/tests/omc-tools/together");
        std::fs::create_dir_all(&dir).unwrap();
        let mut files: Vec<Written> = (0..5)
            .map(|index| Written {
                path: dir.join(format!("{index}.rs")),
                shown: format!("{index}.rs"),
                text: String::new(),
                formatted: false,
            })
            .collect();
        let started = tokio::time::Instant::now();
        settle(&Languages::new(Some(Arc::new(Slow))), &mut files).await;
        assert!(started.elapsed() < Duration::from_secs(3));
        assert!(files.iter().all(|file| file.formatted));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A service whose servers are still loading is waited for as long as it asks, past the usual wait.
    #[tokio::test(start_paused = true)]
    async fn a_loading_service_is_waited_for_longer() {
        let path = PathBuf::from("/nowhere/a.rs");
        let fake = Fake {
            slow: Some(Duration::from_secs(10)),
            patience: Some(Duration::from_secs(20)),
            ..Fake::default()
        };
        fake.diagnostics.lock().unwrap().push(diagnostic(
            &path,
            1,
            Severity::Error,
            "found once loaded",
        ));
        let languages = Languages::new(Some(Arc::new(fake)));
        let mut files = [Written {
            path,
            shown: "a.rs".to_owned(),
            text: "x".to_owned(),
            formatted: false,
        }];
        assert!(
            settle(&languages, &mut files)
                .await
                .contains("found once loaded")
        );
    }

    /// A service that takes longer than the wait is given up on, and nothing is reported.
    #[tokio::test(start_paused = true)]
    async fn a_slow_service_is_given_up_on() {
        let path = PathBuf::from("/nowhere/a.rs");
        let fake = Fake {
            slow: Some(Duration::from_secs(60)),
            ..Fake::default()
        };
        fake.diagnostics
            .lock()
            .unwrap()
            .push(diagnostic(&path, 1, Severity::Error, "late"));
        let languages = Languages::new(Some(Arc::new(fake)));
        let mut files = [Written {
            path,
            shown: "a.rs".to_owned(),
            text: "x".to_owned(),
            formatted: false,
        }];
        assert_eq!(settle(&languages, &mut files).await, "");
    }
}
