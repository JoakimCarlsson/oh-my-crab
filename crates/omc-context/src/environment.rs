//! What the agent is told about the machine and the repository it works in.

use std::path::{Path, PathBuf};

use crate::Locations;

/// The workstation the agent runs on.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Environment {
    /// The folder the agent works in.
    pub directory: PathBuf,
    /// The operating system.
    pub os: String,
    /// Today's date, as `YYYY-MM-DD`.
    pub date: String,
    /// The branch the repository is on, when the folder is in one.
    pub branch: Option<String>,
    /// The model that answers, when known.
    pub model: Option<String>,
}

impl Environment {
    /// The environment of work in `locations` with `model`, as it is now.
    pub fn detect(locations: &Locations, model: &str) -> Self {
        let seconds = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_secs());
        Self {
            directory: locations.directory.clone(),
            os: std::env::consts::OS.to_owned(),
            date: date(seconds),
            branch: locations.root.as_deref().and_then(branch),
            model: Some(model.to_owned()).filter(|model| !model.is_empty()),
        }
    }
}

/// The date `seconds` after the Unix epoch falls on, as `YYYY-MM-DD`, in UTC.
fn date(seconds: u64) -> String {
    let days = (seconds / 86_400) as i64 + 719_468;
    let era = days.div_euclid(146_097);
    let day_of_era = days.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * shifted_month + 2) / 5 + 1;
    let month = if shifted_month < 10 {
        shifted_month + 3
    } else {
        shifted_month - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}")
}

/// The moment `seconds` after the Unix epoch, as `YYYY-MM-DDTHH:MM:SSZ` in UTC, which sorts in time order as text.
pub fn timestamp(seconds: u64) -> String {
    let within = seconds % 86_400;
    format!(
        "{}T{:02}:{:02}:{:02}Z",
        date(seconds),
        within / 3600,
        within / 60 % 60,
        within % 60
    )
}

/// The branch the repository at `root` is on, or a short commit id when its head is detached.
fn branch(root: &Path) -> Option<String> {
    let dot_git = root.join(".git");
    let git_dir = if dot_git.is_file() {
        let pointer = std::fs::read_to_string(&dot_git).ok()?;
        let target = pointer.trim().strip_prefix("gitdir:")?.trim();
        root.join(target)
    } else {
        dot_git
    };
    let head = std::fs::read_to_string(git_dir.join("HEAD")).ok()?;
    let head = head.trim();
    match head.strip_prefix("ref: ") {
        Some(reference) => Some(
            reference
                .strip_prefix("refs/heads/")
                .unwrap_or(reference)
                .to_owned(),
        ),
        None => Some(format!("detached at {}", head.get(..7).unwrap_or(head))),
    }
}

#[cfg(test)]
/// Tests for the environment.
mod tests {
    use super::*;
    use crate::testutil::{put, scratch};

    /// Dates come out right across leap days and epochs.
    #[test]
    fn dates() {
        assert_eq!(date(0), "1970-01-01");
        assert_eq!(date(951_782_400), "2000-02-29");
        assert_eq!(date(1_793_664_000), "2026-11-03");
    }

    /// Timestamps carry the time of day after the date.
    #[test]
    fn timestamps() {
        assert_eq!(timestamp(0), "1970-01-01T00:00:00Z");
        assert_eq!(timestamp(1_793_664_000 + 3_723), "2026-11-03T01:02:03Z");
    }

    /// The branch is read from the head, attached or not.
    #[test]
    fn branches() {
        let base = scratch("environment-branch");
        put(&base, ".git/HEAD", "ref: refs/heads/feature/x\n");
        assert_eq!(branch(&base).as_deref(), Some("feature/x"));
        put(&base, ".git/HEAD", "0123456789abcdef\n");
        assert_eq!(branch(&base).as_deref(), Some("detached at 0123456"));
    }
}
