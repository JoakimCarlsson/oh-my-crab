//! What `crab hooks` and `crab trust` print and change: the hooks that apply in a folder and its project's trust.

use std::path::{Path, PathBuf};

use omc_context::Locations;
use omc_hooks::{Assessment, TrustStore, assess, own_file};
use serde_json::{Value, json};

/// The hooks a conversation in `folder`, or in the current folder when there is none, would find, as JSON: the project and its trust, each hook with its event, matcher, command, timeout, file and origin and whether it would run, and what the project brings that is left out until it is trusted.
///
/// # Errors
///
/// Fails with the reason when there is no folder given and the current one cannot be known.
pub fn hooks(folder: Option<PathBuf>) -> Result<Value, String> {
    let directory = folder_or_current(folder)?;
    let store = TrustStore::locate();
    let own = own_file();
    let assessment = assess(
        &Locations::detect(&directory),
        own.as_deref(),
        store.as_ref(),
        false,
        &[],
    );
    let listed: Vec<Value> = assessment
        .found
        .iter()
        .map(|hook| {
            json!({
                "event": hook.event.name(),
                "matcher": hook.matcher,
                "command": hook.command,
                "timeout": hook.limit().as_secs_f64(),
                "file": hook.file,
                "origin": hook.origin.name(),
                "runs": assessment.hooks.contains(hook),
            })
        })
        .collect();
    Ok(json!({
        "project": project(&assessment),
        "trust": standing(&assessment, store.as_ref()),
        "files": {
            "own": own,
            "project": omc_hooks::PROJECT_FILES.iter().map(|(file, _)| *file).collect::<Vec<_>>(),
        },
        "hooks": listed,
        "skipped": assessment.skipped,
        "problems": assessment.problems,
    }))
}

/// Records whether the project `folder` belongs to is trusted, when `trusted` says, with its hooks as they are now, and returns its trust as JSON: the project, its standing, the trust file and what is left out.
///
/// # Errors
///
/// Fails with the reason when the folder cannot be known, there is no data folder to record trust in, or the record cannot be written.
pub fn trust(folder: Option<PathBuf>, trusted: Option<bool>) -> Result<Value, String> {
    let directory = folder_or_current(folder)?;
    let store =
        TrustStore::locate().ok_or_else(|| "no data folder to record trust in".to_owned())?;
    let locations = Locations::detect(&directory);
    let own = own_file();
    let mut assessment = assess(&locations, own.as_deref(), Some(&store), false, &[]);
    if let Some(trusted) = trusted {
        let hooks = if trusted {
            assessment.project_hooks.clone()
        } else {
            Vec::new()
        };
        store
            .set(&assessment.project, trusted, hooks)
            .map_err(|error| format!("cannot record the trust: {error}"))?;
        assessment = assess(&locations, own.as_deref(), Some(&store), false, &[]);
    }
    Ok(json!({
        "project": project(&assessment),
        "trust": standing(&assessment, Some(&store)),
        "skipped": assessment.skipped,
    }))
}

/// `folder`, or the current folder when there is none.
///
/// # Errors
///
/// Fails with the reason when the current folder cannot be known.
fn folder_or_current(folder: Option<PathBuf>) -> Result<PathBuf, String> {
    match folder {
        Some(folder) => Ok(folder),
        None => std::env::current_dir().map_err(|error| error.to_string()),
    }
}

/// The project of `assessment` as JSON: its folder and what tells it apart.
fn project(assessment: &Assessment) -> Value {
    json!({
        "root": assessment.project.root,
        "key": assessment.project.key,
    })
}

/// The trust of the project of `assessment` as JSON: its standing, whether its hooks run and its other inputs are read, and the file trust is recorded in, from `store`.
fn standing(assessment: &Assessment, store: Option<&TrustStore>) -> Value {
    json!({
        "standing": assessment.standing.name(),
        "hooks": assessment.standing.runs_project_hooks(),
        "inputs": assessment.standing.reads_project(),
        "file": store.map(TrustStore::path).map(Path::to_path_buf),
    })
}
