//! Putting rankings together, and the words keyword search looks for.

use std::collections::HashMap;
use std::hash::Hash;

/// The constant of reciprocal rank fusion: how much the first places of a ranking count over the later ones; 60 is the value of the paper that introduced it.
pub const FUSION_K: f64 = 60.0;

/// Words too common in questions to tell code apart.
const STOP_WORDS: &[&str] = &[
    "a", "an", "and", "are", "as", "at", "be", "by", "do", "does", "for", "from", "how", "i", "in",
    "is", "it", "of", "on", "or", "that", "the", "this", "to", "was", "we", "what", "when",
    "where", "which", "who", "why", "with", "you",
];

/// The items of `rankings`, each a list best first, ranked together by reciprocal rank fusion: an item scores `1 / (FUSION_K + place)` in each list it is in, places counting from 1, summed. Best first; among equals, the one met first.
pub fn fuse<K: Eq + Hash + Clone>(rankings: &[Vec<K>]) -> Vec<(K, f64)> {
    let mut scores: HashMap<K, (f64, usize)> = HashMap::new();
    let mut met = 0;
    for ranking in rankings {
        for (place, item) in ranking.iter().enumerate() {
            let entry = scores.entry(item.clone()).or_insert_with(|| {
                met += 1;
                (0.0, met)
            });
            entry.0 += 1.0 / (FUSION_K + place as f64 + 1.0);
        }
    }
    let mut fused: Vec<(K, f64, usize)> = scores
        .into_iter()
        .map(|(item, (score, order))| (item, score, order))
        .collect();
    fused.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.2.cmp(&b.2)));
    fused
        .into_iter()
        .map(|(item, score, _)| (item, score))
        .collect()
}

/// The cosine of the angle between `a` and `b`: 1 for the same direction, 0 for none in common; 0 when either is all zeros.
pub fn cosine(a: &[f32], b: &[f32]) -> f32 {
    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let norm = |v: &[f32]| v.iter().map(|x| x * x).sum::<f32>().sqrt();
    let product = norm(a) * norm(b);
    if product == 0.0 { 0.0 } else { dot / product }
}

/// The parts of the identifiers in `text` that a plain word split misses: the lowercase pieces of `camelCase` and `PascalCase` names, as one line of words.
pub(crate) fn identifier_parts(text: &str) -> String {
    let mut parts: Vec<String> = Vec::new();
    for word in text.split(|c: char| !c.is_alphanumeric()) {
        let pieces = camel_pieces(word);
        if pieces.len() > 1 {
            parts.extend(pieces);
        }
    }
    parts.join(" ")
}

/// The pieces of `word` at its case changes: `parseHTTPRequest` gives `parse`, `http` and `request`, lowercased.
fn camel_pieces(word: &str) -> Vec<String> {
    let chars: Vec<char> = word.chars().collect();
    let mut pieces = Vec::new();
    let mut start = 0;
    for at in 1..chars.len() {
        let (before, here) = (chars[at - 1], chars[at]);
        let next_lower = chars.get(at + 1).is_some_and(|c| c.is_lowercase());
        let boundary = (before.is_lowercase() && here.is_uppercase())
            || (before.is_uppercase() && here.is_uppercase() && next_lower);
        if boundary {
            pieces.push(chars[start..at].iter().collect::<String>().to_lowercase());
            start = at;
        }
    }
    if start < chars.len() {
        pieces.push(chars[start..].iter().collect::<String>().to_lowercase());
    }
    pieces
}

/// The words of `query` keyword search looks for, lowercase and without the most common words: each word split at its underscores, as the index splits them, and the pieces of a `camelCase` one beside it whole.
pub fn query_terms(query: &str) -> Vec<String> {
    let mut terms: Vec<String> = Vec::new();
    let mut push = |term: String| {
        if !term.is_empty() && !STOP_WORDS.contains(&term.as_str()) && !terms.contains(&term) {
            terms.push(term);
        }
    };
    for word in query.split(|c: char| !c.is_alphanumeric() && c != '_') {
        for piece in word.split('_') {
            push(piece.to_lowercase());
            let pieces = camel_pieces(piece);
            if pieces.len() > 1 {
                pieces.into_iter().for_each(&mut push);
            }
        }
    }
    terms
}

/// The full-text query that finds chunks holding any of `terms`, a term of three or more characters matching as a word start too; nothing when there are no terms.
pub(crate) fn match_expression(terms: &[String]) -> Option<String> {
    if terms.is_empty() {
        return None;
    }
    let parts: Vec<String> = terms
        .iter()
        .map(|term| {
            if term.chars().count() >= 3 {
                format!("\"{term}\"*")
            } else {
                format!("\"{term}\"")
            }
        })
        .collect();
    Some(parts.join(" OR "))
}

#[cfg(test)]
/// Tests for fusing rankings and finding terms.
mod tests {
    use super::*;

    /// An item high in both rankings beats one at the top of only one.
    #[test]
    fn fusion_rewards_agreement() {
        let fused = fuse(&[vec!["a", "b", "c"], vec!["b", "d", "a"]]);
        let order: Vec<&str> = fused.iter().map(|(item, _)| *item).collect();
        assert_eq!(order, ["b", "a", "d", "c"]);
        assert!((fused[0].1 - (1.0 / 62.0 + 1.0 / 61.0)).abs() < 1e-12);
        assert!(fuse::<u8>(&[]).is_empty());
    }

    /// Cosine of parallel, orthogonal and zero vectors.
    #[test]
    fn cosines() {
        assert!((cosine(&[1.0, 0.0], &[2.0, 0.0]) - 1.0).abs() < 1e-6);
        assert_eq!(cosine(&[1.0, 0.0], &[0.0, 1.0]), 0.0);
        assert_eq!(cosine(&[0.0, 0.0], &[1.0, 1.0]), 0.0);
    }

    /// Identifiers split at case changes, digits and underscores, and common words go.
    #[test]
    fn terms_split_identifiers() {
        assert_eq!(
            identifier_parts("fn parseHTTPRequest(x: u8) -> MyType2"),
            "parse http request my type2"
        );
        assert_eq!(
            query_terms("where is the load_config of SessionHost?"),
            ["load", "config", "sessionhost", "session", "host"]
        );
        assert_eq!(
            match_expression(&query_terms("db io")).unwrap(),
            "\"db\" OR \"io\""
        );
        assert_eq!(match_expression(&query_terms("the of")), None);
    }
}
