//! `crab-bench`: fixed editing tasks, run across models with a real crab and scored on the files they leave.
//!
//! Each run gives one model one task in a fresh copy of the task's files, with only the tools the benchmark names, and compares what is left with what the task asks for. The runs are added up by model, so that a change to a tool, its description or the prompt can be judged by how often edits land and how many calls they take.

mod args;
mod baseline;
mod report;
mod run;
mod tally;
mod task;

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::process::{Command as Process, ExitCode};
use std::sync::{Mutex, mpsc};
use std::time::{SystemTime, UNIX_EPOCH};

use args::{Asked, Contender, Options};
use run::{Outcome, Setup};
use task::Task;

/// The file in the output folder that every run's outcome is written to, as a JSON list.
const RESULTS: &str = "results.json";

/// The folder the bundled tasks are in, beside this crate's sources.
pub(crate) fn bundled_tasks() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tasks")
}

/// Runs what the command line asks for, and says what went wrong when it cannot.
fn main() -> ExitCode {
    let options = match args::parse(std::env::args().skip(1)) {
        Ok(Asked::Run(options)) => options,
        Ok(Asked::Help) => {
            println!("{}", args::USAGE);
            return ExitCode::SUCCESS;
        }
        Err(reason) => {
            eprintln!("{reason}\n\n{}", args::USAGE);
            return ExitCode::FAILURE;
        }
    };
    match benchmark(&options) {
        Ok(()) => ExitCode::SUCCESS,
        Err(reason) => {
            eprintln!("{reason}");
            ExitCode::FAILURE
        }
    }
}

/// Lists the tasks, or runs every task with every contender as often as `options` say, writes the outcomes to the output folder and prints the report.
///
/// # Errors
///
/// Fails when the tasks cannot be read, crab is measured and cannot list its tools or lacks one the model is to be given, or the output folder cannot be written.
fn benchmark(options: &Options) -> Result<(), String> {
    let mut tasks = task::load(
        &options.tasks.clone().unwrap_or_else(bundled_tasks),
        &options.only,
    )?;
    if options.list {
        for task in &tasks {
            let asks = task.prompt.lines().next().unwrap_or_default();
            println!("{}: {asks}", task.name);
        }
        return Ok(());
    }
    let crab = options.crab.clone().unwrap_or_else(nearest_crab);
    let out = match &options.out {
        Some(out) => out.clone(),
        None => Path::new("build").join("bench").join(stamp()),
    };
    let blank = out.join("blank");
    std::fs::create_dir_all(&blank)
        .map_err(|error| format!("could not make {}: {error}", blank.display()))?;
    let out = out
        .canonicalize()
        .map_err(|error| format!("could not find {}: {error}", out.display()))?;
    for task in &mut tasks {
        task.settle(&out.join("expected"))?;
    }
    let measures_crab = options
        .contenders
        .iter()
        .any(|contender| matches!(contender, Contender::Crab(_)));
    let setup = Setup {
        tools: options.tools.clone(),
        offered: if measures_crab {
            offered(&crab, &options.tools, &tasks)?
        } else {
            Vec::new()
        },
        crab,
        settings: options.settings.clone(),
        timeout: options.timeout,
        blank: out.join("blank"),
    };
    let outcomes = run_all(options, &setup, &tasks, &out);
    let results = out.join(RESULTS);
    let written = serde_json::to_string_pretty(&outcomes).map_err(|error| error.to_string())?;
    std::fs::write(&results, written)
        .map_err(|error| format!("could not write {}: {error}", results.display()))?;
    println!("{}", report::render(&outcomes));
    println!("Outcomes, transcripts and edited files: {}", out.display());
    Ok(())
}

/// Runs every task in `tasks` with every contender as often as `options` say, `options.jobs` at a time, each in a folder of its own under `out`, saying how each run went as it ends.
///
/// The outcomes come back ordered by contender as given, then by task, then by run.
fn run_all(options: &Options, setup: &Setup, tasks: &[Task], out: &Path) -> Vec<Outcome> {
    let mut waiting: VecDeque<(usize, &Contender, &Task, usize)> = VecDeque::new();
    for contender in &options.contenders {
        for task in tasks {
            for number in 1..=options.runs {
                waiting.push_back((waiting.len(), contender, task, number));
            }
        }
    }
    let total = waiting.len();
    let waiting = Mutex::new(waiting);
    let (done, outcomes) = mpsc::channel::<(usize, Outcome)>();
    let mut finished: Vec<(usize, Outcome)> = Vec::with_capacity(total);
    std::thread::scope(|scope| {
        for _ in 0..options.jobs.min(total) {
            let (waiting, done) = (&waiting, done.clone());
            scope.spawn(move || {
                while let Some((place, model, task, number)) = next(waiting) {
                    let folder = out
                        .join("runs")
                        .join(file_name(&model.label()))
                        .join(&task.name)
                        .join(number.to_string());
                    let outcome = match std::fs::create_dir_all(&folder) {
                        Ok(()) => run::run(setup, model, task, number, &folder),
                        Err(error) => unmade(model, task, number, &folder, &error),
                    };
                    if done.send((place, outcome)).is_err() {
                        break;
                    }
                }
            });
        }
        drop(done);
        for (place, outcome) in outcomes {
            eprintln!(
                "[{}/{total}] {}",
                finished.len() + 1,
                report::line(&outcome)
            );
            finished.push((place, outcome));
        }
    });
    finished.sort_by_key(|(place, _)| *place);
    finished.into_iter().map(|(_, outcome)| outcome).collect()
}

/// The next run in `waiting`, taken out of it, or nothing when none is left.
fn next<T>(waiting: &Mutex<VecDeque<T>>) -> Option<T> {
    waiting.lock().ok()?.pop_front()
}

/// The outcome of run `number` of `task` by `model` whose folder `folder` could not be made, for the reason `error`.
fn unmade(
    model: &Contender,
    task: &Task,
    number: usize,
    folder: &Path,
    error: &std::io::Error,
) -> Outcome {
    Outcome {
        model: model.label(),
        task: task.name.clone(),
        run: number,
        passed: false,
        clean: false,
        differing: Vec::new(),
        error: Some(format!("could not make {}: {error}", folder.display())),
        tools: std::collections::BTreeMap::new(),
        context: 0,
        requests: 0,
        tokens: 0,
        written: 0,
        seconds: 0.0,
    }
}

/// The crab beside this program when there is one, as a build of the workspace leaves it, else the one on `PATH`.
fn nearest_crab() -> PathBuf {
    let name = format!("crab{}", std::env::consts::EXE_SUFFIX);
    std::env::current_exe()
        .ok()
        .and_then(|program| Some(program.parent()?.join(&name)))
        .filter(|beside| beside.is_file())
        .unwrap_or_else(|| PathBuf::from(name))
}

/// The names of the tools `crab` offers.
///
/// # Errors
///
/// Fails when `crab tools` cannot be run or read, or `usual` or one of `tasks` names a tool `crab` does not offer.
fn offered(crab: &Path, usual: &[String], tasks: &[Task]) -> Result<Vec<String>, String> {
    let listed = Process::new(crab)
        .arg("tools")
        .output()
        .map_err(|error| format!("could not run {} tools: {error}", crab.display()))?;
    if !listed.status.success() {
        return Err(format!(
            "{} tools failed: {}",
            crab.display(),
            String::from_utf8_lossy(&listed.stderr).trim()
        ));
    }
    let listed: Vec<omc_wire::ToolInfo> =
        serde_json::from_slice(&listed.stdout).map_err(|error| {
            format!(
                "could not read what {} tools printed: {error}",
                crab.display()
            )
        })?;
    let offered: Vec<String> = listed.into_iter().map(|tool| tool.name).collect();
    let wanted = tasks.iter().flat_map(|task| task.tools(usual)).chain(usual);
    if let Some(unknown) = wanted.into_iter().find(|name| !offered.contains(name)) {
        return Err(format!(
            "{} offers no tool called {unknown}",
            crab.display()
        ));
    }
    Ok(offered)
}

/// The seconds since 1970 as text, which names the output folder of a run that was given none.
fn stamp() -> String {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs())
        .to_string()
}

/// `label` as the name of a folder: every character that is not a letter, a digit, `-`, `_` or `.` becomes `_`.
fn file_name(label: &str) -> String {
    label
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
                c
            } else {
                '_'
            }
        })
        .collect()
}

#[cfg(test)]
/// Tests for the pieces of a benchmark run that need no model.
mod tests {
    use super::*;

    /// A model's label becomes one folder name, whatever it holds.
    #[test]
    fn a_label_becomes_a_folder_name() {
        assert_eq!(
            file_name("openrouter/vendor/model-1.5:free"),
            "openrouter_vendor_model-1.5_free"
        );
    }
}
