//! Finding memories by the words they hold, forgiving small typos and word endings.

use crate::Memory;

/// Words too common to tell memories apart.
const STOP_WORDS: &[&str] = &[
    "a", "an", "and", "are", "as", "at", "be", "by", "do", "does", "for", "from", "how", "i", "in",
    "is", "it", "of", "on", "or", "that", "the", "this", "to", "was", "we", "what", "when",
    "where", "which", "who", "why", "with", "you",
];

/// The memories of `memories` that match `query` and carry every tag of `tags`, best match first and, among equals, the most recently changed first; every memory carrying the tags, most recent first, when the query has no words that count.
pub fn search(memories: Vec<Memory>, query: &str, tags: &[String]) -> Vec<Memory> {
    let tags: Vec<String> = tags.iter().map(|tag| tag.trim().to_lowercase()).collect();
    let terms = terms(query);
    let mut scored: Vec<(f64, Memory)> = memories
        .into_iter()
        .filter(|memory| tags.iter().all(|tag| memory.tags.contains(tag)))
        .map(|memory| (score(&memory, &terms), memory))
        .filter(|(score, _)| terms.is_empty() || *score > 0.0)
        .collect();
    scored.sort_by(|(a, first), (b, second)| {
        b.total_cmp(a)
            .then_with(|| second.changed().cmp(first.changed()))
    });
    scored.into_iter().map(|(_, memory)| memory).collect()
}

/// The words of `text` that count for a search: lowercase runs of letters and digits, without the most common words.
pub(crate) fn terms(text: &str) -> Vec<String> {
    let mut terms: Vec<String> = Vec::new();
    for word in words(text) {
        if !STOP_WORDS.contains(&word.as_str()) && !terms.contains(&word) {
            terms.push(word);
        }
    }
    terms
}

/// The lowercase runs of letters and digits in `text`.
fn words(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .map(str::to_lowercase)
        .collect()
}

/// How well `memory` matches `terms`: for each term, how closely the nearest word of its text or tags comes to it, summed, with tags counting a little more than text.
///
/// A word that is the term scores 3, one that starts the other or holds the term scores 2 or 1.5, and one a typo away scores 1.
pub fn score(memory: &Memory, terms: &[String]) -> f64 {
    let text = words(&memory.text);
    let tags: Vec<String> = memory.tags.iter().flat_map(|tag| words(tag)).collect();
    terms
        .iter()
        .map(|term| {
            let in_text = text
                .iter()
                .map(|word| closeness(term, word))
                .fold(0.0, f64::max);
            let in_tags = tags
                .iter()
                .map(|word| closeness(term, word) * 1.25)
                .fold(0.0, f64::max);
            in_text.max(in_tags)
        })
        .sum()
}

/// How closely `word` comes to `term`, from 0 for not at all to 3 for the same word.
fn closeness(term: &str, word: &str) -> f64 {
    let (term_length, word_length) = (term.chars().count(), word.chars().count());
    if term == word {
        3.0
    } else if term_length >= 3
        && word_length >= 3
        && (word.starts_with(term) || term.starts_with(word))
    {
        2.0
    } else if term_length >= 3 && word.contains(term) {
        1.5
    } else if term_length >= 4 && within(term, word, if term_length >= 8 { 2 } else { 1 }) {
        1.0
    } else {
        0.0
    }
}

/// Whether `a` turns into `b` with at most `most` letters added, removed or changed, or two neighbours swapped.
fn within(a: &str, b: &str, most: usize) -> bool {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    if a.len().abs_diff(b.len()) > most {
        return false;
    }
    let mut table = vec![vec![0usize; b.len() + 1]; a.len() + 1];
    for (i, row) in table.iter_mut().enumerate() {
        row[0] = i;
    }
    for (j, cell) in table[0].iter_mut().enumerate() {
        *cell = j;
    }
    for i in 1..=a.len() {
        for j in 1..=b.len() {
            let changed = table[i - 1][j - 1] + usize::from(a[i - 1] != b[j - 1]);
            let mut best = changed.min(table[i - 1][j] + 1).min(table[i][j - 1] + 1);
            if i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] {
                best = best.min(table[i - 2][j - 2] + 1);
            }
            table[i][j] = best;
        }
    }
    table[a.len()][b.len()] <= most
}

#[cfg(test)]
/// Tests for searching memories.
mod tests {
    use super::*;
    use crate::Scope;

    /// A memory saying `text` with `tags`, last changed at `changed`.
    fn memory(id: &str, text: &str, tags: &[&str], changed: &str) -> Memory {
        Memory {
            id: id.to_owned(),
            text: text.to_owned(),
            tags: tags.iter().map(|tag| (*tag).to_owned()).collect(),
            created: changed.to_owned(),
            conversation: None,
            updated: None,
            updated_in: None,
            scope: Scope::Project,
        }
    }

    /// The memories the tests search.
    fn shelf() -> Vec<Memory> {
        vec![
            memory(
                "a",
                "The staging database listens on port 6543.",
                &["db"],
                "2026-01-01T00:00:00Z",
            ),
            memory(
                "b",
                "Integration tests need Docker running.",
                &["tests"],
                "2026-01-02T00:00:00Z",
            ),
            memory(
                "c",
                "Deploys go through the release workflow.",
                &[],
                "2026-01-03T00:00:00Z",
            ),
        ]
    }

    /// The ids of `found`, in order.
    fn ids(found: &[Memory]) -> Vec<&str> {
        found.iter().map(|memory| memory.id.as_str()).collect()
    }

    /// Exact words, word starts, typos and tags all find a memory; common words alone find nothing in particular.
    #[test]
    fn finds_by_keyword_prefix_typo_and_tag() {
        assert_eq!(
            ids(&search(shelf(), "what port does the database use", &[])),
            ["a"]
        );
        assert_eq!(ids(&search(shelf(), "integration", &[])), ["b"]);
        assert_eq!(ids(&search(shelf(), "deploy", &[])), ["c"]);
        assert_eq!(ids(&search(shelf(), "dokcer", &[])), ["b"]);
        assert_eq!(ids(&search(shelf(), "", &["db".to_owned()])), ["a"]);
        assert!(search(shelf(), "kubernetes", &[]).is_empty());
        assert_eq!(ids(&search(shelf(), "the of", &[])), ["c", "b", "a"]);
    }

    /// Better matches come first, and the most recent of equal ones.
    #[test]
    fn ranks_by_score_then_recency() {
        let found = search(shelf(), "tests database", &[]);
        assert_eq!(ids(&found), ["b", "a"]);
        let mut both = shelf();
        both.push(memory(
            "d",
            "Integration tests are slow.",
            &[],
            "2026-02-01T00:00:00Z",
        ));
        assert_eq!(ids(&search(both, "integration", &[]))[0], "d");
    }

    /// Edit distance counts additions, removals, changes and swaps of neighbours.
    #[test]
    fn distances() {
        assert!(within("docker", "dokcer", 1));
        assert!(!within("docker", "dkcore", 1));
        assert!(within("tests", "test", 1));
        assert!(!within("abc", "xyz", 2));
    }
}
