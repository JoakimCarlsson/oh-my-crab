//! AWS credentials and region, found where the AWS command-line tools look for them.
//!
//! Keys come from `AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY` and
//! `AWS_SESSION_TOKEN`, else from the profile `AWS_PROFILE` names (`default`
//! otherwise) in the shared credentials file and then the config file: static
//! keys, or a `credential_process` whose output is used for five minutes. The
//! region comes from `AWS_REGION`, `AWS_DEFAULT_REGION` or the profile's
//! `region`, and is `us-east-1` when none says.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::Deserialize;

use super::sigv4::Keys;

/// The region used when nothing names one.
const DEFAULT_REGION: &str = "us-east-1";

/// The profile used when nothing names one.
const DEFAULT_PROFILE: &str = "default";

/// How long the keys a `credential_process` printed are used before it is run again.
const PROCESS_KEYS_LIFETIME: Duration = Duration::from_secs(5 * 60);

/// Where keys are to be had from.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Found {
    /// Keys read as they are.
    Keys(Keys),
    /// A command that prints keys as JSON.
    Process(String),
}

/// The sections of an INI file by name, each with its keys and values.
type Ini = BTreeMap<String, BTreeMap<String, String>>;

/// Parses the INI text of an AWS credentials or config file: `[section]` lines, `key = value` lines, and `#` or `;` comments.
fn parse_ini(text: &str) -> Ini {
    let mut sections = Ini::new();
    let mut current: Option<String> = None;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        if let Some(name) = line
            .strip_prefix('[')
            .and_then(|rest| rest.strip_suffix(']'))
        {
            let name = name.trim().to_owned();
            sections.entry(name.clone()).or_default();
            current = Some(name);
            continue;
        }
        if let (Some(section), Some((key, value))) = (&current, line.split_once('=')) {
            sections
                .entry(section.clone())
                .or_default()
                .insert(key.trim().to_owned(), value.trim().to_owned());
        }
    }
    sections
}

/// The section of the config file `config` for `profile`: `[profile name]`, or `[default]` for the default profile.
fn config_section<'a>(config: &'a Ini, profile: &str) -> Option<&'a BTreeMap<String, String>> {
    config.get(&format!("profile {profile}")).or_else(|| {
        (profile == DEFAULT_PROFILE)
            .then(|| config.get(DEFAULT_PROFILE))
            .flatten()
    })
}

/// The static keys of `section`, when it holds both an access key and a secret.
fn section_keys(section: &BTreeMap<String, String>) -> Option<Keys> {
    let value = |key: &str| section.get(key).filter(|value| !value.is_empty()).cloned();
    Some(Keys {
        access_key: value("aws_access_key_id")?,
        secret_key: value("aws_secret_access_key")?,
        session_token: value("aws_session_token"),
    })
}

/// The name of the profile in use, read with `lookup`.
fn profile_name(lookup: &dyn Fn(&str) -> Option<String>) -> String {
    lookup("AWS_PROFILE").unwrap_or_else(|| DEFAULT_PROFILE.to_owned())
}

/// Where keys are to be had from, given the environment through `lookup` and the texts of the shared credentials and config files.
///
/// # Errors
///
/// Fails, saying what to set, when neither the environment nor the profile holds keys or a `credential_process`.
fn resolve(
    lookup: &dyn Fn(&str) -> Option<String>,
    credentials: Option<&str>,
    config: Option<&str>,
) -> Result<Found, String> {
    if let (Some(access_key), Some(secret_key)) =
        (lookup("AWS_ACCESS_KEY_ID"), lookup("AWS_SECRET_ACCESS_KEY"))
    {
        return Ok(Found::Keys(Keys {
            access_key,
            secret_key,
            session_token: lookup("AWS_SESSION_TOKEN"),
        }));
    }
    let profile = profile_name(lookup);
    let credentials = credentials.map(parse_ini).unwrap_or_default();
    if let Some(keys) = credentials.get(&profile).and_then(section_keys) {
        return Ok(Found::Keys(keys));
    }
    let config = config.map(parse_ini).unwrap_or_default();
    let section = config_section(&config, &profile);
    if let Some(keys) = section.and_then(section_keys) {
        return Ok(Found::Keys(keys));
    }
    if let Some(command) = section
        .and_then(|section| section.get("credential_process"))
        .filter(|command| !command.is_empty())
    {
        return Ok(Found::Process(command.clone()));
    }
    let unsupported = section
        .map(|section| {
            section
                .keys()
                .any(|key| key.starts_with("sso_") || key == "role_arn")
        })
        .unwrap_or(false);
    let hint = if unsupported {
        format!(
            "the profile {profile} signs in with SSO or a role, which crab does not run itself; add `credential_process = aws configure export-credentials --profile {profile} --format process` to it"
        )
    } else {
        format!(
            "set AWS_ACCESS_KEY_ID and AWS_SECRET_ACCESS_KEY, give the profile {profile} keys in ~/.aws/credentials, or set AWS_BEARER_TOKEN_BEDROCK"
        )
    };
    Err(hint)
}

/// The region, given the environment through `lookup` and the text of the config file.
fn resolve_region(lookup: &dyn Fn(&str) -> Option<String>, config: Option<&str>) -> String {
    lookup("AWS_REGION")
        .or_else(|| lookup("AWS_DEFAULT_REGION"))
        .or_else(|| {
            let config = parse_ini(config?);
            config_section(&config, &profile_name(lookup))?
                .get("region")
                .filter(|region| !region.is_empty())
                .cloned()
        })
        .unwrap_or_else(|| DEFAULT_REGION.to_owned())
}

/// The value of the environment variable `name`, when it is set and not blank.
fn env(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

/// The file `variable` names, else `name` in `~/.aws`.
fn aws_file(variable: &str, name: &str) -> Option<PathBuf> {
    env(variable)
        .map(PathBuf::from)
        .or_else(|| Some(dirs::home_dir()?.join(".aws").join(name)))
}

/// The text of the shared credentials file, when there is one.
fn credentials_text() -> Option<String> {
    std::fs::read_to_string(aws_file("AWS_SHARED_CREDENTIALS_FILE", "credentials")?).ok()
}

/// The text of the config file, when there is one.
fn config_text() -> Option<String> {
    std::fs::read_to_string(aws_file("AWS_CONFIG_FILE", "config")?).ok()
}

/// The region requests go to.
pub(crate) fn region() -> String {
    resolve_region(&env, config_text().as_deref())
}

/// Says whether keys can be found, without running a `credential_process`.
///
/// # Errors
///
/// Fails with what to set when none can.
pub(crate) fn check() -> Result<(), String> {
    resolve(
        &env,
        credentials_text().as_deref(),
        config_text().as_deref(),
    )
    .map(|_| ())
}

/// The keys of a `credential_process` run lately, with the command and when it ran.
static PROCESS_KEYS: Mutex<Option<(String, Keys, Instant)>> = Mutex::new(None);

/// The keys requests are signed with right now.
///
/// # Errors
///
/// Fails with what to set when none can be found, or when the `credential_process` fails or prints something else than keys.
pub(crate) fn keys() -> Result<Keys, String> {
    match resolve(
        &env,
        credentials_text().as_deref(),
        config_text().as_deref(),
    )? {
        Found::Keys(keys) => Ok(keys),
        Found::Process(command) => process_keys(&command),
    }
}

/// The keys `command` prints, reusing those of a run less than [`PROCESS_KEYS_LIFETIME`] ago.
///
/// # Errors
///
/// Fails when the command cannot run, fails, or prints something else than keys.
fn process_keys(command: &str) -> Result<Keys, String> {
    let mut held = PROCESS_KEYS.lock().unwrap_or_else(|held| held.into_inner());
    if let Some((ran, keys, at)) = held.as_ref()
        && ran == command
        && at.elapsed() < PROCESS_KEYS_LIFETIME
    {
        return Ok(keys.clone());
    }
    let output = shell(command)
        .output()
        .map_err(|error| format!("credential_process could not run: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "credential_process failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let keys = parse_process_output(&output.stdout)?;
    *held = Some((command.to_owned(), keys.clone(), Instant::now()));
    Ok(keys)
}

/// The command that runs the command line `command` through the system's shell.
fn shell(command: &str) -> std::process::Command {
    if cfg!(windows) {
        let mut shell = std::process::Command::new("cmd");
        shell.args(["/C", command]);
        shell
    } else {
        let mut shell = std::process::Command::new("sh");
        shell.args(["-c", command]);
        shell
    }
}

/// What a `credential_process` prints.
#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct ProcessOutput {
    /// The access key id.
    access_key_id: String,
    /// The secret.
    secret_access_key: String,
    /// The session token, for temporary keys.
    #[serde(default)]
    session_token: Option<String>,
}

/// The keys in the JSON a `credential_process` printed.
///
/// # Errors
///
/// Fails when the output is not that JSON.
fn parse_process_output(bytes: &[u8]) -> Result<Keys, String> {
    let output: ProcessOutput = serde_json::from_slice(bytes)
        .map_err(|error| format!("credential_process printed no keys: {error}"))?;
    Ok(Keys {
        access_key: output.access_key_id,
        secret_key: output.secret_access_key,
        session_token: output.session_token.filter(|token| !token.is_empty()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An environment with `pairs` set and nothing else.
    fn with(pairs: &'static [(&'static str, &'static str)]) -> impl Fn(&str) -> Option<String> {
        move |name| {
            pairs
                .iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| (*value).to_owned())
        }
    }

    /// A shared credentials file with a default and a work profile.
    const CREDENTIALS: &str = "# keys\n[default]\naws_access_key_id = AKIDDEFAULT\naws_secret_access_key = s1\n\n[work]\naws_access_key_id=AKIDWORK\naws_secret_access_key=s2\naws_session_token=t2\n";

    /// A config file with regions, a process profile and an SSO profile.
    const CONFIG: &str = "[default]\nregion = eu-west-1\n[profile proc]\ncredential_process = printf keys\nregion = ap-southeast-2\n[profile sso]\nsso_start_url = https://example.awsapps.com/start\n";

    /// The environment wins, then the profile's keys in the credentials file, then the config file's process.
    #[test]
    fn resolves_keys_in_order() {
        let from_env = resolve(
            &with(&[
                ("AWS_ACCESS_KEY_ID", "AKIDENV"),
                ("AWS_SECRET_ACCESS_KEY", "s0"),
                ("AWS_SESSION_TOKEN", "t0"),
            ]),
            Some(CREDENTIALS),
            Some(CONFIG),
        );
        assert!(
            matches!(from_env, Ok(Found::Keys(keys)) if keys.access_key == "AKIDENV" && keys.session_token.as_deref() == Some("t0"))
        );
        let default = resolve(&with(&[]), Some(CREDENTIALS), Some(CONFIG)).unwrap();
        assert!(
            matches!(default, Found::Keys(keys) if keys.access_key == "AKIDDEFAULT" && keys.session_token.is_none())
        );
        let work = resolve(&with(&[("AWS_PROFILE", "work")]), Some(CREDENTIALS), None).unwrap();
        assert!(matches!(work, Found::Keys(keys) if keys.session_token.as_deref() == Some("t2")));
        let process = resolve(
            &with(&[("AWS_PROFILE", "proc")]),
            Some(CREDENTIALS),
            Some(CONFIG),
        );
        assert_eq!(process, Ok(Found::Process("printf keys".to_owned())));
    }

    /// Missing keys say what to set, and an SSO profile says how to make it usable.
    #[test]
    fn missing_keys_say_what_to_do() {
        let none = resolve(&with(&[]), None, None).unwrap_err();
        assert!(none.contains("AWS_ACCESS_KEY_ID"), "{none}");
        let sso = resolve(&with(&[("AWS_PROFILE", "sso")]), None, Some(CONFIG)).unwrap_err();
        assert!(sso.contains("credential_process"), "{sso}");
    }

    /// The region comes from the environment, then the profile, then the default.
    #[test]
    fn resolves_the_region() {
        assert_eq!(
            resolve_region(&with(&[("AWS_REGION", "us-west-2")]), Some(CONFIG)),
            "us-west-2"
        );
        assert_eq!(
            resolve_region(
                &with(&[("AWS_DEFAULT_REGION", "ca-central-1")]),
                Some(CONFIG)
            ),
            "ca-central-1"
        );
        assert_eq!(resolve_region(&with(&[]), Some(CONFIG)), "eu-west-1");
        assert_eq!(
            resolve_region(&with(&[("AWS_PROFILE", "proc")]), Some(CONFIG)),
            "ap-southeast-2"
        );
        assert_eq!(resolve_region(&with(&[]), None), "us-east-1");
    }

    /// A process's JSON output becomes keys, and anything else is an error.
    #[test]
    fn reads_process_output() {
        let keys = parse_process_output(
            br#"{"Version":1,"AccessKeyId":"AKIDP","SecretAccessKey":"sp","SessionToken":"tp","Expiration":"2030-01-01T00:00:00Z"}"#,
        )
        .unwrap();
        assert_eq!(keys.access_key, "AKIDP");
        assert_eq!(keys.session_token.as_deref(), Some("tp"));
        assert!(parse_process_output(b"nope").is_err());
    }
}
