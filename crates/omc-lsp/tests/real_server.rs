//! Runs against a real language server: `rust-analyzer` formats a file and finds an error in it, navigates and renames code across files, applies code actions, answers the move of a file, and is restarted. Skipped when `rust-analyzer` is not installed.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use omc_lsp::{Servers, default_specs};
use omc_tools::{Chosen, LanguageError, LanguageService, Location, Lookup, Severity, Spot};

/// Creates a small Cargo project of its own under the workspace's git-ignored `build/` and returns its folder.
fn project() -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../build/tests/omc-lsp/probe");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(
        dir.join("Cargo.toml"),
        "[package]\nname = \"probe\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[workspace]\n",
    )
    .unwrap();
    dir.canonicalize().unwrap()
}

/// Asks `servers` what is wrong with the file at `path` until it reports an error or `patience` runs out, since a server that has just started reports nothing until it has loaded the project.
async fn first_error(
    servers: &Servers,
    path: &Path,
    patience: Duration,
) -> Option<omc_tools::Diagnostic> {
    let until = Instant::now() + patience;
    while Instant::now() < until {
        let found = servers
            .diagnostics(&[path.to_path_buf()], Duration::from_secs(5))
            .await;
        if let Some(error) = found.into_iter().find(|d| d.severity == Severity::Error) {
            return Some(error);
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    None
}

/// `rust-analyzer` formats a badly laid out file, then reports the type error in another version of it on the line it is on.
#[tokio::test(flavor = "multi_thread")]
async fn rust_analyzer_formats_and_finds_errors() {
    let installed = default_specs()
        .iter()
        .any(|spec| spec.id == "rust-analyzer" && spec.is_installed());
    if !installed {
        eprintln!("skipped: rust-analyzer is not installed");
        return;
    }
    let dir = project();
    let file = dir.join("src/lib.rs");
    let servers = Servers::new(&dir);
    std::fs::write(&file, "pub fn  answer( )->u32{ 42 }\n").unwrap();
    let formatted = servers.format(&file).await.expect("rust-analyzer formats");
    assert!(
        formatted.contains("pub fn answer() -> u32 {"),
        "{formatted}"
    );
    std::fs::write(&file, "pub fn answer() -> u32 {\n    \"text\"\n}\n").unwrap();
    let error = first_error(&servers, &file, Duration::from_secs(120))
        .await
        .expect("rust-analyzer reports the type error");
    assert_eq!(error.path, file);
    assert_eq!(error.line, 2, "{error:?}");
}

/// Creates a small Cargo project named `name` with a library function used from a second module and from the binary, and returns its folder.
fn navigable(name: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../build/tests/omc-lsp")
        .join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(
        dir.join("Cargo.toml"),
        "[package]\nname = \"nav\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[workspace]\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("src/lib.rs"),
        "pub mod util;\n\n/// Adds one.\npub fn bump(x: u32) -> u32 {\n    x + 1\n}\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("src/util.rs"),
        "pub fn twice(x: u32) -> u32 {\n    crate::bump(crate::bump(x))\n}\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("src/main.rs"),
        "fn main() {\n    println!(\"{}\", nav::bump(1));\n}\n",
    )
    .unwrap();
    dir.canonicalize().unwrap()
}

/// `rust-analyzer`, asked first thing after it starts, waits to load the project and then finds a function's definition and every use of it across files, says what it is, outlines its file, and renames it everywhere.
#[tokio::test(flavor = "multi_thread")]
async fn rust_analyzer_navigates_and_renames() {
    let installed = default_specs()
        .iter()
        .any(|spec| spec.id == "rust-analyzer" && spec.is_installed());
    if !installed {
        eprintln!("skipped: rust-analyzer is not installed");
        return;
    }
    let dir = navigable("navigate");
    let (lib, util, main) = (
        dir.join("src/lib.rs"),
        dir.join("src/util.rs"),
        dir.join("src/main.rs"),
    );
    let servers = Servers::new(&dir);
    servers
        .serves(&lib)
        .await
        .expect("rust-analyzer serves .rs files");
    assert!(matches!(
        servers.serves(&dir.join("notes.unknown")).await,
        Err(LanguageError::NoServer(_))
    ));
    let use_site = Spot {
        path: util.clone(),
        line: 2,
        column: 12,
    };
    let found = servers
        .locate(Lookup::References, &use_site)
        .await
        .expect("rust-analyzer finds the references once it has loaded the project");
    let mut places: Vec<(PathBuf, u32)> = found
        .iter()
        .map(|location| (location.path.clone(), location.line))
        .collect();
    places.sort();
    places.dedup();
    assert!(places.contains(&(lib.clone(), 4)), "{found:?}");
    assert!(places.contains(&(util.clone(), 2)), "{found:?}");
    assert!(places.contains(&(main.clone(), 2)), "{found:?}");
    let definition = servers
        .locate(Lookup::Definition, &use_site)
        .await
        .expect("rust-analyzer finds the definition");
    assert_eq!(
        definition
            .iter()
            .map(|location| (location.path.clone(), location.line, location.column))
            .collect::<Vec<_>>(),
        vec![(lib.clone(), 4, 8)]
    );
    let hover = servers
        .hover(&use_site)
        .await
        .expect("rust-analyzer hovers")
        .expect("it says something");
    assert!(hover.contains("fn bump(x: u32) -> u32"), "{hover}");
    let symbols = servers
        .document_symbols(&lib)
        .await
        .expect("rust-analyzer outlines");
    assert!(
        symbols
            .iter()
            .any(|symbol| symbol.name == "bump" && symbol.kind == "function"),
        "{symbols:?}"
    );
    let renamed = servers
        .rename(&use_site, "increment")
        .await
        .expect("rust-analyzer renames");
    let mut changed: Vec<&Path> = renamed.iter().map(|file| file.path.as_path()).collect();
    changed.sort();
    let mut expected = vec![lib.as_path(), main.as_path(), util.as_path()];
    expected.sort();
    assert_eq!(changed, expected);
    let util_text = &renamed.iter().find(|file| file.path == util).unwrap().text;
    assert!(
        util_text.contains("crate::increment(crate::increment(x))"),
        "{util_text}"
    );
}

/// `rust-analyzer` offers to import a name used without its `use` and the import is made when the action is applied, formats a whole file but refuses to format a stretch, updates the module declaration when a file is moved, and is listed running and restarted on request.
#[tokio::test(flavor = "multi_thread")]
async fn rust_analyzer_applies_code_actions_and_moves_files() {
    let installed = default_specs()
        .iter()
        .any(|spec| spec.id == "rust-analyzer" && spec.is_installed());
    if !installed {
        eprintln!("skipped: rust-analyzer is not installed");
        return;
    }
    let dir = navigable("actions");
    let (lib, util) = (dir.join("src/lib.rs"), dir.join("src/util.rs"));
    std::fs::write(
        &util,
        "pub fn names() -> HashMap<u32, u32> {\n    HashMap::new()\n}\n",
    )
    .unwrap();
    let servers = Servers::new(&dir);
    let range = Location {
        path: util.clone(),
        line: 2,
        column: 5,
        end_line: 2,
        end_column: 12,
    };
    let until = Instant::now() + Duration::from_secs(120);
    let (index, title) = loop {
        let offered = servers
            .code_actions(&range, &[])
            .await
            .expect("rust-analyzer offers code actions");
        if let Some(found) = offered
            .iter()
            .position(|action| action.title.contains("std::collections::HashMap"))
        {
            break (found + 1, offered[found].title.clone());
        }
        assert!(Instant::now() < until, "no import offered: {offered:?}");
        tokio::time::sleep(Duration::from_millis(500)).await;
    };
    let applied = servers
        .apply_code_action(
            &range,
            &[],
            &Chosen {
                index,
                title: Some(title.clone()),
            },
        )
        .await
        .expect("the import is applied");
    assert_eq!(applied.title, title);
    let imported = &applied
        .files
        .iter()
        .find(|file| file.path == util)
        .expect("the import changes the file")
        .text;
    assert!(
        imported.contains("use std::collections::HashMap;"),
        "{imported}"
    );
    assert!(
        std::fs::read_to_string(&util)
            .unwrap()
            .starts_with("pub fn names()")
    );
    std::fs::write(&util, "pub fn  names( )->u32{ 1 }\n").unwrap();
    let whole = servers
        .reformat(&util, None)
        .await
        .expect("rust-analyzer formats a file")
        .expect("the file changes");
    assert!(whole.contains("pub fn names() -> u32 {"), "{whole}");
    let stretch = servers
        .reformat(
            &util,
            Some(&Location {
                line: 1,
                column: 1,
                end_line: 1,
                end_column: 9,
                ..range.clone()
            }),
        )
        .await;
    assert!(
        matches!(stretch, Err(LanguageError::Unsupported(_))),
        "{stretch:?}"
    );
    let helpers = dir.join("src/helpers.rs");
    let moved = servers
        .rename_file(&util, &helpers)
        .await
        .expect("rust-analyzer answers the move");
    let lib_text = &moved
        .iter()
        .find(|file| file.path == lib)
        .expect("the move changes the module declaration")
        .text;
    assert!(lib_text.contains("pub mod helpers;"), "{lib_text}");
    let status = servers
        .status()
        .await
        .expect("the servers say how they stand");
    let rust = status
        .iter()
        .find(|server| server.name == "rust-analyzer")
        .unwrap();
    assert!(
        ["running", "loading"].contains(&rust.state.as_str()),
        "{rust:?}"
    );
    assert_eq!(rust.languages, ["rust"]);
    let restarted = servers
        .reload(Some("rust-analyzer"), None)
        .await
        .expect("rust-analyzer restarts");
    assert_eq!(restarted.len(), 1);
    assert!(
        ["running", "loading"].contains(&restarted[0].state.as_str()),
        "{restarted:?}"
    );
    assert!(matches!(
        servers.reload(Some("no-such-server"), None).await,
        Err(LanguageError::NoServer(_))
    ));
}
