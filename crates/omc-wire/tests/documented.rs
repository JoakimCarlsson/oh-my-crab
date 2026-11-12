//! Checks that `docs/wire-protocol.md` keeps up with the wire types.
//!
//! The variant names are read from serde itself, by asking each enum to read a
//! variant it does not have and taking the names its error lists, and the type
//! names from the `pub use` lines of the crate root, so a variant or type added
//! to the code fails these tests until the page describes it. Every complete
//! JSON example on the page must also read as one of the wire types and write
//! back the same fields.

use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

/// The wire protocol page.
const PAGE: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../docs/wire-protocol.md"
));

/// The crate root, whose `pub use` lines name every public wire type.
const ROOT: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/lib.rs"));

/// The text that marks an example line as shortened, which is not checked.
const SHORTENED: &str = "...";

/// The variant names serde knows for `T`, read from the error it gives for `probe`, a value naming a variant `T` does not have.
///
/// # Panics
///
/// Panics when `probe` reads as a `T`, or the error lists no variant names.
fn variants_from<T: DeserializeOwned>(probe: &str) -> Vec<String> {
    let error = match serde_json::from_str::<T>(probe) {
        Ok(_) => panic!(
            "{probe} unexpectedly read as {}",
            std::any::type_name::<T>()
        ),
        Err(error) => error.to_string(),
    };
    let listed = error
        .split_once("expected")
        .map(|(_, listed)| listed)
        .unwrap_or_else(|| panic!("no variant list in: {error}"));
    let names: Vec<String> = listed
        .split('`')
        .skip(1)
        .step_by(2)
        .map(str::to_owned)
        .collect();
    assert!(!names.is_empty(), "no variant names in: {error}");
    names
}

/// The variant names of the externally tagged enum `T`.
fn variants<T: DeserializeOwned>() -> Vec<String> {
    variants_from::<T>(r#""__not_a_variant__""#)
}

/// The part of the page from the heading `start` to the next heading of the same level, or to the end.
///
/// # Panics
///
/// Panics when the page has no line `start`.
fn section(start: &str) -> &'static str {
    let begin = PAGE
        .find(&format!("\n{start}\n"))
        .unwrap_or_else(|| panic!("the page has no heading {start}"));
    let level = start.split(' ').next().unwrap_or_default();
    let rest = &PAGE[begin + start.len() + 2..];
    let end = rest
        .find(&format!("\n{level} "))
        .map_or(rest.len(), |at| at + 1);
    &rest[..end]
}

/// The variant names of `T` that the page section under `heading` has no `### `name`` heading for.
fn missing_headings<T: DeserializeOwned>(heading: &str) -> Vec<String> {
    let text = section(heading);
    variants::<T>()
        .into_iter()
        .filter(|name| !text.contains(&format!("\n### `{name}`\n")))
        .collect()
}

/// Every command has a heading of its own under "Commands".
#[test]
fn every_command_has_a_section() {
    let missing = missing_headings::<omc_wire::Command>("## Commands");
    assert!(missing.is_empty(), "commands not documented: {missing:?}");
}

/// Every event has a heading of its own under "Events".
#[test]
fn every_event_has_a_section() {
    let missing = missing_headings::<omc_wire::Event>("## Events");
    assert!(missing.is_empty(), "events not documented: {missing:?}");
}

/// Every variant of every enum carried by a command or an event is named on the page, in backticks, as it is written on the wire.
#[test]
fn every_variant_of_the_carried_enums_is_named() {
    let names = [
        variants::<omc_wire::Part>(),
        variants::<omc_wire::ToolState>(),
        variants::<omc_wire::SubagentState>(),
        variants::<omc_wire::Kind>(),
        variants::<omc_wire::Merge>(),
        variants::<omc_wire::Verdict>(),
        variants::<omc_wire::Progress>(),
        variants::<omc_wire::QueueState>(),
        variants::<omc_wire::Reach>(),
        variants::<omc_wire::Input>(),
        variants::<omc_wire::Standing>(),
        variants::<omc_wire::Severity>(),
        variants::<omc_wire::LanguageAsk>(),
        variants::<omc_wire::LanguageAnswer>(),
        variants::<omc_wire::SignIn>(),
        variants_from::<omc_wire::KeySource>(r#"{"from":"__not_a_variant__"}"#),
    ];
    let missing: Vec<String> = names
        .into_iter()
        .flatten()
        .filter(|name| !PAGE.contains(&format!("`{name}`")))
        .collect();
    assert!(missing.is_empty(), "variants not named: {missing:?}");
}

/// The public type names the crate root exports, read from its `pub use` lines; functions, whose names start in lower case, are left out.
fn exported_types() -> Vec<String> {
    ROOT.lines()
        .filter_map(|line| line.trim().strip_prefix("pub use "))
        .flat_map(|path| {
            let names = path
                .rsplit_once("::")
                .map_or(path, |(_, names)| names)
                .trim_end_matches(';');
            names
                .trim_matches(|c| c == '{' || c == '}')
                .split(',')
                .map(|name| name.trim().to_owned())
                .collect::<Vec<_>>()
        })
        .filter(|name| name.starts_with(|c: char| c.is_ascii_uppercase()))
        .collect()
}

/// Every public wire type is named on the page in backticks.
#[test]
fn every_public_type_is_named() {
    let types = exported_types();
    assert!(
        types.iter().any(|name| name == "Command"),
        "the crate root's exports were not read: {types:?}"
    );
    let missing: Vec<String> = types
        .into_iter()
        .filter(|name| !PAGE.contains(&format!("`{name}`")))
        .collect();
    assert!(missing.is_empty(), "types not named: {missing:?}");
}

/// The complete lines of the page's `json` code blocks; lines marked as shortened are left out.
fn example_lines() -> Vec<&'static str> {
    let mut lines = Vec::new();
    let mut inside = false;
    for line in PAGE.lines() {
        match line.trim() {
            "```json" => inside = true,
            "```" => inside = false,
            text if inside && !text.is_empty() && !text.contains(SHORTENED) => lines.push(text),
            _ => {}
        }
    }
    lines
}

/// Whether every field `written` has is in `read` with the same value, objects compared field by field and lists item by item.
fn covers(read: &Value, written: &Value) -> bool {
    match (read, written) {
        (Value::Object(read), Value::Object(written)) => written
            .iter()
            .all(|(key, value)| read.get(key).is_some_and(|present| covers(present, value))),
        (Value::Array(read), Value::Array(written)) => {
            read.len() == written.len()
                && read
                    .iter()
                    .zip(written)
                    .all(|(read, written)| covers(read, written))
        }
        _ => read == written,
    }
}

/// Whether `line` reads as a `T` that writes back every field `line` has, with the same values.
fn reads_as<T: DeserializeOwned + Serialize>(line: &str) -> bool {
    let (Ok(message), Ok(written)) = (
        omc_wire::decode::<T>(line),
        serde_json::from_str::<Value>(line),
    ) else {
        return false;
    };
    serde_json::to_value(&message).is_ok_and(|read| covers(&read, &written))
}

/// Every complete JSON example on the page reads as a command, an event or a type a subcommand prints or reads, without losing or renaming a field.
#[test]
fn every_example_reads_back() {
    let lines = example_lines();
    assert!(lines.len() > 50, "too few examples found: {}", lines.len());
    let wrong: Vec<&str> = lines
        .into_iter()
        .filter(|line| {
            !(reads_as::<omc_wire::Command>(line)
                || reads_as::<omc_wire::Event>(line)
                || reads_as::<Vec<omc_wire::ProviderInfo>>(line)
                || reads_as::<Vec<omc_wire::ToolInfo>>(line)
                || reads_as::<Vec<omc_wire::Limits>>(line)
                || reads_as::<omc_wire::MemoryListing>(line)
                || reads_as::<omc_wire::Imported>(line)
                || reads_as::<omc_wire::Conversation>(line))
        })
        .collect();
    assert!(
        wrong.is_empty(),
        "examples that do not read back: {wrong:#?}"
    );
}
