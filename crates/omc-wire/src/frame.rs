//! One message per line: how commands and events travel.

use serde::Serialize;
use serde::de::DeserializeOwned;

/// `message` as one line of JSON, newline included.
///
/// # Errors
///
/// Fails when `message` cannot be written as JSON.
pub fn encode<T: Serialize>(message: &T) -> Result<String, serde_json::Error> {
    let mut line = serde_json::to_string(message)?;
    line.push('\n');
    Ok(line)
}

/// The message one `line` holds.
///
/// # Errors
///
/// Fails when `line` is not JSON or does not hold a `T`.
pub fn decode<T: DeserializeOwned>(line: &str) -> Result<T, serde_json::Error> {
    serde_json::from_str(line.trim_end())
}

#[cfg(test)]
/// Tests for the line framing.
mod tests {
    use super::{decode, encode};
    use crate::{Command, Event};

    /// A command survives a trip through a line.
    #[test]
    fn commands_round_trip() {
        let command = Command::Set {
            setting: "model".to_owned(),
            value: "x".to_owned(),
        };
        let line = encode(&command).unwrap();
        assert!(line.ends_with('\n') && line.matches('\n').count() == 1);
        assert_eq!(decode::<Command>(&line).unwrap(), command);
    }

    /// An event survives a trip through a line.
    #[test]
    fn events_round_trip() {
        let event = Event::Said("hi\nthere".to_owned());
        assert_eq!(decode::<Event>(&encode(&event).unwrap()).unwrap(), event);
    }
}
