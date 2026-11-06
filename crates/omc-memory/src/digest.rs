//! The few memories a conversation starts with.

use crate::search::{score, terms};
use crate::{Memory, Scope};

/// The most bytes the lines of a digest may hold together.
pub const DIGEST_LIMIT: usize = 4000;

/// The most bytes of one memory's text a digest shows; a longer one is cut short.
pub const ENTRY_LIMIT: usize = 300;

/// The most bytes the line saying how many memories were left out may take.
const MORE_LINE: usize = 80;

/// The memories of `memories` that fit in [`DIGEST_LIMIT`] bytes, one to a line, those that share the most words with `prompt` first and the most recently changed among equals; a last line says how many were left out, when any were. Empty when there are no memories.
///
/// Each line gives the memory's id, `(user)` for the user's memories, its text on one line, cut to [`ENTRY_LIMIT`] bytes, and its tags.
pub fn digest(memories: Vec<Memory>, prompt: &str) -> String {
    let terms = terms(prompt);
    let mut ranked: Vec<(f64, Memory)> = memories
        .into_iter()
        .map(|memory| (score(&memory, &terms), memory))
        .collect();
    ranked.sort_by(|(a, first), (b, second)| {
        b.total_cmp(a)
            .then_with(|| second.changed().cmp(first.changed()))
    });
    let total = ranked.len();
    let mut lines: Vec<String> = Vec::new();
    let mut used = 0;
    for (_, memory) in &ranked {
        let line = line(memory);
        let reserve = if lines.len() + 1 < total {
            MORE_LINE
        } else {
            0
        };
        if used + line.len() + 1 + reserve > DIGEST_LIMIT {
            break;
        }
        used += line.len() + 1;
        lines.push(line);
    }
    let left = total - lines.len();
    if left > 0 {
        lines.push(format!(
            "- … {left} more {} not shown; search for them.",
            if left == 1 { "memory" } else { "memories" }
        ));
    }
    lines.join("\n")
}

/// `memory` as one line of a digest.
fn line(memory: &Memory) -> String {
    let text = memory.text.split_whitespace().collect::<Vec<_>>().join(" ");
    let text = if text.len() > ENTRY_LIMIT {
        let mut end = ENTRY_LIMIT;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}…", text[..end].trim_end())
    } else {
        text
    };
    let user = if memory.scope == Scope::User {
        "(user) "
    } else {
        ""
    };
    let tags = if memory.tags.is_empty() {
        String::new()
    } else {
        format!(" (tags: {})", memory.tags.join(", "))
    };
    format!("- [{}] {user}{text}{tags}", memory.id)
}

#[cfg(test)]
/// Tests for the digest.
mod tests {
    use super::*;

    /// A memory numbered `n` saying `text`, changed on day `day` of January.
    fn memory(n: usize, text: &str, day: usize, scope: Scope) -> Memory {
        Memory {
            id: format!("{n:08x}"),
            text: text.to_owned(),
            tags: Vec::new(),
            created: format!("2026-01-{day:02}T00:00:00Z"),
            conversation: None,
            updated: None,
            updated_in: None,
            scope,
        }
    }

    /// No memories make an empty digest.
    #[test]
    fn nothing_saved_is_empty() {
        assert_eq!(digest(Vec::new(), "hello"), "");
    }

    /// Memories that share words with the prompt come first, then the most recent; the user's are marked and tags shown.
    #[test]
    fn relevant_then_recent_first() {
        let mut tagged = memory(3, "Deploys need VPN.", 1, Scope::User);
        tagged.tags = vec!["ops".to_owned()];
        let lines = digest(
            vec![
                memory(1, "Old fact.", 1, Scope::Project),
                memory(2, "New fact.", 9, Scope::Project),
                tagged,
            ],
            "how do I deploy",
        );
        assert_eq!(
            lines,
            "- [00000003] (user) Deploys need VPN. (tags: ops)\n- [00000002] New fact.\n- [00000001] Old fact."
        );
    }

    /// However many and however long the memories, the digest stays within its limit, cuts long ones short and counts the ones left out.
    #[test]
    fn the_digest_is_bounded() {
        let long = "word ".repeat(200);
        let memories: Vec<Memory> = (0..200)
            .map(|n| memory(n, &long, 1 + n % 28, Scope::Project))
            .collect();
        let lines = digest(memories, "");
        assert!(lines.len() <= DIGEST_LIMIT, "{} bytes", lines.len());
        let shown = lines.lines().filter(|line| line.starts_with("- [")).count();
        assert!(shown > 5);
        assert!(lines.lines().all(|line| line.len() < ENTRY_LIMIT + 40));
        assert!(lines.contains('…'));
        assert!(lines.ends_with(&format!(
            "{} more memories not shown; search for them.",
            200 - shown
        )));
    }
}
