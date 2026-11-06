//! Reading GitHub as paths: pull requests, issues, repositories, files and
//! directories at a ref, named by their github.com URL or a short scheme.
//!
//! | Path                                   | Reads                                  |
//! | -------------------------------------- | -------------------------------------- |
//! | `pr://123`, `issue://45`               | in the repository `origin` points at   |
//! | `pr://owner/repo/123`                  | in a named repository                  |
//! | `gh://owner/repo`                      | the repository and its README          |
//! | `gh://owner/repo/pull/123`             | a pull request; `:diff` for its diff   |
//! | `gh://owner/repo/issues/45`            | an issue with its comments             |
//! | `gh://owner/repo/blob/<ref>/<path>`    | a file at a ref                        |
//! | `gh://owner/repo/tree/<ref>/<path>`    | a directory at a ref                   |
//!
//! The same resources at `https://github.com/...`, or `github.com/...`, read
//! the same way; other github.com pages are fetched as web pages. Line
//! selectors work as they do for files, after `:diff` when there is one.

mod api;
mod shape;

use std::path::Path;

use serde_json::Value;

use super::selector::{self, Selector};
use super::{lines, media, web};
use crate::{Spill, ToolError, ToolOutput};
use api::{Client, Credential};

/// The approval scope of every GitHub read: approving GitHub once covers its
/// URLs and the short schemes alike.
pub(super) const SCOPE: &str = "read https://github.com";

/// The kind of file saved when a GitHub read is too long to show whole.
const SPILL_KIND: &str = "read-github";

/// A repository on GitHub.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Repo {
    /// The user or organisation owning it.
    pub(super) owner: String,
    /// Its name, without `.git`.
    pub(super) name: String,
}

/// Which repository a request is about.
#[derive(Debug, Clone, PartialEq, Eq)]
enum RepoRef {
    /// One named in the path.
    Named(Repo),
    /// The one the working directory's `origin` remote points at.
    Origin,
}

/// What in a repository a request reads.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Kind {
    /// The repository itself.
    Repo,
    /// A pull request, or its diff when `diff` is set.
    Pull {
        /// Its number.
        number: u64,
        /// Whether its unified diff is wanted instead of its conversation.
        diff: bool,
    },
    /// An issue.
    Issue {
        /// Its number.
        number: u64,
    },
    /// A file at a ref.
    Blob {
        /// The branch, tag or commit.
        reference: String,
        /// The file's path in the repository.
        path: String,
    },
    /// A directory at a ref, or at the default branch when there is none.
    Tree {
        /// The branch, tag or commit.
        reference: Option<String>,
        /// The directory's path in the repository; empty for the root.
        path: String,
    },
}

/// A GitHub read: what to read, where, and which lines.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Request {
    /// The repository.
    repo: RepoRef,
    /// What in it.
    kind: Kind,
    /// The lines wanted.
    pub(super) selector: Selector,
}

/// Recognizes a GitHub path: `None` when `raw` is not one, so that it is read
/// as a URL or file instead, and the request or why it is wrong otherwise.
///
/// `pr://`, `issue://` and `gh://` paths are always GitHub's, so a wrong one is
/// an error. A github.com URL is only claimed when it names a repository, pull
/// request, issue, file or directory; any other github.com page is left to be
/// fetched as a web page.
pub(super) fn parse(raw: &str) -> Option<Result<Request, ToolError>> {
    let (rest, selector) = selector::split(raw)
        .filter(|(_, selector)| !selector.conflicts)
        .unwrap_or((raw, Selector::default()));
    let (rest, diff) = match rest.strip_suffix(":diff") {
        Some(rest) => (rest, true),
        None => (rest, false),
    };
    let lower = rest.to_ascii_lowercase();
    let scheme = |name: &str| lower.starts_with(name).then(|| &rest[name.len()..]);
    let parsed = if let Some(after) = scheme("pr://").or_else(|| scheme("pull://")) {
        Some(numbered(after, raw, |number| Kind::Pull { number, diff }))
    } else if let Some(after) = scheme("issue://").or_else(|| scheme("issues://")) {
        Some(numbered(after, raw, |number| Kind::Issue { number }))
    } else if let Some(after) = scheme("gh://") {
        Some(
            located(after, diff)
                .ok_or_else(|| ToolError::InvalidArguments(forms(raw, "is not a GitHub path"))),
        )
    } else {
        let after = ["https://", "http://", ""].iter().find_map(|prefix| {
            ["github.com/", "www.github.com/"]
                .iter()
                .find_map(|host| scheme(&format!("{prefix}{host}")))
        })?;
        let after = after.split(['?', '#']).next().unwrap_or_default();
        located(after, diff).map(Ok)
    };
    let (repo, kind) = match parsed? {
        Ok(found) => found,
        Err(error) => return Some(Err(error)),
    };
    if diff && !matches!(kind, Kind::Pull { .. }) {
        return Some(Err(ToolError::InvalidArguments(format!(
            "{raw}: the diff selector applies to pull requests"
        ))));
    }
    if selector.web_only() {
        return Some(Err(ToolError::InvalidArguments(format!(
            "{raw}: the raw and full selectors apply to web pages, not GitHub paths"
        ))));
    }
    Some(Ok(Request {
        repo,
        kind,
        selector,
    }))
}

/// The usage message for the GitHub path `raw`, which `why` says is wrong.
fn forms(raw: &str, why: &str) -> String {
    format!(
        "{raw} {why}; GitHub paths are pr://<n>, issue://<n>, pr://<owner>/<repo>/<n>, \
         gh://<owner>/<repo>, gh://<owner>/<repo>/pull/<n>[:diff], \
         gh://<owner>/<repo>/issues/<n>, gh://<owner>/<repo>/blob/<ref>/<path> and \
         gh://<owner>/<repo>/tree/<ref>/<path>"
    )
}

/// The repository and kind of a `pr://` or `issue://` path whose part after
/// the scheme is `after`: a number for the `origin` repository, or
/// `owner/repo/number`, with `kind` making the kind from the number.
///
/// # Errors
///
/// Returns [`ToolError::InvalidArguments`] naming the forms when `after` is
/// neither.
fn numbered(
    after: &str,
    raw: &str,
    kind: impl Fn(u64) -> Kind,
) -> Result<(RepoRef, Kind), ToolError> {
    let segments: Vec<&str> = after.split('/').filter(|part| !part.is_empty()).collect();
    let number = |text: &str| text.trim_start_matches('#').parse::<u64>().ok();
    match segments.as_slice() {
        [n] => number(n).map(|n| (RepoRef::Origin, kind(n))),
        [owner, name, n] => number(n).map(|n| (RepoRef::Named(repo(owner, name)), kind(n))),
        _ => None,
    }
    .ok_or_else(|| ToolError::InvalidArguments(forms(raw, "has no number")))
}

/// The repository `owner/name`, with a `.git` ending dropped.
fn repo(owner: &str, name: &str) -> Repo {
    Repo {
        owner: owner.to_owned(),
        name: name.trim_end_matches(".git").to_owned(),
    }
}

/// The repository and kind named by `after`, the part of a GitHub URL or
/// `gh://` path after the host: `owner/repo` and what follows it, with `diff`
/// saying the `:diff` selector was given. `None` when it names nothing read
/// understands.
fn located(after: &str, diff: bool) -> Option<(RepoRef, Kind)> {
    let segments: Vec<&str> = after.split('/').filter(|part| !part.is_empty()).collect();
    let [owner, name, rest @ ..] = segments.as_slice() else {
        return None;
    };
    let number = |text: &str| text.parse::<u64>().ok();
    let kind = match rest {
        [] => Kind::Repo,
        ["pull" | "pulls", n, tail @ ..] => Kind::Pull {
            number: number(n)?,
            diff: diff || matches!(tail.first(), Some(&("files" | "changes"))),
        },
        ["issues" | "issue", n] => Kind::Issue { number: number(n)? },
        ["blob", reference, path @ ..] if !path.is_empty() => Kind::Blob {
            reference: (*reference).to_owned(),
            path: path.join("/"),
        },
        ["tree", reference, path @ ..] => Kind::Tree {
            reference: Some((*reference).to_owned()),
            path: path.join("/"),
        },
        _ => return None,
    };
    Some((RepoRef::Named(repo(owner, name)), kind))
}

/// The GitHub repository the remote URL `remote` points at, when it is one:
/// `git@github.com:o/r.git`, `ssh://git@github.com/o/r`,
/// `https://github.com/o/r.git` and the like.
pub(super) fn remote_repo(remote: &str) -> Option<Repo> {
    let remote = remote.trim();
    let path = if let Some((_, rest)) = remote.split_once("://") {
        let (authority, path) = rest.split_once('/')?;
        let host = authority.rsplit('@').next()?.split(':').next()?;
        is_github_host(host).then_some(path)?
    } else {
        let (authority, path) = remote.split_once(':')?;
        let host = authority.rsplit('@').next()?;
        is_github_host(host).then_some(path)?
    };
    let segments: Vec<&str> = path
        .trim_end_matches('/')
        .split('/')
        .filter(|part| !part.is_empty())
        .collect();
    match segments.as_slice() {
        [owner, name] => Some(repo(owner, name)),
        _ => None,
    }
}

/// Whether `host` is GitHub's, for git or its SSH endpoint.
fn is_github_host(host: &str) -> bool {
    matches!(
        host.to_ascii_lowercase().as_str(),
        "github.com" | "www.github.com" | "ssh.github.com"
    )
}

/// The GitHub repository the `origin` remote of the git work tree at `root`
/// points at.
///
/// # Errors
///
/// Returns [`ToolError::Failed`] when there is no `origin` remote or it is not
/// on GitHub, saying how to name the repository instead.
fn origin(root: &Path) -> Result<Repo, ToolError> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["remote", "get-url", "origin"])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output();
    let remote = match output {
        Ok(output) if output.status.success() => {
            String::from_utf8_lossy(&output.stdout).trim().to_owned()
        }
        _ => {
            return Err(ToolError::Failed(
                "the working directory has no git remote named origin; name the repository, \
                 as in gh://<owner>/<repo>/pull/<n>"
                    .to_owned(),
            ));
        }
    };
    remote_repo(&remote).ok_or_else(|| {
        ToolError::Failed(format!(
            "origin is {remote}, which is not a GitHub repository; name the repository, as in \
             gh://<owner>/<repo>/pull/<n>"
        ))
    })
}

impl Request {
    /// The repository the request is about, looking up `origin` in the work
    /// tree at `root` when the path did not name one.
    ///
    /// # Errors
    ///
    /// Same as looking up `origin`.
    fn resolve(&self, root: &Path) -> Result<Repo, ToolError> {
        match &self.repo {
            RepoRef::Named(repo) => Ok(repo.clone()),
            RepoRef::Origin => origin(root),
        }
    }

    /// The github.com URL of what the request reads in `repo`, shown to the
    /// user for the call.
    fn url(&self, repo: &Repo) -> String {
        let base = format!("https://github.com/{}/{}", repo.owner, repo.name);
        match &self.kind {
            Kind::Repo => base,
            Kind::Pull { number, diff } => {
                format!("{base}/pull/{number}{}", if *diff { "/files" } else { "" })
            }
            Kind::Issue { number } => format!("{base}/issues/{number}"),
            Kind::Blob { reference, path } => format!("{base}/blob/{reference}/{path}"),
            Kind::Tree {
                reference: Some(reference),
                path,
            } => format!("{base}/tree/{reference}/{path}")
                .trim_end_matches('/')
                .to_owned(),
            Kind::Tree {
                reference: None, ..
            } => base,
        }
    }

    /// The `gh://` path of what the request reads in `repo`, which reads it
    /// again with a line selector after it.
    fn short(&self, repo: &Repo) -> String {
        let url = self.url(repo);
        let path = url.trim_start_matches("https://github.com/");
        match &self.kind {
            Kind::Pull { number, diff: true } => {
                format!("gh://{}/{}/pull/{number}:diff", repo.owner, repo.name)
            }
            _ => format!("gh://{path}"),
        }
    }

    /// The line shown to the user for this read: its github.com URL in the
    /// repository the work tree at `root` names, or `raw` when that cannot be
    /// found.
    pub(super) fn describe(&self, root: &Path, raw: &str) -> String {
        self.resolve(root)
            .map(|repo| self.url(&repo))
            .unwrap_or_else(|_| raw.to_owned())
    }
}

/// Reads `request` through the GitHub API, finding the repository of a short
/// path from the work tree at `root`, signed in as well as the machine allows,
/// and saving text too long to show whole in `spill`.
///
/// # Errors
///
/// Returns [`ToolError`] when the repository cannot be found, GitHub refuses
/// or fails the request, or the selector does not apply.
pub(super) async fn read(
    request: Request,
    root: &Path,
    spill: Spill,
) -> Result<ToolOutput, ToolError> {
    let root = root.to_owned();
    let lookup = request.clone();
    let repo = tokio::task::spawn_blocking(move || lookup.resolve(&root))
        .await
        .map_err(|error| ToolError::Failed(error.to_string()))??;
    let client = Client::new(api::API, Credential::discover().await)?;
    read_with(&request, &repo, client, spill).await
}

/// Reads `request` about `repo` with `client`, saving long text in `spill`.
///
/// # Errors
///
/// Same as [`read`].
async fn read_with(
    request: &Request,
    repo: &Repo,
    mut client: Client,
    spill: Spill,
) -> Result<ToolOutput, ToolError> {
    let short = request.short(repo);
    let name = format!("{}/{}", repo.owner, repo.name);
    let base = ["repos", repo.owner.as_str(), repo.name.as_str()];
    let page = shape::PAGE.to_string();
    let paged = [("per_page", page.as_str())];
    let (what, text) = match &request.kind {
        Kind::Pull { number, diff: true } => {
            let number = number.to_string();
            let diff = client
                .get(
                    &api_path(&base, &["pulls", number.as_str()]),
                    &[],
                    api::DIFF,
                )
                .await?;
            let text = String::from_utf8(diff)
                .map_err(|_| ToolError::Failed(format!("the diff of {short} is not UTF-8 text")))?;
            (format!("diff of pull {name}#{number}"), text)
        }
        Kind::Pull {
            number,
            diff: false,
        } => {
            let number = number.to_string();
            let number = number.as_str();
            let pull = client
                .json(&api_path(&base, &["pulls", number]), &[])
                .await?;
            let files = client
                .json(&api_path(&base, &["pulls", number, "files"]), &paged)
                .await?;
            let reviews = client
                .json(&api_path(&base, &["pulls", number, "reviews"]), &paged)
                .await?;
            let review_comments = client
                .json(&api_path(&base, &["pulls", number, "comments"]), &paged)
                .await?;
            let comments = client
                .json(&api_path(&base, &["issues", number, "comments"]), &paged)
                .await?;
            let parts = shape::Pull {
                pull: &pull,
                files: &files,
                reviews: &reviews,
                review_comments: &review_comments,
                comments: &comments,
            };
            (
                format!("pull {name}#{number}"),
                shape::pull(&parts, &format!("{short}:diff")),
            )
        }
        Kind::Issue { number } => {
            let number = number.to_string();
            let number = number.as_str();
            let issue = client
                .json(&api_path(&base, &["issues", number]), &[])
                .await?;
            let comments = client
                .json(&api_path(&base, &["issues", number, "comments"]), &paged)
                .await?;
            let as_pull = format!("gh://{name}/pull/{number}");
            (
                format!("issue {name}#{number}"),
                shape::issue(&issue, &comments, &as_pull),
            )
        }
        Kind::Repo => {
            let meta = client.json(&base, &[]).await?;
            let root = client.json(&api_path(&base, &["contents"]), &[]).await.ok();
            let readme = client
                .get(&api_path(&base, &["readme"]), &[], api::RAW)
                .await
                .ok()
                .map(|bytes| String::from_utf8_lossy(&bytes).into_owned());
            (
                format!("repository {name}"),
                shape::repo(&meta, root.as_ref(), readme.as_deref()),
            )
        }
        Kind::Blob { reference, path } => {
            let segments = contents_path(&base, path);
            let bytes = client
                .get(&segments, &[("ref", reference.as_str())], api::RAW)
                .await?;
            return file(
                &short,
                &bytes,
                &request.selector,
                &client.credential,
                &spill,
            );
        }
        Kind::Tree { reference, path } => {
            let segments = contents_path(&base, path);
            let query: Vec<(&str, &str)> = reference
                .iter()
                .map(|reference| ("ref", reference.as_str()))
                .collect();
            let value = client.json(&segments, &query).await?;
            if !shape::is_listing(&value) {
                let bytes = decoded(&value).ok_or_else(|| {
                    ToolError::Failed(format!("{short} is neither a directory nor a file"))
                })?;
                return file(
                    &short,
                    &bytes,
                    &request.selector,
                    &client.credential,
                    &spill,
                );
            }
            (format!("directory {short}"), shape::listing(&value))
        }
    };
    let numbered = web::number_saved(&text, &request.selector, &short, &spill, SPILL_KIND)?;
    Ok(ToolOutput::new(format!(
        "[github {what}, {}]\n{numbered}",
        client.credential.describe()
    )))
}

/// The API path made of the repository's API path `base` and `tail`.
fn api_path<'a>(base: &[&'a str], tail: &[&'a str]) -> Vec<&'a str> {
    [base, tail].concat()
}

/// The API path of the contents of `path` in the repository whose API path is
/// `base`, one segment per directory.
fn contents_path<'a>(base: &[&'a str], path: &'a str) -> Vec<&'a str> {
    let mut segments = base.to_vec();
    segments.push("contents");
    segments.extend(path.split('/').filter(|part| !part.is_empty()));
    segments
}

/// The bytes of a file GitHub gave as JSON with base64 content, when `value`
/// is one.
fn decoded(value: &Value) -> Option<Vec<u8>> {
    use base64::Engine as _;
    if value["encoding"].as_str() != Some("base64") {
        return None;
    }
    let content: String = value["content"]
        .as_str()?
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect();
    base64::engine::general_purpose::STANDARD
        .decode(content)
        .ok()
}

/// The file `bytes`, called `label`, as `read` shows files: images as images,
/// PDFs as their text, text as numbered lines, under a header saying it came
/// from GitHub signed in with `credential`; long text is saved in `spill`.
///
/// # Errors
///
/// Returns [`ToolError`] when the file is binary data of another kind or the
/// selector does not apply.
fn file(
    label: &str,
    bytes: &[u8],
    selector: &Selector,
    credential: &Credential,
    spill: &Spill,
) -> Result<ToolOutput, ToolError> {
    let (text, note) = match media::sniff(bytes) {
        Some(media::Media::Image(media_type)) => {
            if selector.picks_lines() {
                return Err(ToolError::InvalidArguments(format!(
                    "{label} is an image; line selectors apply to text"
                )));
            }
            media::check_image_size(bytes.len(), label)?;
            return media::image(bytes, media_type, label);
        }
        Some(media::Media::Pdf) => {
            let (text, pages) = media::pdf_text(bytes, label)?;
            (text, format!(", {}", media::pages(pages)))
        }
        None if bytes[..bytes.len().min(8192)].contains(&0) => {
            return Err(lines::not_text(label));
        }
        None => (String::from_utf8_lossy(bytes).into_owned(), String::new()),
    };
    let numbered = web::number_saved(&text, selector, label, spill, SPILL_KIND)?;
    Ok(ToolOutput::new(format!(
        "[github file {label}{note}, {}]\n{numbered}",
        credential.describe()
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Parses `raw`, panicking when it is not a valid GitHub path.
    fn parsed(raw: &str) -> Request {
        parse(raw).unwrap().unwrap()
    }

    /// The repository `o/r`.
    fn named() -> RepoRef {
        RepoRef::Named(repo("o", "r"))
    }

    /// Short schemes resolve against origin or name a repository.
    #[test]
    fn parses_short_schemes() {
        let pr = parsed("pr://12");
        assert_eq!(pr.repo, RepoRef::Origin);
        assert_eq!(
            pr.kind,
            Kind::Pull {
                number: 12,
                diff: false
            }
        );
        let diff = parsed("pr://12:diff:5-9");
        assert_eq!(
            diff.kind,
            Kind::Pull {
                number: 12,
                diff: true
            }
        );
        assert!(diff.selector.picks_lines());
        assert_eq!(
            parsed("PR://#7").kind,
            Kind::Pull {
                number: 7,
                diff: false
            }
        );
        let issue = parsed("issue://o/r/45");
        assert_eq!(
            (issue.repo, issue.kind),
            (named(), Kind::Issue { number: 45 })
        );
        assert!(matches!(
            parse("pr://abc"),
            Some(Err(ToolError::InvalidArguments(_)))
        ));
        assert!(matches!(
            parse("issue://3:diff"),
            Some(Err(ToolError::InvalidArguments(_)))
        ));
    }

    /// `gh://` paths and github.com URLs name the same resources.
    #[test]
    fn parses_paths_and_urls() {
        for raw in [
            "gh://o/r/pull/3",
            "https://github.com/o/r/pull/3",
            "github.com/o/r/pull/3/commits",
            "https://www.github.com/o/r/pull/3#issuecomment-1",
        ] {
            let request = parsed(raw);
            assert_eq!(request.repo, named(), "{raw}");
            assert_eq!(
                request.kind,
                Kind::Pull {
                    number: 3,
                    diff: false
                },
                "{raw}"
            );
        }
        assert_eq!(
            parsed("https://github.com/o/r/pull/3/files").kind,
            Kind::Pull {
                number: 3,
                diff: true
            }
        );
        assert_eq!(parsed("gh://o/r.git").kind, Kind::Repo);
        assert_eq!(
            parsed("https://github.com/o/r/blob/main/src/lib.rs:10-20").kind,
            Kind::Blob {
                reference: "main".into(),
                path: "src/lib.rs".into()
            }
        );
        assert_eq!(
            parsed("gh://o/r/tree/v1/docs").kind,
            Kind::Tree {
                reference: Some("v1".into()),
                path: "docs".into()
            }
        );
        assert!(parse("https://github.com/o/r/actions/runs/1").is_none());
        assert!(parse("https://github.com/features").is_none());
        assert!(parse("https://example.com/o/r/pull/1").is_none());
        assert!(parse("src/github.rs").is_none());
        assert!(matches!(parse("gh://o"), Some(Err(_))));
        assert!(matches!(parse("gh://o/r/pull/1:raw"), Some(Err(_))));
    }

    /// Requests show as github.com URLs and continue as `gh://` paths.
    #[test]
    fn names_requests() {
        let repo = repo("o", "r");
        let diff = parsed("pr://4:diff");
        assert_eq!(diff.url(&repo), "https://github.com/o/r/pull/4/files");
        assert_eq!(diff.short(&repo), "gh://o/r/pull/4:diff");
        let root = parsed("gh://o/r/tree/main");
        assert_eq!(root.url(&repo), "https://github.com/o/r/tree/main");
        assert_eq!(parsed("issue://9").short(&repo), "gh://o/r/issues/9");
    }

    /// Remote URLs of every usual form are recognized, and others are not.
    #[test]
    fn reads_remotes() {
        for remote in [
            "git@github.com:o/r.git",
            "ssh://git@github.com/o/r.git",
            "ssh://git@ssh.github.com:443/o/r",
            "https://github.com/o/r",
            "https://user:token@github.com/o/r.git/",
            "git://github.com/o/r.git",
        ] {
            assert_eq!(remote_repo(remote), Some(repo("o", "r")), "{remote}");
        }
        for remote in [
            "git@gitlab.com:o/r.git",
            "https://github.com/o",
            "/srv/git/r.git",
            "https://evilgithub.com/o/r",
        ] {
            assert_eq!(remote_repo(remote), None, "{remote}");
        }
    }

    /// The `origin` of a work tree is found, and a missing one is explained.
    #[test]
    fn finds_origin() {
        let dir = crate::testutil::scratch("github-origin");
        let git = |args: &[&str]| {
            std::process::Command::new("git")
                .arg("-C")
                .arg(&dir)
                .args(args)
                .output()
                .unwrap()
        };
        git(&["init", "-q"]);
        let missing = origin(&dir).unwrap_err().to_string();
        assert!(missing.contains("no git remote named origin"), "{missing}");
        git(&["remote", "add", "origin", "git@github.com:o/r.git"]);
        assert_eq!(origin(&dir).unwrap(), repo("o", "r"));
        assert_eq!(
            parsed("pr://5").describe(&dir, "pr://5"),
            "https://github.com/o/r/pull/5"
        );
    }

    /// Answers the GitHub API requests of the tests from the fixtures.
    fn answer(request: &crate::testutil::Received) -> crate::testutil::Answer {
        let path = request.target.split('?').next().unwrap_or_default();
        let json = |body: &str| (200, "application/json", body.as_bytes().to_vec());
        match path {
            "/repos/o/r/pulls/3565" if request.header("accept") == Some(api::DIFF) => (
                200,
                "text/plain",
                include_bytes!("fixtures/pull.diff").to_vec(),
            ),
            "/repos/o/r/pulls/3565" => json(include_str!("fixtures/pull.json")),
            "/repos/o/r/pulls/3565/files" => json(include_str!("fixtures/pull_files.json")),
            "/repos/o/r/pulls/3565/reviews" => json(include_str!("fixtures/pull_reviews.json")),
            "/repos/o/r/pulls/3565/comments" => {
                json(include_str!("fixtures/pull_review_comments.json"))
            }
            "/repos/o/r/issues/3565/comments" => json(include_str!("fixtures/pull_comments.json")),
            "/repos/o/r/contents/src/a%20b.rs" => (200, "text/plain", b"fn a() {}\n".to_vec()),
            "/repos/o/r/contents/src" => json(include_str!("fixtures/contents.json")),
            _ => (
                404,
                "application/json",
                br#"{"message":"Not Found"}"#.to_vec(),
            ),
        }
    }

    /// A pull request, its diff, a file and a directory are read through the
    /// API, and a missing one fails with GitHub's message.
    #[tokio::test]
    async fn reads_through_the_api() {
        let (address, received) = crate::testutil::serve(answer).await;
        let base = format!("http://{address}");
        let repo = repo("o", "r");
        let spill = Spill::disabled();
        let read = |raw: &'static str| {
            let base = base.clone();
            let repo = repo.clone();
            let spill = spill.clone();
            async move {
                let client = Client::new(&base, Credential::Anonymous).unwrap();
                read_with(&parsed(raw), &repo, client, spill).await
            }
        };
        let pull = read("pr://3565").await.unwrap().text;
        assert!(
            pull.starts_with("[github pull o/r#3565, not signed in]\n     1\t# ci: install lz4"),
            "{pull}"
        );
        assert!(
            pull.contains("\tThe diff: read gh://o/r/pull/3565:diff\n"),
            "{pull}"
        );
        let diff = read("pr://3565:diff:1-2").await.unwrap().text;
        assert_eq!(
            diff,
            "[github diff of pull o/r#3565, not signed in]\n     1\tdiff --git \
             a/.github/workflows/ci.yml b/.github/workflows/ci.yml\n     2\tindex \
             4b88a75b9b..a9954a55fa 100644\n"
        );
        let file = read("gh://o/r/blob/main/src/a b.rs").await.unwrap().text;
        assert_eq!(
            file,
            "[github file gh://o/r/blob/main/src/a b.rs, not signed in]\n     1\tfn a() {}\n"
        );
        let listing = read("gh://o/r/tree/main/src").await.unwrap().text;
        assert!(
            listing.starts_with("[github directory gh://o/r/tree/main/src"),
            "{listing}"
        );
        let missing = read("issue://o/r/1").await.unwrap_err().to_string();
        assert!(missing.contains("404"), "{missing}");
        assert!(missing.contains("gh auth login"), "{missing}");
        let requests = received.lock().unwrap();
        let file_request = requests
            .iter()
            .find(|request| {
                request
                    .target
                    .starts_with("/repos/o/r/contents/src/a%20b.rs")
            })
            .unwrap();
        assert_eq!(
            file_request.target,
            "/repos/o/r/contents/src/a%20b.rs?ref=main"
        );
        assert_eq!(file_request.header("accept"), Some(api::RAW));
        assert!(
            requests
                .iter()
                .all(|request| request.header("authorization").is_none())
        );
    }
}
