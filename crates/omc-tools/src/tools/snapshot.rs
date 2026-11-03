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
