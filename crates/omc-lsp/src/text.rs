//! Converting between the forms the language server protocol uses and the ones the harness does: file addresses, and places counted in UTF-16 code units and in characters.

use std::path::Path;

/// The `file://` address of `path`, with every byte that is not plainly safe in an address percent-encoded.
pub(crate) fn file_uri(path: &Path) -> String {
    let mut uri = String::from("file://");
    for byte in path.to_string_lossy().bytes() {
        if byte.is_ascii_alphanumeric() || b"/-_.~".contains(&byte) {
            uri.push(char::from(byte));
        } else {
            uri.push_str(&format!("%{byte:02X}"));
        }
    }
    uri
}

/// The byte offset in `text` of the place `character` UTF-16 code units into line `line`, both counted from nought; a place past the end of its line is the end of the line, and a line past the end of the text is the end of the text.
pub(crate) fn offset(text: &str, line: u32, character: u32) -> usize {
    let mut start = 0;
    for _ in 0..line {
        match text[start..].find('\n') {
            Some(newline) => start += newline + 1,
            None => return text.len(),
        }
    }
    let end = text[start..]
        .find('\n')
        .map_or(text.len(), |newline| start + newline);
    let mut units = 0;
    for (index, found) in text[start..end].char_indices() {
        if units >= character {
            return start + index;
        }
        units += u32::try_from(found.len_utf16()).unwrap_or(2);
    }
    end
}

/// The character, counted from one, that the place `character` UTF-16 code units into line `line` of `text` falls on.
pub(crate) fn column(text: &str, line: u32, character: u32) -> u32 {
    let at = offset(text, line, character);
    let start = text[..at].rfind('\n').map_or(0, |newline| newline + 1);
    u32::try_from(text[start..at].chars().count()).unwrap_or(u32::MAX) + 1
}

#[cfg(test)]
/// Tests for addresses and places.
mod tests {
    use super::*;

    /// A path with spaces and non-ASCII letters is percent-encoded in its address.
    #[test]
    fn paths_are_encoded_in_addresses() {
        let uri = file_uri(Path::new("/w/my project/ünï.rs"));
        assert_eq!(uri, "file:///w/my%20project/%C3%BCn%C3%AF.rs");
    }

    /// Places count UTF-16 code units, so a character outside the basic plane counts twice.
    #[test]
    fn places_count_utf16_units() {
        let text = "ab\n🦀x = 1\nlast";
        assert_eq!(offset(text, 0, 1), 1);
        assert_eq!(offset(text, 1, 2), "ab\n🦀".len());
        assert_eq!(column(text, 1, 2), 2);
        assert_eq!(offset(text, 1, 99), "ab\n🦀x = 1".len());
        assert_eq!(offset(text, 9, 0), text.len());
        assert_eq!(column(text, 2, 0), 1);
    }
}
