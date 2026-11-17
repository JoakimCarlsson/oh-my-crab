//! Turns crab starts by itself when background runs end while the conversation waits for the reader.

use std::time::Duration;

use omc_tools::Finished;

/// How long crab gathers the ends of background runs before it starts a turn for them, so that runs ending close together are reported in one turn.
pub(crate) const GATHER: Duration = Duration::from_millis(500);

/// The prefix of the id of a turn crab starts when background runs end.
pub(crate) const TURN_PREFIX: &str = "wake-";

/// The prompt of the turn that tells the agent the runs `ended` ended: which, how, and the end of what each printed or answered.
pub(crate) fn notice(ended: &[Finished]) -> String {
    let what = if ended.len() == 1 {
        "A background run you started has ended"
    } else {
        "Background runs you started have ended"
    };
    let reports: Vec<String> = ended.iter().map(Finished::brief).collect();
    format!(
        "<background-runs-ended>\n{what} while you were waiting for the user. Carry on with what you started them for, or tell the user how they went.\n\n{}\n</background-runs-ended>",
        reports.join("\n\n")
    )
}

/// An id for a turn crab starts for runs that ended, unlike any other: the time to the nanosecond, after [`TURN_PREFIX`].
pub(crate) fn turn_id() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_nanos());
    format!("{TURN_PREFIX}{now}")
}

#[cfg(test)]
/// Tests for the notice of runs that ended.
mod tests {
    use super::*;

    /// The notice names every run that ended, with its status and output, and turn ids carry the prefix.
    #[test]
    fn the_notice_names_each_run() {
        let ended = [
            Finished {
                id: "bg-1".into(),
                label: "cargo test".into(),
                ok: true,
                text: "exit code: 0\nok".into(),
            },
            Finished {
                id: "bg-2".into(),
                label: "review".into(),
                ok: false,
                text: "stopped".into(),
            },
        ];
        let notice = notice(&ended);
        assert!(notice.contains("Background runs you started have ended"));
        assert!(notice.contains("bg-1 completed: cargo test\nexit code: 0\nok"));
        assert!(notice.contains("bg-2 failed: review\nstopped"));
        assert!(turn_id().starts_with(TURN_PREFIX));
    }
}
