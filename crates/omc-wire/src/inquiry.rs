//! Questions the agent puts to the reader as a form, and what the reader answered.

/// One answer a question offers.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Alternative {
    /// What the agent calls the answer, and what it is told when the reader picks it.
    pub label: String,
    /// What picking it means, when the agent says.
    pub description: String,
}

/// One question of a form, with the answers it offers.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Question {
    /// A word or two naming what is asked, when the agent gives one.
    pub header: String,
    /// The question itself.
    pub text: String,
    /// Whether several answers may be picked.
    pub multiple: bool,
    /// The answers on offer.
    pub alternatives: Vec<Alternative>,
}

/// A form of questions the agent waits on the reader to answer before it goes on.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Inquiry {
    /// Names this form in the answer.
    pub ticket: u64,
    /// The questions, in the order they are asked.
    pub questions: Vec<Question>,
}

/// What the reader answered to one question.
#[derive(Clone, Debug, Default, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Reply {
    /// The labels of the alternatives picked.
    pub picks: Vec<String>,
    /// What the reader typed in place of, or beside, the alternatives.
    pub other: String,
}

/// Each of `questions` with what `replies` answered to it, joined with commas, or nothing when it was skipped.
pub fn summary(questions: &[Question], replies: &[Reply]) -> Vec<(String, Option<String>)> {
    questions
        .iter()
        .zip(replies)
        .map(|(question, reply)| {
            let mut given = reply.picks.clone();
            if !reply.other.trim().is_empty() {
                given.push(reply.other.trim().to_owned());
            }
            (
                question.text.clone(),
                (!given.is_empty()).then(|| given.join(", ")),
            )
        })
        .collect()
}
