//! Snapshot tags: a short fingerprint of a file's contents that `read` and
//! `grep` report and `edit` checks.

use std::io::Read;
use std::path::Path;

/// Offset basis of the 32-bit FNV-1a hash.
const BASIS: u32 = 0x811c_9dc5;

/// Feeds `bytes` into the running FNV-1a hash `hash`.
fn feed(mut hash: u32, bytes: &[u8]) -> u32 {
    for byte in bytes {
        hash ^= u32::from(*byte);
        hash = hash.wrapping_mul(0x0100_0193);
    }
    hash
}

/// Folds a 32-bit hash to four uppercase hex digits.
fn render(hash: u32) -> String {
    format!("{:04X}", (hash >> 16) ^ (hash & 0xffff))
}

/// Returns the four uppercase hex digits that fingerprint `bytes`.
///
/// The tag is a 32-bit FNV-1a hash folded to 16 bits, so equal contents always
/// give equal tags and a changed file almost always gives a different one.
pub fn tag(bytes: &[u8]) -> String {
    render(feed(BASIS, bytes))
}

/// What the hash of an outline tag starts from, so that its digits differ
/// from those of the plain tag of the same contents.
const OUTLINE_SALT: &[u8] = b"outline\0";

/// The letter every outline tag starts with, which no plain tag has.
const OUTLINE_PREFIX: char = 'O';

/// Returns the tag a `read` outline of a file holding `bytes` carries: the
/// letter `O` and four uppercase hex digits.
///
/// It is as stable as [`tag`] and never equal to any plain tag, so that
/// `edit` can tell an edit made against an outline, which must keep off the
/// folded lines, from one made against the whole text by the form of the tag
/// alone.
pub(crate) fn outline_tag(bytes: &[u8]) -> String {
    format!(
        "{OUTLINE_PREFIX}{}",
        render(feed(feed(BASIS, OUTLINE_SALT), bytes))
    )
}

/// Whether `cited` has the form of an outline tag rather than a plain one.
pub(crate) fn is_outline_tag(cited: &str) -> bool {
    cited
        .trim()
        .strip_prefix([OUTLINE_PREFIX, OUTLINE_PREFIX.to_ascii_lowercase()])
        .is_some_and(|digits| digits.len() == 4 && digits.chars().all(|c| c.is_ascii_hexdigit()))
}

/// Returns the tag of the file at `path`, reading it in chunks so a large file is never held in memory.
///
/// # Errors
///
/// Returns the I/O error when the file cannot be opened or read.
pub fn tag_file(path: &Path) -> std::io::Result<String> {
    let mut file = std::fs::File::open(path)?;
    let mut buf = vec![0u8; 64 * 1024];
    let mut hash = BASIS;
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            return Ok(render(hash));
        }
        hash = feed(hash, &buf[..n]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Tags are four uppercase hex digits, stable for equal input and different for changed input.
    #[test]
    fn tag_is_four_hex_digits_and_tracks_content() {
        let a = tag(b"alpha\n");
        assert_eq!(a.len(), 4);
        assert!(
            a.chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_lowercase())
        );
        assert_eq!(a, tag(b"alpha\n"));
        assert_ne!(a, tag(b"alpha \n"));
    }

    /// An outline tag is stable for equal input and differs from the plain
    /// tag, and the two kinds are told apart by their form alone.
    #[test]
    fn outline_tag_differs_from_plain_tag() {
        let outline = outline_tag(b"alpha\n");
        assert_eq!(outline.len(), 5);
        assert!(outline.starts_with('O'), "{outline}");
        assert_eq!(outline, outline_tag(b"alpha\n"));
        assert_ne!(outline, tag(b"alpha\n"));
        assert_ne!(outline, outline_tag(b"alpha \n"));
        assert!(is_outline_tag(&outline));
        assert!(is_outline_tag(&outline.to_lowercase()));
        assert!(!is_outline_tag(&tag(b"alpha\n")));
        assert!(!is_outline_tag("OXYZW"));
    }

    /// Hashing a file in chunks gives the tag of its bytes.
    #[test]
    fn file_tag_matches_byte_tag() {
        let dir = crate::testutil::scratch("snapshot-file");
        let content = "line\n".repeat(40_000);
        std::fs::write(dir.join("f.txt"), &content).unwrap();
        assert_eq!(
            tag_file(&dir.join("f.txt")).unwrap(),
            tag(content.as_bytes())
        );
    }
}
