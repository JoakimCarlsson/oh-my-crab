//! Turning what the reader wrote and attached into the text and images a turn starts with.

use std::path::Path;

use omc_providers::Image;

/// The largest file whose text goes into a prompt whole, in bytes.
const EMBEDDED: u64 = 256 * 1024;

/// `text` followed by what each of `files` holds, or where it is when it is not small text, and the images among `files`, which the model is shown with the prompt rather than told the path of.
pub(crate) fn compose(text: &str, files: &[impl AsRef<Path>]) -> (String, Vec<Image>) {
    let mut prompt = text.to_owned();
    let mut images = Vec::new();
    for file in files {
        let file = file.as_ref();
        prompt.push_str("\n\n");
        match omc_tools::attached_image(file) {
            Some(Ok((image, line))) => {
                prompt.push_str(&format!(
                    "Attached image {}, shown below: {line}",
                    file.display()
                ));
                images.push(Image {
                    media_type: image.media_type,
                    data: image.data,
                });
            }
            Some(Err(reason)) => prompt.push_str(&format!(
                "Attached image {}, which could not be shown: {reason}",
                file.display()
            )),
            None => match small_text(file) {
                Some(held) => prompt.push_str(&format!(
                    "Attached file {}:\n```\n{held}\n```",
                    file.display()
                )),
                None => prompt.push_str(&format!("Attached file: {}", file.display())),
            },
        }
    }
    (prompt, images)
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
        assert_eq!(
            compose("hi", &[] as &[PathBuf]),
            ("hi".to_owned(), Vec::new())
        );
    }

    /// A file that cannot be read is named instead of embedded.
    #[test]
    fn missing_file_is_named() {
        let (composed, _) = compose("hi", &[PathBuf::from("/no/such/file")]);
        assert!(composed.ends_with("Attached file: /no/such/file"));
    }

    /// A PNG of one pixel.
    const DOT: &[u8] = &[
        137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 1, 0, 0, 0, 1, 8, 6,
        0, 0, 0, 31, 21, 196, 137, 0, 0, 0, 13, 73, 68, 65, 84, 120, 218, 99, 100, 96, 248, 95, 15,
        0, 2, 135, 1, 128, 235, 71, 186, 146, 0, 0, 0, 0, 73, 69, 78, 68, 174, 66, 96, 130,
    ];

    /// An attached image is shown to the model with the prompt, and the text says so instead of giving only its path.
    #[test]
    fn an_attached_image_is_shown() {
        let folder =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../build/tests/omc-session/prompt");
        std::fs::create_dir_all(&folder).unwrap();
        let path = folder.join("dot.png");
        std::fs::write(&path, DOT).unwrap();
        let (composed, images) = compose("look", &[path]);
        assert!(composed.contains("shown below"));
        assert_eq!(images.len(), 1);
        assert_eq!(images[0].media_type, "image/png");
    }
}
