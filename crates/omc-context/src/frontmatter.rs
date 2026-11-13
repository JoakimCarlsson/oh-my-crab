//! The front matter of a markdown file: `key: value` lines between two `---` lines.

/// A markdown file split into its front matter and its body.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Parsed {
    /// The front matter's keys, lower-cased, with their values.
    pub fields: Vec<(String, String)>,
    /// What follows the front matter.
    pub body: String,
}

impl Parsed {
    /// The value of `key`, when the front matter has it.
    pub fn get(&self, key: &str) -> Option<&str> {
        self.fields
            .iter()
            .find(|(name, _)| name == key)
            .map(|(_, value)| value.as_str())
    }

    /// The value of `key` read as a list: comma separated, or a bracketed list, or lines of `- item`.
    pub fn list(&self, key: &str) -> Option<Vec<String>> {
        let value = self.get(key)?;
        let value = value.trim().trim_start_matches('[').trim_end_matches(']');
        Some(
            value
                .split([',', '\n'])
                .map(|item| unquote(item.trim().trim_start_matches("- ").trim()))
                .filter(|item| !item.is_empty())
                .collect(),
        )
    }
}

/// Splits `text` into its front matter and body; text without front matter is all body.
pub fn parse(text: &str) -> Parsed {
    let text = text.trim_start_matches('\u{feff}');
    let Some(rest) = text
        .strip_prefix("---\n")
        .or_else(|| text.strip_prefix("---\r\n"))
    else {
        return Parsed {
            fields: Vec::new(),
            body: text.to_owned(),
        };
    };
    let mut head = Vec::new();
    let mut lines = rest.lines();
    let mut closed = false;
    for line in lines.by_ref() {
        if line.trim_end() == "---" {
            closed = true;
            break;
        }
        head.push(line);
    }
    if !closed {
        return Parsed {
            fields: Vec::new(),
            body: text.to_owned(),
        };
    }
    let body = lines.collect::<Vec<_>>().join("\n");
    Parsed {
        fields: fields(&head),
        body: body.trim_start_matches('\n').to_owned(),
    }
}

/// The keys of the front matter lines `head`, with indented lines joined onto the key above.
fn fields(head: &[&str]) -> Vec<(String, String)> {
    let mut found: Vec<(String, String)> = Vec::new();
    let mut folded = false;
    for line in head {
        let indented = line.starts_with([' ', '\t']) || line.trim_start().starts_with("- ");
        if indented && let Some((_, value)) = found.last_mut() {
            if !value.is_empty() {
                value.push(if folded { ' ' } else { '\n' });
            }
            value.push_str(line.trim());
            continue;
        }
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim();
        folded = value.starts_with('>');
        let value = if value.starts_with(['>', '|']) {
            String::new()
        } else {
            unquote(value)
        };
        found.push((key.trim().to_lowercase(), value));
    }
    found
}

/// `text` without one pair of surrounding quotes.
pub(crate) fn unquote(text: &str) -> String {
    let text = text.trim();
    for quote in ['"', '\''] {
        if let Some(inner) = text
            .strip_prefix(quote)
            .and_then(|rest| rest.strip_suffix(quote))
        {
            return inner.to_owned();
        }
    }
    text.to_owned()
}

#[cfg(test)]
/// Tests for the front matter parser.
mod tests {
    use super::*;

    /// Keys, quoted values and the body are told apart.
    #[test]
    fn splits_head_and_body() {
        let parsed = parse("---\nname: pdf\ndescription: \"Reads PDFs\"\n---\n# Body\ntext");
        assert_eq!(parsed.get("name"), Some("pdf"));
        assert_eq!(parsed.get("description"), Some("Reads PDFs"));
        assert_eq!(parsed.body, "# Body\ntext");
    }

    /// Folded blocks and lists are read.
    #[test]
    fn folded_values_and_lists() {
        let parsed = parse(
            "---\ndescription: >\n  one\n  two\ntools:\n  - Read\n  - grep\nmore: [a, b]\n---\nx",
        );
        assert_eq!(parsed.get("description"), Some("one two"));
        assert_eq!(
            parsed.list("tools"),
            Some(vec!["Read".into(), "grep".into()])
        );
        assert_eq!(parsed.list("more"), Some(vec!["a".into(), "b".into()]));
    }

    /// Text without front matter is all body.
    #[test]
    fn no_front_matter() {
        let parsed = parse("just text\n---\nnot a head");
        assert!(parsed.fields.is_empty());
        assert_eq!(parsed.body, "just text\n---\nnot a head");
    }
}
