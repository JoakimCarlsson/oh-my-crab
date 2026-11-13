//! What a turn changed in the files of the folder the agent works in.

/// One file a turn changed.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct TurnChange {
    /// Where the file is, from the root of the repository.
    pub path: String,
    /// Where the file was before the turn renamed or copied it, when it did.
    #[serde(default)]
    pub from: Option<String>,
    /// What happened to the file, as the letter git writes it with: `A` added, `M` modified, `D` deleted, `R` renamed, `C` copied, `T` changed in kind.
    pub status: String,
    /// The lines of its diff, from the first hunk on, each starting with `@`, `+`, `-` or a space; empty for a file that is not text.
    pub diff: String,
    /// How many lines the turn put in.
    pub added: u64,
    /// How many lines the turn took out.
    pub removed: u64,
    /// Whether `diff` was cut short because it was too long to send whole; `added` and `removed` count only the lines sent.
    #[serde(default)]
    pub truncated: bool,
}

#[cfg(test)]
/// Tests for the files a turn changed.
mod tests {
    use super::*;

    /// A changed file survives a trip through a line, and one written without the optional fields still reads.
    #[test]
    fn a_change_round_trips() {
        let change = TurnChange {
            path: "src/b.rs".to_owned(),
            from: Some("src/a.rs".to_owned()),
            status: "R".to_owned(),
            diff: "@@ -1 +1 @@\n-a\n+b".to_owned(),
            added: 1,
            removed: 1,
            truncated: false,
        };
        let line = crate::encode(&change).unwrap();
        assert_eq!(crate::decode::<TurnChange>(&line).unwrap(), change);
        let bare: TurnChange =
            crate::decode(r#"{"path":"a","status":"M","diff":"","added":0,"removed":0}"#).unwrap();
        assert_eq!((bare.from, bare.truncated), (None, false));
    }
}
