//! The crash log: the last panics a program hit, kept on disk for the next start and for bug reports.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// How many crashes the log keeps; older ones are dropped.
const KEPT: usize = 5;

/// How long after a crash the next start still tells the user about it.
const NOTICE_WINDOW: Duration = Duration::from_secs(7 * 24 * 60 * 60);

/// One panic, as the hook [`install`] sets recorded it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Crash {
    /// When it happened, in milliseconds since the Unix epoch.
    pub at_ms: u64,
    /// The program that panicked, such as `crab`.
    pub program: String,
    /// The version of that program.
    pub version: String,
    /// What the panic said.
    pub message: String,
    /// The source file, line and column that panicked, when known.
    pub location: Option<String>,
    /// The name of the thread that panicked, when it had one.
    pub thread: Option<String>,
    /// The call stack, as the standard library prints it.
    pub backtrace: String,
    /// Whether a start of the program has told the user about it already.
    #[serde(default)]
    pub noticed: bool,
}

/// The crash log of one program: a JSON list of its last [`Crash`]es in one file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CrashLog {
    /// The file the crashes are kept in.
    path: PathBuf,
}

impl CrashLog {
    /// The crash log in `crashes.json` of the folder `app` in the user's data folder, such as `~/.local/share/oh-my-crab`; nothing when the platform has no data folder.
    pub fn of(app: &str) -> Option<Self> {
        dirs::data_dir().map(|data| Self::at(data.join(app).join("crashes.json")))
    }

    /// The crash log kept in the file `path`.
    pub fn at(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// The file the crashes are kept in.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The crashes kept, oldest first; none when the file is missing or cannot be read.
    pub fn crashes(&self) -> Vec<Crash> {
        std::fs::read(&self.path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    /// Adds `crash` to the log, dropping the oldest beyond the last five.
    ///
    /// # Errors
    ///
    /// Fails when the folder cannot be made or the file cannot be written.
    pub fn record(&self, crash: Crash) -> std::io::Result<()> {
        let mut crashes = self.crashes();
        crashes.push(crash);
        let excess = crashes.len().saturating_sub(KEPT);
        crashes.drain(..excess);
        self.save(&crashes)
    }

    /// The newest crash no start has told the user about, when it happened less than seven days before `now_ms`; every crash kept is marked as told about, so it is given once.
    pub fn take_unnoticed(&self, now_ms: u64) -> Option<Crash> {
        let mut crashes = self.crashes();
        let window = u64::try_from(NOTICE_WINDOW.as_millis()).unwrap_or(u64::MAX);
        let newest = crashes
            .iter()
            .rev()
            .find(|crash| !crash.noticed && now_ms.saturating_sub(crash.at_ms) < window)
            .cloned();
        if crashes.iter().any(|crash| !crash.noticed) {
            crashes.iter_mut().for_each(|crash| crash.noticed = true);
            self.save(&crashes).ok()?;
        }
        newest
    }

    /// Forgets every crash kept.
    ///
    /// # Errors
    ///
    /// Fails when the file exists and cannot be removed.
    pub fn clear(&self) -> std::io::Result<()> {
        match std::fs::remove_file(&self.path) {
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(error),
            _ => Ok(()),
        }
    }

    /// Writes `crashes` as the whole log, through a file beside it that replaces it, so a reader never sees half of it.
    ///
    /// # Errors
    ///
    /// Fails when the folder cannot be made or the file cannot be written.
    fn save(&self, crashes: &[Crash]) -> std::io::Result<()> {
        if let Some(folder) = self.path.parent() {
            std::fs::create_dir_all(folder)?;
        }
        let partial = self.path.with_extension("json.partial");
        std::fs::write(&partial, serde_json::to_vec_pretty(crashes)?)?;
        std::fs::rename(partial, &self.path)
    }
}

/// Milliseconds since the Unix epoch now; zero when the clock is set before it.
pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| {
            u64::try_from(since.as_millis()).unwrap_or(u64::MAX)
        })
}

/// Sets a panic hook that records every panic of this process in `log` as a crash of `program` at `version`, then runs the hook that was set before, so the panic is still printed.
///
/// A panic that is caught later, as by [`std::panic::catch_unwind`], is recorded too, since the hook cannot know. A log that cannot be written is passed over, since the process may be going down.
pub fn install(log: CrashLog, program: &'static str, version: &'static str) {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let crash = Crash {
            at_ms: now_ms(),
            program: program.to_owned(),
            version: version.to_owned(),
            message: message_of(info.payload()),
            location: info.location().map(ToString::to_string),
            thread: std::thread::current().name().map(str::to_owned),
            backtrace: std::backtrace::Backtrace::force_capture().to_string(),
            noticed: false,
        };
        let _ = log.record(crash);
        previous(info);
    }));
}

/// The text a panic was raised with, or a stand-in when it was raised with something other than text.
fn message_of(payload: &(dyn std::any::Any + Send)) -> String {
    payload
        .downcast_ref::<&str>()
        .map(|text| (*text).to_owned())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "a panic without a message".to_owned())
}

#[cfg(test)]
/// Tests for the crash log.
mod tests {
    use super::*;

    /// A crash of `crab` at `at_ms` saying `message`.
    fn crash(at_ms: u64, message: &str) -> Crash {
        Crash {
            at_ms,
            program: "crab".to_owned(),
            version: "1.0.0".to_owned(),
            message: message.to_owned(),
            location: None,
            thread: None,
            backtrace: String::new(),
            noticed: false,
        }
    }

    /// A crash log in a fresh folder under the build directory.
    fn log(name: &str) -> CrashLog {
        let folder = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../build/test-crash-log")
            .join(name);
        let _ = std::fs::remove_dir_all(&folder);
        CrashLog::at(folder.join("crashes.json"))
    }

    /// The log keeps only the last five crashes.
    #[test]
    fn keeps_the_last_five() {
        let log = log("keeps");
        for at in 0..7 {
            log.record(crash(at, &at.to_string())).unwrap();
        }
        let kept: Vec<u64> = log.crashes().iter().map(|crash| crash.at_ms).collect();
        assert_eq!(kept, vec![2, 3, 4, 5, 6]);
    }

    /// The newest recent crash is given once, and an old one never.
    #[test]
    fn tells_of_a_recent_crash_once() {
        let log = log("notice");
        let day = 24 * 60 * 60 * 1000;
        log.record(crash(0, "old")).unwrap();
        assert_eq!(log.take_unnoticed(10 * day), None);
        log.record(crash(10 * day, "first")).unwrap();
        log.record(crash(10 * day + 1, "second")).unwrap();
        assert_eq!(
            log.take_unnoticed(11 * day).map(|crash| crash.message),
            Some("second".to_owned())
        );
        assert_eq!(log.take_unnoticed(11 * day), None);
    }

    /// The hook records a panic with its message and where it happened, and the panic still unwinds.
    #[test]
    fn the_hook_records_a_panic() {
        let log = log("hook");
        install(log.clone(), "crab", "1.0.0");
        let caught = std::panic::catch_unwind(|| panic!("boom {}", 7));
        let _ = std::panic::take_hook();
        assert!(caught.is_err());
        let crashes = log.crashes();
        assert_eq!(crashes.len(), 1);
        assert_eq!(crashes[0].message, "boom 7");
        assert!(crashes[0].location.as_deref().unwrap().contains("crash.rs"));
    }

    /// Clearing a log that was never written succeeds.
    #[test]
    fn clears_a_missing_log() {
        let log = log("missing");
        log.clear().unwrap();
        assert!(log.crashes().is_empty());
    }
}
