//! `crab providers` lists the providers the user defined in configuration after the built-in ones.

use std::path::PathBuf;
use std::process::Command;

/// A provider written in the file `OMC_PROVIDERS_FILE` names is listed with its name and sign-in, after the built-in ones, and a broken file leaves only the built-in ones.
#[test]
fn lists_custom_providers() {
    let folder =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../build/tests/omc/custom-providers");
    let _ = std::fs::remove_dir_all(&folder);
    std::fs::create_dir_all(&folder).unwrap();
    let file = folder.join("providers.json");
    std::fs::write(
        &file,
        r#"{"providers": [
            {"id": "work", "label": "Work gateway", "base_url": "https://llm.example.com/v1",
             "api_key_env": "WORK_LLM_KEY", "default_model": "qwen-coder"},
            {"id": "box", "base_url": "http://localhost:5000/v1"}
        ]}"#,
    )
    .unwrap();
    let listed = list(&file);
    let ids: Vec<&str> = listed.iter().filter_map(|p| p["id"].as_str()).collect();
    assert_eq!(ids[0], "claude");
    assert_eq!(ids[ids.len() - 2..], ["work", "box"]);
    let work = &listed[listed.len() - 2];
    assert_eq!(work["name"], "Work gateway");
    assert_eq!(work["key_env"], serde_json::json!(["WORK_LLM_KEY"]));
    std::fs::write(&file, "{ not json").unwrap();
    let fallback = list(&file);
    assert!(fallback.iter().all(|p| p["id"] != "work"));
    assert_eq!(fallback.len(), listed.len() - 2);
    std::fs::remove_dir_all(folder).unwrap();
}

/// Runs `crab providers` with custom providers read from `file` and returns the providers it printed.
fn list(file: &std::path::Path) -> Vec<serde_json::Value> {
    let output = Command::new(env!("CARGO_BIN_EXE_crab"))
        .arg("providers")
        .env("OMC_PROVIDERS_FILE", file)
        .output()
        .unwrap();
    assert!(output.status.success());
    serde_json::from_slice(&output.stdout).unwrap()
}
