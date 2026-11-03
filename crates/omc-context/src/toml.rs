//! The few TOML keys an agent definition uses: top-level strings, single or triple quoted.

/// The top-level string keys of `text`, in order; anything else is skipped.
pub(crate) fn strings(text: &str) -> Vec<(String, String)> {
    let mut found = Vec::new();
    let mut lines = text.lines();
    while let Some(line) = lines.next() {
        let line = line.trim();
        if line.starts_with('[') {
            break;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let value = value.trim();
        if let Some(first) = value.strip_prefix("\"\"\"") {
            let mut held = String::new();
            if let Some(end) = first.find("\"\"\"") {
                held.push_str(&first[..end]);
            } else {
                held.push_str(first);
                for next in lines.by_ref() {
                    if let Some(end) = next.find("\"\"\"") {
                        held.push('\n');
                        held.push_str(&next[..end]);
                        break;
                    }
                    held.push('\n');
                    held.push_str(next);
                }
            }
            found.push((key.trim().to_owned(), held.trim_matches('\n').to_owned()));
        } else if let Some(quoted) = value.strip_prefix('"') {
            let end = quoted.rfind('"').unwrap_or(quoted.len());
            found.push((
                key.trim().to_owned(),
                quoted[..end].replace("\\n", "\n").replace("\\\"", "\""),
            ));
        } else if let Some(quoted) = value.strip_prefix('\'') {
            let end = quoted.rfind('\'').unwrap_or(quoted.len());
            found.push((key.trim().to_owned(), quoted[..end].to_owned()));
        }
    }
    found
}

#[cfg(test)]
/// Tests for the TOML reader.
mod tests {
    use super::*;

    /// Plain and multi-line strings are read, tables end the scan.
    #[test]
    fn reads_strings() {
        let found = strings(
            "name = \"rev\"\ndescription = 'checks'\ndeveloper_instructions = \"\"\"\nLine one\nLine two\n\"\"\"\n[x]\nname = \"no\"\n",
        );
        assert_eq!(found[0], ("name".into(), "rev".into()));
        assert_eq!(found[1], ("description".into(), "checks".into()));
        assert_eq!(found[2].1, "Line one\nLine two");
        assert_eq!(found.len(), 3);
    }
}
