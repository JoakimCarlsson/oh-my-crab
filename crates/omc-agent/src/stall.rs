//! Replies that announce work and then stop.
//!
//! A model now and then ends its turn with a reply such as "Launching the fixers now" that says what it is about to do and makes no tool call, so nothing happens until the user asks it to go on. Such a reply is told apart from a finished answer by its last sentences alone, without asking any model: one of them must open with the agent saying it acts now, such as "I'll run", "Let me check" or "Starting the next round". The match leans hard toward letting the turn end: a reply that asks the user something, offers to do something, holds a condition or a negation, or reports finished work is never taken for a stall.

use omc_providers::{Reply, Stop};

use crate::todos::asks_question;

/// Told once a turn when its reply announced work but made no tool call.
pub(crate) const NOTICE: &str = "<harness-notice>
Your last reply said you would act now, but it made no tool call, so nothing ran and the turn would end here. Do it now with a real tool call. If something blocks you, say plainly what it is instead of announcing the work.
</harness-notice>";

/// How many of the last sentences of a reply are looked at.
const LAST_SENTENCES: usize = 2;

/// The longest a sentence that opens with an action in progress, such as "Starting the next round", may be without saying `now` or `next`, in words.
const SHORT_PROGRESS: usize = 6;

/// Words at the start of a sentence that say nothing about what follows and are skipped.
const FILLERS: [&str; 11] = [
    "ok", "okay", "alright", "right", "so", "now", "next", "then", "great", "good", "first",
];

/// How a sentence opens when the agent says it will act itself.
const PROMISES: [&str; 12] = [
    "i'll",
    "i will",
    "i'm going to",
    "i am going to",
    "i'm now",
    "i am now",
    "let me",
    "let's",
    "now i'll",
    "now i will",
    "i'm gonna",
    "i shall",
];

/// Words that may come between a promise and its verb, such as the `now` of "I'll now run".
const BETWEEN: [&str; 7] = ["now", "go", "ahead", "and", "first", "also", "then"];

/// The verbs of the work an agent announces; a promise counts only when one of them follows it.
const VERBS: [&str; 60] = [
    "start",
    "begin",
    "launch",
    "run",
    "rerun",
    "re-run",
    "read",
    "look",
    "check",
    "inspect",
    "fix",
    "write",
    "rewrite",
    "edit",
    "implement",
    "update",
    "add",
    "create",
    "investigate",
    "search",
    "open",
    "apply",
    "build",
    "test",
    "dig",
    "proceed",
    "continue",
    "spawn",
    "kick",
    "do",
    "work",
    "make",
    "try",
    "verify",
    "patch",
    "refactor",
    "explore",
    "examine",
    "review",
    "grep",
    "scan",
    "move",
    "wire",
    "port",
    "get",
    "handle",
    "tackle",
    "finish",
    "install",
    "compile",
    "debug",
    "trace",
    "change",
    "remove",
    "delete",
    "rename",
    "address",
    "dispatch",
    "fire",
    "set",
];

/// How a sentence opens when it names an action that is under way, such as "Running the tests now".
const UNDER_WAY: [&str; 30] = [
    "starting",
    "launching",
    "running",
    "rerunning",
    "re-running",
    "reading",
    "kicking off",
    "spawning",
    "proceeding",
    "continuing",
    "beginning",
    "moving on",
    "working on",
    "looking",
    "checking",
    "fixing",
    "dispatching",
    "firing off",
    "applying",
    "writing",
    "editing",
    "implementing",
    "investigating",
    "searching",
    "digging",
    "updating",
    "building",
    "testing",
    "verifying",
    "exploring",
];

/// Words that, anywhere in a sentence, make it something other than a plain promise to act now: a condition, an offer, a negation, a wish or a later time.
const HEDGES: [&str; 27] = [
    "if",
    "when",
    "once",
    "unless",
    "want",
    "wants",
    "would",
    "could",
    "can",
    "might",
    "may",
    "should",
    "not",
    "won't",
    "don't",
    "can't",
    "cannot",
    "never",
    "later",
    "tomorrow",
    "eventually",
    "someday",
    "prefer",
    "happy",
    "glad",
    "whether",
    "nothing",
];

/// Verbs that make a sentence opening with an action under way a statement about what was found, as in "Running the suite showed three failures".
const FINDINGS: [&str; 18] = [
    "is", "are", "was", "were", "has", "had", "shows", "showed", "shown", "gives", "gave", "fails",
    "failed", "passes", "passed", "works", "worked", "found",
];

/// Phrases in the last paragraph that report finished work.
const FINISHED: [&str; 12] = [
    "summary",
    "all done",
    "is done",
    "are done",
    "completed",
    "is complete",
    "are complete",
    "finished",
    "all tests pass",
    "nothing left",
    "nothing more to do",
    "that's it",
];

/// The notice to send the model when `reply` would end the turn on a promise to act: it finished, made no tool call, and its text announces work the agent is about to do; nothing otherwise.
pub(crate) fn notice(reply: &Reply) -> Option<&'static str> {
    (reply.tool_calls.is_empty() && reply.stop == Stop::Finished && announces_work(&reply.text))
        .then_some(NOTICE)
}

/// Whether `text` ends by announcing work the agent is about to do itself, rather than by answering, reporting or asking.
///
/// Only the last [`LAST_SENTENCES`] sentences of the last paragraph are looked at. A reply that ends by asking the user something, or whose last paragraph reports finished work, never counts.
pub(crate) fn announces_work(text: &str) -> bool {
    if text.trim().is_empty() || asks_question(text) {
        return false;
    }
    let paragraph = last_paragraph(text);
    let lower = normalized(paragraph);
    if FINISHED.iter().any(|phrase| lower.contains(phrase)) {
        return false;
    }
    let sentences = sentences(&lower);
    sentences
        .iter()
        .rev()
        .take(LAST_SENTENCES)
        .any(|sentence| promises(sentence))
}

/// The last paragraph of `text` that holds anything, paragraphs being parted by a blank line.
fn last_paragraph(text: &str) -> &str {
    text.trim()
        .rsplit("\n\n")
        .map(str::trim)
        .find(|paragraph| !paragraph.is_empty())
        .unwrap_or_default()
}

/// `text` in lower case with curly apostrophes made straight and the marks of emphasis, headings, lists and quotes taken out, so that the phrases match however the reply was set.
fn normalized(text: &str) -> String {
    text.to_lowercase()
        .replace(['\u{2019}', '\u{2018}'], "'")
        .chars()
        .filter(|character| {
            !matches!(
                character,
                '*' | '_' | '`' | '#' | '>' | '"' | '\u{201c}' | '\u{201d}'
            )
        })
        .collect()
}

/// The sentences of `text`, which ends with one that has no closing mark too: split at full stops, exclamation marks, colons, semicolons, dashes set apart by spaces and line breaks, each trimmed of spaces and list markers, the empty ones left out.
fn sentences(text: &str) -> Vec<String> {
    text.replace(" - ", "\n")
        .replace(" \u{2014} ", "\n")
        .replace('\u{2014}', "\n")
        .split(['.', '!', ':', ';', '\n', '\u{2026}'])
        .map(|sentence| {
            sentence
                .trim()
                .trim_start_matches(['-', '+', ' '])
                .trim()
                .to_owned()
        })
        .filter(|sentence| !sentence.is_empty())
        .collect()
}

/// The words of `sentence`, split at spaces and commas, with the fillers at its start left out.
fn words(sentence: &str) -> Vec<&str> {
    let words: Vec<&str> = sentence
        .split([' ', ','])
        .filter(|word| !word.is_empty())
        .collect();
    let skipped = words
        .iter()
        .take_while(|word| FILLERS.contains(word))
        .count();
    words[skipped..].to_vec()
}

/// Whether `sentence`, normalized, is the agent saying it acts now: a promise followed by a verb of work, an action under way, or the reader told that a word from them sets the work going; never a sentence that holds a hedge.
fn promises(sentence: &str) -> bool {
    let words = words(sentence);
    if words.is_empty() || words.iter().any(|word| HEDGES.contains(word)) {
        return false;
    }
    let opening = words.join(" ");
    promised_verb(&words) || under_way(&opening, &words) || waits_for_go(&opening)
}

/// Whether `words` open with a promise of the agent's, such as "I'll", followed, past words such as `now` or `go ahead and`, by a verb of work.
fn promised_verb(words: &[&str]) -> bool {
    let opening = words.join(" ");
    PROMISES.iter().any(|promise| {
        let Some(rest) = opening.strip_prefix(promise) else {
            return false;
        };
        let Some(rest) = rest.strip_prefix(' ') else {
            return false;
        };
        rest.split(' ')
            .find(|word| !BETWEEN.contains(word))
            .is_some_and(|verb| VERBS.contains(&verb))
    })
}

/// Whether `opening`, the sentence whose words are `words`, opens with an action under way and either says it happens now or next, or is short and states no finding.
fn under_way(opening: &str, words: &[&str]) -> bool {
    if !UNDER_WAY
        .iter()
        .any(|action| opening == *action || opening.starts_with(&format!("{action} ")))
    {
        return false;
    }
    if words.iter().any(|word| FINDINGS.contains(word)) {
        return false;
    }
    words.iter().any(|word| matches!(*word, "now" | "next")) || words.len() <= SHORT_PROGRESS
}

/// Whether `opening` tells the reader to say a word for the agent to start, as in "Say go and I'll work".
fn waits_for_go(opening: &str) -> bool {
    opening.starts_with("say ")
        && (opening.contains(" and i'll ") || opening.contains(" and i will "))
}

#[cfg(test)]
/// Tests for telling replies that announce work from those that end the turn.
mod tests {
    use super::*;

    /// Replies that announce work and stop are taken for stalls, the examples seen in real conversations included.
    #[test]
    fn announcements_are_caught() {
        for text in [
            "Launching parallel fixers now",
            "Starting the next round",
            "Reading both repos now",
            "Say 'go' and I'll work",
            "Say \"go\" and I'll work through the list.",
            "Found the cause in `parse.rs`.\n\nI'll now fix it and run the tests.",
            "Let me check the remaining callers.",
            "OK, I will run the full suite next.",
            "The plan is clear. I'm going to implement step two now.",
            "Running the tests now…",
            "**Next:** let me look at the config loader.",
            "I\u{2019}ll start with the parser.",
            "Three files need changes.\n\n- Kicking off the edits now.",
        ] {
            assert!(announces_work(text), "{text}");
        }
    }

    /// Final answers, summaries, questions, offers, negations and plans for later are never taken for stalls.
    #[test]
    fn endings_are_left_alone() {
        for text in [
            "",
            "The function returns the length of the list.",
            "## Summary\n\nFixed the parser and added tests. I'll leave the docs as they are.",
            "All done: the build passes and the docs are updated.\n\nI'll run nothing more.",
            "Fixed both bugs; all tests pass.\n\nStarting the next round was not needed.",
            "Should I also update the docs?",
            "Want me to run the full suite?",
            "I can refactor the loader if you want.",
            "I will not change the public API.",
            "I won't touch the lock file.",
            "I'll update the docs in a follow-up once this lands.",
            "Let me know which option you prefer.",
            "If the tests fail again, I'll dig into the fixtures.",
            "Running the suite showed three failures in `io.rs`.",
            "Reading the code, the bug is in the retry loop.",
            "Let's keep the current design.",
            "I'll be here if you need anything else.",
            "The work is completed. Starting the server is up to you.",
        ] {
            assert!(!announces_work(text), "{text}");
        }
    }

    /// Only a finished reply without tool calls earns the notice.
    #[test]
    fn only_finished_replies_without_calls_are_nudged() {
        let mut reply = Reply {
            text: "Launching the fixers now.".into(),
            stop: Stop::Finished,
            ..Reply::default()
        };
        assert_eq!(notice(&reply), Some(NOTICE));
        reply.stop = Stop::Length;
        assert_eq!(notice(&reply), None);
        reply.stop = Stop::Finished;
        reply.tool_calls.push(omc_providers::ToolCall {
            id: "c".into(),
            name: "bash".into(),
            arguments: serde_json::json!({}),
        });
        assert_eq!(notice(&reply), None);
    }

    /// Only the last sentences count: a promise early in a long reply that ends with a plain statement is no stall.
    #[test]
    fn only_the_last_sentences_count() {
        assert!(!announces_work(
            "I'll check the loader. It reads the file. It parses the header. The header is valid."
        ));
        assert!(announces_work(
            "The header is valid. I'll check the loader next."
        ));
    }
}
