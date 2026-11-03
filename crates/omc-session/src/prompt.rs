//! Turning what the reader wrote and attached into the text a turn starts with.

use std::path::Path;

/// The largest file whose text goes into a prompt whole, in bytes.
const EMBEDDED: u64 = 256 * 1024;

/// `text` followed by what each of `files` holds, or where it is when it is not small text.
pub(crate) fn compose(text: &str, files: &[impl AsRef<Path>]) -> String {
    let mut prompt = text.to_owned();
    for file in files {
        let file = file.as_ref();
        prompt.push_str("\n\n");
        match small_text(file) {
            Some(held) => prompt.push_str(&format!(
                "Attached file {}:\n```\n{held}\n```",
                file.display()
            )),
            None => prompt.push_str(&format!("Attached file: {}", file.display())),
        }
    }
    prompt
}

/// The text of the file at `path`, when it is text and small enough to send whole.
fn small_text(path: &Path) -> Option<String> {
    (std::fs::metadata(path).ok()?.len() <= EMBEDDED)
        .then(|| std::fs::read_to_string(path).ok())
        .flatten()
}

#[cfg(test)]
/// Tests for composing prompts.
mod tests {
    use super::compose;
    use std::path::PathBuf;

    /// A prompt without files is just the text.
    #[test]
    fn plain() {
        assert_eq!(compose("hi", &[] as &[PathBuf]), "hi");
    }

    /// A file that cannot be read is named instead of embedded.
    #[test]
    fn missing_file_is_named() {
        let composed = compose("hi", &[PathBuf::from("/no/such/file")]);
        assert!(composed.ends_with("Attached file: /no/such/file"));
    }
}
