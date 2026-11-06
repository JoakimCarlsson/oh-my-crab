//! A run against a real language server: `rust-analyzer` formats a file and finds an error in it. Skipped when `rust-analyzer` is not installed.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use omc_lsp::{Servers, default_specs};
use omc_tools::{LanguageService, Severity};

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
