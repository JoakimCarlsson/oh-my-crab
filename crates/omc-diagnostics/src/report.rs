//! Bug reports: facts about the machine and the program, the crash log and other files, bundled into one zip.

use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use zip::write::SimpleFileOptions;

use crate::crash::{CrashLog, now_ms};

/// What a bug report says about the program and the machine it ran on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Facts {
    /// The program, such as `crab`.
    pub program: String,
    /// Its version.
    pub version: String,
    /// The operating system, as [`std::env::consts::OS`] names it.
    pub os: String,
    /// The processor architecture, as [`std::env::consts::ARCH`] names it.
    pub arch: String,
    /// When the facts were gathered, in milliseconds since the Unix epoch.
    pub at_ms: u64,
}

impl Facts {
    /// The facts about `program` at `version` on this machine now.
    pub fn now(program: &str, version: &str) -> Self {
        Self {
            program: program.to_owned(),
            version: version.to_owned(),
            os: std::env::consts::OS.to_owned(),
            arch: std::env::consts::ARCH.to_owned(),
            at_ms: now_ms(),
        }
    }
}

/// A bug report being put together: the facts, what the user said and the files that go with them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    /// The program and machine the report is about.
    facts: Facts,
    /// What the user said went wrong, when they said.
    note: Option<String>,
    /// The files of the report, by their path inside the zip.
    files: Vec<(String, Vec<u8>)>,
}

impl Report {
    /// A report about `facts` holding nothing else yet.
    pub fn new(facts: Facts) -> Self {
        Self {
            facts,
            note: None,
            files: Vec::new(),
        }
    }

    /// Adds what the user said went wrong.
    #[must_use]
    pub fn note(mut self, note: impl Into<String>) -> Self {
        self.note = Some(note.into());
        self
    }

    /// Adds `bytes` as the file `name` of the zip, such as `server/facts.json`.
    #[must_use]
    pub fn file(mut self, name: impl Into<String>, bytes: Vec<u8>) -> Self {
        self.files.push((name.into(), bytes));
        self
    }

    /// Adds the crashes kept in `log` as the file `name`, when it holds any.
    #[must_use]
    pub fn crashes(self, name: impl Into<String>, log: &CrashLog) -> Self {
        let crashes = log.crashes();
        if crashes.is_empty() {
            return self;
        }
        let bytes = serde_json::to_vec_pretty(&crashes).unwrap_or_default();
        self.file(name, bytes)
    }

    /// Writes the report as `<program>-bug-report-<time>.zip` in `folder`, making the folder when it is missing, and returns the zip's path.
    ///
    /// The zip holds `report.json`, with the facts and the note, then every file added, in the order added.
    ///
    /// # Errors
    ///
    /// Fails when the folder cannot be made or the zip cannot be written.
    pub fn write(&self, folder: &Path) -> std::io::Result<PathBuf> {
        std::fs::create_dir_all(folder)?;
        let path = folder.join(format!(
            "{}-bug-report-{}.zip",
            self.facts.program, self.facts.at_ms
        ));
        let mut zip = zip::ZipWriter::new(std::fs::File::create(&path)?);
        let options = SimpleFileOptions::default();
        let summary = serde_json::json!({ "facts": self.facts, "note": self.note });
        zip.start_file("report.json", options)?;
        zip.write_all(&serde_json::to_vec_pretty(&summary)?)?;
        for (name, bytes) in &self.files {
            zip.start_file(name.as_str(), options)?;
            zip.write_all(bytes)?;
        }
        zip.finish()?;
        Ok(path)
    }
}

#[cfg(test)]
/// Tests for bug reports.
mod tests {
    use super::*;

    /// A report holds its summary and every file added, in order.
    #[test]
    fn writes_every_file() {
        let folder = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../build/test-bug-report");
        let _ = std::fs::remove_dir_all(&folder);
        let path = Report::new(Facts::now("crab", "1.0.0"))
            .note("it broke")
            .file("extra.txt", b"hello".to_vec())
            .crashes("crashes.json", &CrashLog::at(folder.join("none.json")))
            .write(&folder)
            .unwrap();
        let mut archive = zip::ZipArchive::new(std::fs::File::open(path).unwrap()).unwrap();
        let names: Vec<String> = (0..archive.len())
            .map(|index| {
                archive
                    .by_index(index)
                    .unwrap()
                    .name()
                    .unwrap()
                    .into_owned()
            })
            .collect();
        assert_eq!(names, vec!["report.json", "extra.txt"]);
        let summary: serde_json::Value =
            serde_json::from_reader(archive.by_name("report.json").unwrap()).unwrap();
        assert_eq!(summary["note"], "it broke");
        assert_eq!(summary["facts"]["program"], "crab");
    }
}
