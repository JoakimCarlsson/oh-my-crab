//! Visuals the agent puts in front of the reader without waiting on them.

/// What an exhibit's content is, which says how the client renders it.
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum ExhibitKind {
    /// Mermaid source, rendered as a diagram.
    Diagram,
    /// A self-contained HTML document, rendered sandboxed with the network denied.
    Prototype,
}

/// A visual the agent shows the reader: a diagram or a prototype, which the turn does not wait on.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Exhibit {
    /// The id of the tool call that showed it, as in the [`crate::Tool`] events of that call.
    pub id: String,
    /// What the exhibit is called.
    pub title: String,
    /// What `content` is.
    pub kind: ExhibitKind,
    /// The Mermaid source of a diagram, or the HTML document of a prototype.
    pub content: String,
}

#[cfg(test)]
/// Tests for exhibits on the wire.
mod tests {
    use super::*;
    use crate::Event;

    /// A shown exhibit is written with its kind by name and reads back as it was.
    #[test]
    fn a_shown_exhibit_round_trips() {
        let event = Event::Show(Exhibit {
            id: "call-1".to_owned(),
            title: "Login flow".to_owned(),
            kind: ExhibitKind::Diagram,
            content: "graph TD; A-->B".to_owned(),
        });
        let line = crate::encode(&event).unwrap();
        assert_eq!(
            line.trim_end(),
            r#"{"Show":{"id":"call-1","title":"Login flow","kind":"Diagram","content":"graph TD; A-->B"}}"#
        );
        assert_eq!(crate::decode::<Event>(&line).unwrap(), event);
    }
}
