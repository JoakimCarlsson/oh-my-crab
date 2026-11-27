//! The crash log, bug reports and telemetry of the `crab` binary.

use std::path::PathBuf;

use omc_diagnostics::{CrashLog, Facts, Report, Telemetry};

/// The name `crab` goes by in its crash log, its bug reports and its telemetry.
const PROGRAM: &str = "crab";

/// The version of this build.
const VERSION: &str = env!("CARGO_PKG_VERSION");

/// The folder in the user's data folder that holds crab's crash log.
const DATA_FOLDER: &str = "oh-my-crab";

/// The crash log of crab, when the platform has a data folder to keep it in.
fn crash_log() -> Option<CrashLog> {
    CrashLog::of(DATA_FOLDER)
}

/// Records every panic of this process in crab's crash log.
pub(crate) fn record_crashes() {
    if let Some(log) = crash_log() {
        omc_diagnostics::install(log, PROGRAM, VERSION);
    }
}

/// Says on standard error that crab crashed since the user was last told, once per crash, and how to report it.
pub(crate) fn tell_of_crash() {
    let Some(crash) = crash_log().and_then(|log| log.take_unnoticed(omc_diagnostics::now_ms()))
    else {
        return;
    };
    let minutes = omc_diagnostics::now_ms().saturating_sub(crash.at_ms) / 60_000;
    eprintln!(
        "crab: crab {} crashed {} ago: {}\ncrab: run `crab bug-report` to bundle the crash into a zip you can send.",
        crash.version,
        ago(minutes),
        crash.message
    );
}

/// How long `minutes` is, in the largest whole unit that fits: minutes, hours or days.
fn ago(minutes: u64) -> String {
    match minutes {
        0..60 => format!("{minutes} min"),
        60..1440 => format!("{} h", minutes / 60),
        _ => format!("{} d", minutes / 1440),
    }
}

/// Starts sending crab's traces and metrics, when the `OTEL_*` variables ask for it and the build can.
pub(crate) fn start_telemetry() -> Option<Telemetry> {
    Telemetry::start(PROGRAM, VERSION)
}

/// Writes a bug report zip, as `crab bug-report [--note <text>] [folder]` asks, and returns its path: the facts about this build and machine, the note, and crab's crash log, in `folder` or the current folder.
///
/// # Errors
///
/// Fails when an option lacks its value or the zip cannot be written.
pub(crate) fn bug_report(arguments: impl Iterator<Item = String>) -> anyhow::Result<PathBuf> {
    let mut note = None;
    let mut folder = None;
    let mut arguments = arguments;
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--note" => {
                note = Some(
                    arguments
                        .next()
                        .ok_or_else(|| anyhow::anyhow!("--note needs a text"))?,
                );
            }
            path => folder = Some(PathBuf::from(path)),
        }
    }
    let mut report = Report::new(Facts::now(PROGRAM, VERSION));
    if let Some(note) = note {
        report = report.note(note);
    }
    if let Some(log) = crash_log() {
        report = report.crashes("crashes.json", &log);
    }
    let folder = match folder {
        Some(folder) => folder,
        None => std::env::current_dir()?,
    };
    Ok(report.write(&folder)?)
}
