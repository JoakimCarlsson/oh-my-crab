//! Where tool output too long to show the model in full is saved whole, so that the model can read the rest with `read` instead of running the tool again.
//!
//! Each conversation saves into a folder of its own under the harness's cache folder, `oh-my-crab/output/<conversation>`, so the files outlive a turn and stay readable when the conversation is resumed. Folders nobody wrote to for a while are removed by [`Spill::prune`].

use std::fs::{File, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// The number the next saved file gets, shared by every [`Spill`] of the process so that two never pick the same name.
static NEXT: AtomicU64 = AtomicU64::new(1);

/// The folder outputs go in when no conversation names one, made once per process.
static PROCESS_DIR: OnceLock<Option<PathBuf>> = OnceLock::new();

/// A folder where tool output too long to show is saved whole.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Spill {
    /// The folder, or nothing when output is not saved anywhere.
    dir: Option<PathBuf>,
}

impl Default for Spill {
    /// A folder of the running process under the harness's cache folder, or none when there is no cache folder.
    fn default() -> Self {
        let dir = PROCESS_DIR.get_or_init(|| {
            let started = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            output_root().map(|root| root.join(format!("run-{}-{started}", std::process::id())))
        });
        Self { dir: dir.clone() }
    }
}

impl Spill {
    /// Saves into `dir`, which is made when the first output is saved.
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self {
            dir: Some(dir.into()),
        }
    }

    /// Saves nowhere: long output is only cut.
    pub fn disabled() -> Self {
        Self { dir: None }
    }

    /// Saves into the folder of the conversation `id` under the harness's cache folder, or the process's own folder when there is no cache folder.
    pub fn for_conversation(id: &str) -> Self {
        match output_root() {
            Some(root) => Self::new(root.join(id)),
            None => Self::default(),
        }
    }

    /// The folder saved into, when there is one.
    pub fn dir(&self) -> Option<&Path> {
        self.dir.as_deref()
    }

    /// Makes a new, empty file named after `label` in the folder, making the folder when it is missing, and returns its path and the file open for writing.
    ///
    /// # Errors
    ///
    /// Fails when output is not saved anywhere, or the folder or file cannot be made.
    pub fn create(&self, label: &str) -> std::io::Result<(PathBuf, File)> {
        let dir = self.dir.as_ref().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "there is no folder to save output in",
            )
        })?;
        std::fs::create_dir_all(dir)?;
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        let number = NEXT.fetch_add(1, Ordering::Relaxed);
        let path = dir.join(format!("{label}-{stamp}-{number}.log"));
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)?;
        Ok((path, file))
    }

    /// Saves `bytes` in a new file named after `label` and returns its path.
    ///
    /// # Errors
    ///
    /// Same as [`Spill::create`], and fails when the bytes cannot be written.
    pub fn save(&self, label: &str, bytes: &[u8]) -> std::io::Result<PathBuf> {
        let (path, mut file) = self.create(label)?;
        file.write_all(bytes)?;
        Ok(path)
    }

    /// The real path of `path` when it is a file inside the folder, which `read` may open wherever the working directory is.
    pub fn holds(&self, path: &Path) -> Option<PathBuf> {
        let dir = self.dir.as_ref()?.canonicalize().ok()?;
        let real = path.canonicalize().ok()?;
        (real.starts_with(&dir) && real.is_file()).then_some(real)
    }

    /// `text` when it is at most `limit` bytes; otherwise its start and end, half the limit each, around a note of what was cut, followed by where the whole text was saved under a name made from `label`.
    pub fn cap(&self, label: &str, text: String, limit: usize) -> String {
        if text.len() <= limit {
            return text;
        }
        let head_end = text.floor_char_boundary(limit / 2);
        let tail_start = text.ceil_char_boundary(text.len() - (limit - limit / 2));
        let lines = text.lines().count();
        format!(
            "{}\n[... {} bytes cut from the middle ...]\n{}\n[output truncated: {} bytes and {lines} lines in all; {}]",
            &text[..head_end],
            tail_start - head_end,
            &text[tail_start..],
            text.len(),
            self.saved_whole(label, text.as_bytes())
        )
    }

    /// Saves `bytes` under a name made from `label` and says, for the model, where they went or why they could not be saved.
    pub fn saved_whole(&self, label: &str, bytes: &[u8]) -> String {
        match self.save(label, bytes) {
            Ok(path) => read_hint(&path),
            Err(error) => format!("the whole output could not be saved: {error}"),
        }
    }

    /// Removes the conversation folders under the harness's cache folder that were not written to for `age`, so saved output does not pile up.
    pub fn prune(age: Duration) {
        let Some(root) = output_root() else { return };
        let Ok(entries) = std::fs::read_dir(&root) else {
            return;
        };
        let now = SystemTime::now();
        for entry in entries.flatten() {
            let stale = entry
                .metadata()
                .and_then(|metadata| metadata.modified())
                .ok()
                .and_then(|modified| now.duration_since(modified).ok())
                .is_some_and(|since| since > age);
            if stale {
                let _ = std::fs::remove_dir_all(entry.path());
            }
        }
    }
}

/// Tells the model the whole output is in `path` and how to read parts of it.
pub(crate) fn read_hint(path: &Path) -> String {
    let path = path.display();
    format!(
        "the whole output is in {path}; read parts of it with the `read` tool, e.g. path \"{path}:1-200\", or \"{path}:-200\" for the last 200 lines"
    )
}

/// The folder every conversation's saved output goes under: `oh-my-crab/output` in the user's cache folder.
fn output_root() -> Option<PathBuf> {
    dirs::cache_dir().map(|cache| cache.join("oh-my-crab").join("output"))
}

#[cfg(test)]
/// Tests for saving long output.
mod tests {
    use super::*;
    use crate::testutil::scratch;

    /// Text within the limit is left alone, and longer text keeps its ends and points at the whole of it.
    #[test]
    fn caps_long_text_and_saves_it_whole() {
        let dir = scratch("spill-cap");
        let spill = Spill::new(dir.join("out"));
        assert_eq!(spill.cap("mcp", "short".to_owned(), 100), "short");
        let text = format!("{}{}{}", "a".repeat(100), "b".repeat(1000), "c".repeat(100));
        let capped = spill.cap("mcp", text.clone(), 200);
        assert!(capped.starts_with(&"a".repeat(100)));
        assert!(capped.contains("[... 1000 bytes cut from the middle ...]"));
        assert!(capped.contains(&format!(
            "{}\n[output truncated: 1200 bytes",
            "c".repeat(100)
        )));
        let saved = std::fs::read_dir(dir.join("out"))
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        assert!(capped.contains(&saved.display().to_string()));
        assert_eq!(std::fs::read_to_string(&saved).unwrap(), text);
    }

    /// Cuts never split a character.
    #[test]
    fn caps_on_character_boundaries() {
        let text = "é".repeat(100);
        let capped = Spill::disabled().cap("mcp", text, 51);
        assert!(capped.starts_with(&"é".repeat(12)));
        assert!(capped.contains("could not be saved"));
    }

    /// Only files inside the folder are held.
    #[test]
    fn holds_only_its_own_files() {
        let dir = scratch("spill-holds");
        let spill = Spill::new(dir.join("out"));
        let path = spill.save("bash", b"x").unwrap();
        assert_eq!(spill.holds(&path), Some(path.canonicalize().unwrap()));
        std::fs::write(dir.join("other.txt"), "y").unwrap();
        assert_eq!(spill.holds(&dir.join("other.txt")), None);
        assert_eq!(spill.holds(&dir.join("out/../other.txt")), None);
        assert_eq!(Spill::disabled().holds(&path), None);
    }
}
