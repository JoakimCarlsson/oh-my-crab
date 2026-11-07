//! Google Cloud credentials, project and region, found where Google's own tools look for them.
//!
//! The token comes from the Application Default Credentials file:
//! `GOOGLE_APPLICATION_CREDENTIALS`, else `application_default_credentials.json`
//! in gcloud's config folder (`CLOUDSDK_CONFIG`, else `~/.config/gcloud`, or
//! `%APPDATA%\gcloud` on Windows). Without one, `gcloud auth print-access-token`
//! is asked. The project comes from `GOOGLE_CLOUD_PROJECT`, `GCLOUD_PROJECT` or
//! `ANTHROPIC_VERTEX_PROJECT_ID`, else from the credentials file; the region
//! from `GOOGLE_CLOUD_LOCATION` or `CLOUD_ML_REGION`, else `global`.

use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use super::google_token::{self, CredentialFile, Token};

/// The region used when nothing names one.
const DEFAULT_REGION: &str = "global";

/// How long before it expires a token is no longer used.
const EXPIRY_MARGIN: Duration = Duration::from_secs(60);

/// How long a token gcloud printed is used, since gcloud does not say how long it lives.
const GCLOUD_TOKEN_LIFETIME: Duration = Duration::from_secs(10 * 60);

/// The value of the environment variable `name`, when it is set and not blank.
fn env(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

/// gcloud's config folder.
fn gcloud_folder() -> Option<PathBuf> {
    if let Some(folder) = env("CLOUDSDK_CONFIG") {
        return Some(PathBuf::from(folder));
    }
    if cfg!(windows) {
        return env("APPDATA").map(|appdata| PathBuf::from(appdata).join("gcloud"));
    }
    Some(dirs::home_dir()?.join(".config").join("gcloud"))
}

/// The Application Default Credentials file in use, when there is one.
fn adc_path() -> Option<PathBuf> {
    env("GOOGLE_APPLICATION_CREDENTIALS")
        .map(PathBuf::from)
        .or_else(|| {
            Some(gcloud_folder()?.join("application_default_credentials.json"))
                .filter(|path| path.is_file())
        })
}

/// The Application Default Credentials file, read.
///
/// # Errors
///
/// Fails when the file named cannot be read or is not of a kind crab reads; `Ok(None)` when there is none.
fn credential_file() -> Result<Option<CredentialFile>, String> {
    let Some(path) = adc_path() else {
        return Ok(None);
    };
    let text =
        std::fs::read_to_string(&path).map_err(|error| format!("{}: {error}", path.display()))?;
    serde_json::from_str(&text).map(Some).map_err(|error| {
        format!(
            "{}: not a service account key or user login: {error}",
            path.display()
        )
    })
}

/// The project requests are billed to.
///
/// # Errors
///
/// Fails, saying what to set, when neither the environment nor the credentials file names one.
pub(crate) fn project() -> Result<String, String> {
    env("GOOGLE_CLOUD_PROJECT")
        .or_else(|| env("GCLOUD_PROJECT"))
        .or_else(|| env("ANTHROPIC_VERTEX_PROJECT_ID"))
        .or_else(|| {
            credential_file()
                .ok()
                .flatten()
                .and_then(|file| file.project().map(str::to_owned))
        })
        .ok_or_else(|| "set GOOGLE_CLOUD_PROJECT to the project Vertex AI is used in".to_owned())
}

/// The region requests go to.
pub(crate) fn region() -> String {
    env("GOOGLE_CLOUD_LOCATION")
        .or_else(|| env("CLOUD_ML_REGION"))
        .unwrap_or_else(|| DEFAULT_REGION.to_owned())
}

/// Whether a `gcloud` program can be found on the search path.
fn gcloud_installed() -> bool {
    let names: &[&str] = if cfg!(windows) {
        &["gcloud.cmd", "gcloud.exe"]
    } else {
        &["gcloud"]
    };
    std::env::var_os("PATH").is_some_and(|path| {
        std::env::split_paths(&path)
            .any(|folder| names.iter().any(|name| folder.join(name).is_file()))
    })
}

/// Says whether a token and a project can be found, without asking for a token.
///
/// # Errors
///
/// Fails with what to set when either cannot.
pub(crate) fn check() -> Result<(), String> {
    if credential_file()?.is_none() && !gcloud_installed() {
        return Err(
            "run `gcloud auth application-default login`, or set GOOGLE_APPLICATION_CREDENTIALS to a service account key"
                .to_owned(),
        );
    }
    project().map(|_| ())
}

/// The token in use and when it stops being used.
static TOKEN: Mutex<Option<(String, Instant)>> = Mutex::new(None);

/// A token for Vertex AI, reused until a minute before it expires.
///
/// # Errors
///
/// Fails when no credentials can be found or the token cannot be had.
pub(crate) async fn access_token() -> Result<String, String> {
    if let Some((token, until)) = TOKEN
        .lock()
        .unwrap_or_else(|held| held.into_inner())
        .as_ref()
        && Instant::now() < *until
    {
        return Ok(token.clone());
    }
    let token = match credential_file()? {
        Some(file) => google_token::exchange(&file).await?,
        None => gcloud_token()?,
    };
    let until = Instant::now() + token.lifetime.saturating_sub(EXPIRY_MARGIN);
    *TOKEN.lock().unwrap_or_else(|held| held.into_inner()) = Some((token.access.clone(), until));
    Ok(token.access)
}

/// The token `gcloud auth print-access-token` prints.
///
/// # Errors
///
/// Fails when gcloud cannot run or prints no token.
fn gcloud_token() -> Result<Token, String> {
    let program = if cfg!(windows) {
        "gcloud.cmd"
    } else {
        "gcloud"
    };
    let output = std::process::Command::new(program)
        .args(["auth", "print-access-token"])
        .output()
        .map_err(|error| {
            format!(
                "no Google credentials: run `gcloud auth application-default login` or set GOOGLE_APPLICATION_CREDENTIALS ({error})"
            )
        })?;
    let access = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    if !output.status.success() || access.is_empty() {
        return Err(format!(
            "gcloud auth print-access-token failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(Token {
        access,
        lifetime: GCLOUD_TOKEN_LIFETIME + EXPIRY_MARGIN,
    })
}
