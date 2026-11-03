//! The small template language the prompts are written in: `{{name}}`, and `{{#if name}}…{{else}}…{{/if}}`.

/// Renders `template`, replacing `{{name}}` with the value `vars` gives it, or nothing when it gives none.
///
/// `{{#if name}}…{{/if}}` keeps its body when the value is not empty, and `{{else}}` splits the body into what is kept when it is and what is kept when it is not. Blocks may nest. Values are put in as they are and are never read as template.
pub fn render(template: &str, vars: &[(&str, &str)]) -> String {
    let mut out = String::with_capacity(template.len());
    emit(template, vars, &mut out);
    let mut tidy = String::with_capacity(out.len());
    let mut blanks = 0;
    for line in out.lines() {
        if line.trim().is_empty() {
            blanks += 1;
            if blanks > 1 {
                continue;
            }
        } else {
            blanks = 0;
        }
        tidy.push_str(line);
        tidy.push('\n');
    }
    tidy.trim().to_owned()
}

/// The value `vars` gives `name`, or nothing.
fn lookup<'a>(vars: &[(&str, &'a str)], name: &str) -> &'a str {
    vars.iter()
        .find(|(key, _)| *key == name)
        .map_or("", |(_, value)| *value)
}

/// Appends `template` rendered with `vars` to `out`.
fn emit(template: &str, vars: &[(&str, &str)], out: &mut String) {
    let mut rest = template;
    while let Some(start) = rest.find("{{") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let Some(end) = after.find("}}") else {
            out.push_str(&rest[start..]);
            return;
        };
        let tag = after[..end].trim();
        let tail = &after[end + 2..];
        if let Some(name) = tag.strip_prefix("#if ") {
            let (body, remainder) = block(tail);
            let (yes, no) = split_else(body);
            let chosen = if lookup(vars, name.trim()).is_empty() {
                no
            } else {
                yes
            };
            emit(chosen, vars, out);
            rest = remainder;
        } else {
            out.push_str(lookup(vars, tag));
            rest = tail;
        }
    }
    out.push_str(rest);
}

/// Splits `text`, which follows an `{{#if}}` tag, into the block's body and what follows its `{{/if}}`.
fn block(text: &str) -> (&str, &str) {
    let mut depth = 1;
    let mut at = 0;
    while let Some(found) = text[at..].find("{{") {
        let tag_start = at + found;
        let Some(close) = text[tag_start..].find("}}") else {
            break;
        };
        let tag = text[tag_start + 2..tag_start + close].trim();
        if tag.starts_with("#if ") {
            depth += 1;
        } else if tag == "/if" {
            depth -= 1;
            if depth == 0 {
                return (&text[..tag_start], &text[tag_start + close + 2..]);
            }
        }
        at = tag_start + close + 2;
    }
    (text, "")
}

/// Splits a block body at its own `{{else}}`, leaving the else of nested blocks alone.
fn split_else(body: &str) -> (&str, &str) {
    let mut depth = 0;
    let mut at = 0;
    while let Some(found) = body[at..].find("{{") {
        let tag_start = at + found;
        let Some(close) = body[tag_start..].find("}}") else {
            break;
        };
        let tag = body[tag_start + 2..tag_start + close].trim();
        if tag.starts_with("#if ") {
            depth += 1;
        } else if tag == "/if" {
            depth -= 1;
        } else if tag == "else" && depth == 0 {
            return (&body[..tag_start], &body[tag_start + close + 2..]);
        }
        at = tag_start + close + 2;
    }
    (body, "")
}

#[cfg(test)]
/// Tests for the template language.
mod tests {
    use super::*;

    /// Values are substituted, and unknown names vanish.
    #[test]
    fn substitutes() {
        assert_eq!(render("a {{x}} b {{y}}c", &[("x", "1")]), "a 1 b c");
    }

    /// Blocks follow their value, with else and nesting.
    #[test]
    fn conditionals() {
        let t = "{{#if a}}A{{#if b}}B{{else}}b{{/if}}{{else}}no{{/if}}!";
        assert_eq!(render(t, &[("a", "1"), ("b", "1")]), "AB!");
        assert_eq!(render(t, &[("a", "1")]), "Ab!");
        assert_eq!(render(t, &[]), "no!");
    }

    /// A value that looks like a tag is left alone.
    #[test]
    fn values_are_not_templates() {
        assert_eq!(render("{{x}}", &[("x", "{{#if y}}")]), "{{#if y}}");
    }

    /// Runs of blank lines collapse.
    #[test]
    fn collapses_blank_lines() {
        assert_eq!(render("a\n{{#if n}}x{{/if}}\n\n\n\nb", &[]), "a\n\nb");
    }
}
