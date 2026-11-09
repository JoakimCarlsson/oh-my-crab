//! Tests for reading permission rules from their files and keeping "always allow" answers.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use omc_agent::permissions::{Judgement, Rule};
use omc_agent::{Keeper, Place};
use omc_context::Locations;
use omc_hooks::{TrustStore, assess};
use omc_memory::Project;
use omc_tools::Subject;

use super::{FileKeeper, PermissionsFile, load};

/// Creates an empty folder named `name` under the workspace's git-ignored `build/tests/omc-session/permissions` and returns its canonical path.
fn scratch(name: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../build/tests/omc-session/permissions")
        .join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir.canonicalize().unwrap()
}

/// Writes `text` to `path` under `base`, creating the folders on the way.
fn put(base: &Path, path: &str, text: &str) {
    let file = base.join(path);
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(file, text).unwrap();
}

/// The locations of work in the project `project`, with the user's home and Claude Code folder under `base` so nothing of the real user's is read.
fn locations(base: &Path, project: &Path) -> Locations {
    Locations {
        directory: project.to_path_buf(),
        root: Some(project.to_path_buf()),
        home: Some(base.join("home")),
        claude: Some(base.join("home/.claude")),
        codex: None,
        crab: None,
        project: true,
    }
}

/// The texts of `rules`.
fn texts(rules: &[Rule]) -> Vec<String> {
    rules.iter().map(Rule::to_string).collect()
}

/// A project's own Claude Code settings bring deny and ask rules whether it is trusted or not, and allow rules only once it is.
#[test]
fn project_allow_rules_wait_for_trust() {
    let base = scratch("trust");
    let project = base.join("repo");
    std::fs::create_dir_all(project.join(".git")).unwrap();
    put(
        &project,
        ".claude/settings.json",
        r#"{"permissions":{"allow":["Bash"],"deny":["Bash(rm:*)"],"ask":["Bash(git push:*)"]}}"#,
    );
    put(
        &project,
        ".claude/settings.local.json",
        r#"{"permissions":{"deny":["Read(./.env)"]}}"#,
    );
    let store = TrustStore::new(base.join("trust.json"));
    let at = locations(&base, &project);
    let untrusted = assess(&at, None, Some(&store), false, &[]);
    let loaded = load(&at, &untrusted, None);
    assert!(loaded.book.allow.is_empty());
    assert_eq!(texts(&loaded.book.deny), ["Bash(rm:*)", "Read(./.env)"]);
    assert_eq!(texts(&loaded.book.ask), ["Bash(git push:*)"]);
    assert_eq!(loaded.found.iter().filter(|found| !found.used).count(), 1);
    let vouched = assess(&at, None, Some(&store), true, &[]);
    let loaded = load(&at, &vouched, None);
    assert_eq!(texts(&loaded.book.allow), ["Bash"]);
    assert!(matches!(
        loaded
            .book
            .judge("bash", &Subject::Command("ls; rm x".into()), &[]),
        Judgement::Denied(_)
    ));
}

/// Crab's file brings the user's rules and this project's, not another project's; Claude Code's user settings are read too.
#[test]
fn crab_and_claude_user_rules_are_read() {
    let base = scratch("own");
    let project = base.join("repo");
    std::fs::create_dir_all(project.join(".git")).unwrap();
    let file = PermissionsFile::new(base.join("permissions.json"));
    let here = Project::of(&project);
    let elsewhere = Project::of(&base);
    file.add(
        omc_agent::permissions::Effect::Allow,
        &["Bash(git status:*)".into()],
        None,
    )
    .unwrap();
    file.add(
        omc_agent::permissions::Effect::Allow,
        &["Edit(/src/**)".into()],
        Some(&here),
    )
    .unwrap();
    file.add(
        omc_agent::permissions::Effect::Deny,
        &["Bash(curl:*)".into()],
        Some(&elsewhere),
    )
    .unwrap();
    put(
        &base,
        "home/.claude/settings.json",
        r#"{"permissions":{"allow":["WebSearch"],"defaultMode":"default"}}"#,
    );
    let store = TrustStore::new(base.join("trust.json"));
    let at = locations(&base, &project);
    let assessment = assess(&at, None, Some(&store), false, &[]);
    let loaded = load(&at, &assessment, Some(&file));
    assert_eq!(
        texts(&loaded.book.allow),
        ["Bash(git status:*)", "Edit(/src/**)", "WebSearch"]
    );
    assert!(loaded.book.deny.is_empty());
    assert!(
        loaded
            .book
            .covered("edit", &Subject::Files(vec![project.join("src/a.rs")]), &[])
    );
    assert!(loaded.problems.is_empty(), "{:?}", loaded.problems);
}

/// A path written `/path` in a project's settings file is read from the folder holding its `.claude`.
#[test]
fn project_paths_are_read_from_their_folder() {
    let base = scratch("base");
    let project = base.join("repo");
    std::fs::create_dir_all(project.join(".git")).unwrap();
    std::fs::create_dir_all(project.join("sub")).unwrap();
    put(
        &project,
        "sub/.claude/settings.json",
        r#"{"permissions":{"deny":["Edit(/locked/**)"]}}"#,
    );
    let store = TrustStore::new(base.join("trust.json"));
    let mut at = locations(&base, &project);
    at.directory = project.join("sub");
    let assessment = assess(&at, None, Some(&store), false, &[]);
    let loaded = load(&at, &assessment, None);
    let judged = |path: PathBuf| loaded.book.judge("edit", &Subject::Files(vec![path]), &[]);
    assert!(matches!(
        judged(project.join("sub/locked/a")),
        Judgement::Denied(_)
    ));
    assert_eq!(judged(project.join("locked/a")), Judgement::Open);
}

/// What cannot be parsed is reported and the rest still read.
#[test]
fn problems_are_reported() {
    let base = scratch("problems");
    let project = base.join("repo");
    std::fs::create_dir_all(project.join(".git")).unwrap();
    put(
        &project,
        ".claude/settings.json",
        r#"{"permissions":{"deny":["Bash(rm", "Bash(dd:*)"]}}"#,
    );
    put(&project, ".claude/settings.local.json", "{ not json");
    let file = PermissionsFile::new(base.join("permissions.json"));
    put(&base, "permissions.json", "[1, 2]");
    let store = TrustStore::new(base.join("trust.json"));
    let at = locations(&base, &project);
    let assessment = assess(&at, None, Some(&store), false, &[]);
    let loaded = load(&at, &assessment, Some(&file));
    assert_eq!(texts(&loaded.book.deny), ["Bash(dd:*)"]);
    assert_eq!(loaded.problems.len(), 3, "{:?}", loaded.problems);
}

/// The keeper adds rules for the project or every project, once each, keeping what else the file holds; a file it cannot parse is left as it is and the failure reported.
#[test]
fn the_keeper_adds_to_crabs_file() {
    let base = scratch("keeper");
    let project = Project::of(&base);
    let path = base.join("config/permissions.json");
    put(
        &base,
        "config/permissions.json",
        r#"{"note":"mine","permissions":{"deny":["Bash(rm:*)"],"extra":1}}"#,
    );
    let reported = Arc::new(Mutex::new(Vec::new()));
    let keeper = {
        let reported = reported.clone();
        FileKeeper::new(
            PermissionsFile::new(&path),
            project.clone(),
            Box::new(move |problem| reported.lock().unwrap().push(problem)),
        )
    };
    keeper.keep(&["Bash(git status:*)".into()], Place::Project);
    keeper.keep(&["Bash(git status:*)".into()], Place::Project);
    keeper.keep(&["WebSearch".into()], Place::User);
    let stored: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(stored["note"], "mine");
    assert_eq!(stored["permissions"]["extra"], 1);
    assert_eq!(
        stored["permissions"]["deny"],
        serde_json::json!(["Bash(rm:*)"])
    );
    assert_eq!(
        stored["permissions"]["allow"],
        serde_json::json!(["WebSearch"])
    );
    let key = project.key.display().to_string();
    assert_eq!(
        stored["projects"][&key]["permissions"]["allow"],
        serde_json::json!(["Bash(git status:*)"])
    );
    assert!(reported.lock().unwrap().is_empty());
    std::fs::write(&path, "{ broken").unwrap();
    keeper.keep(&["WebSearch".into()], Place::User);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "{ broken");
    assert_eq!(reported.lock().unwrap().len(), 1);
}

/// Removing a rule takes it out of the user's lists and the project's, however it was spaced, and drops a project left with nothing.
#[test]
fn rules_are_removed() {
    let base = scratch("remove");
    let project = Project::of(&base);
    let file = PermissionsFile::new(base.join("permissions.json"));
    file.add(
        omc_agent::permissions::Effect::Allow,
        &["Bash(ls:*)".into()],
        Some(&project),
    )
    .unwrap();
    file.add(
        omc_agent::permissions::Effect::Deny,
        &["Bash( ls:*)".into(), "Bash(rm:*)".into()],
        None,
    )
    .unwrap();
    assert_eq!(file.remove("Bash(ls:*)", &project).unwrap(), 2);
    let stored = file.read().unwrap();
    assert!(stored.projects.is_empty());
    assert_eq!(stored.permissions.deny, ["Bash(rm:*)"]);
    assert_eq!(file.remove("Bash(nothing)", &project).unwrap(), 0);
}
