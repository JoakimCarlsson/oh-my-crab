//! The binary event stream AWS streams responses in (`application/vnd.amazon.eventstream`).
//!
//! Each message is a prelude (total length, headers length, both big-endian
//! `u32`, and the CRC-32 of those eight bytes), the headers, the payload, and
//! the CRC-32 of everything before it.

/// Bytes of the prelude: two lengths and their checksum.
const PRELUDE: usize = 12;

/// Bytes of the checksum that ends a message.
const TRAILER: usize = 4;

/// The largest message accepted, against a corrupt length asking for gigabytes.
const MAX_MESSAGE: usize = 16 * 1024 * 1024;

/// One message of an event stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Message {
    /// The headers with string values, by name; headers of other types are skipped.
    pub headers: Vec<(String, String)>,
    /// The payload.
    pub payload: Vec<u8>,
}

impl Message {
    /// The value of the string header `name`, when the message has one.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }
}

/// Collects the bytes of an event stream as they arrive and hands out each message once it is whole.
#[derive(Debug, Default)]
pub(crate) struct Decoder {
    /// Bytes received and not yet handed out.
    buffer: Vec<u8>,
}

impl Decoder {
    /// Adds `bytes` received from the stream.
    pub fn push(&mut self, bytes: &[u8]) {
        self.buffer.extend_from_slice(bytes);
    }

    /// The next whole message, or nothing until more bytes arrive.
    ///
    /// # Errors
    ///
    /// Fails when a length is impossible or a checksum does not match, after which the stream cannot be read further.
    pub fn next(&mut self) -> Result<Option<Message>, String> {
        if self.buffer.len() < PRELUDE {
            return Ok(None);
        }
        let total = be_u32(&self.buffer[0..4]) as usize;
        let headers_len = be_u32(&self.buffer[4..8]) as usize;
        if crc32fast::hash(&self.buffer[0..8]) != be_u32(&self.buffer[8..12]) {
            return Err("event stream prelude checksum does not match".to_owned());
        }
        if total < PRELUDE + TRAILER + headers_len || total > MAX_MESSAGE {
            return Err(format!(
                "event stream message has an impossible length {total}"
            ));
        }
        if self.buffer.len() < total {
            return Ok(None);
        }
        let frame: Vec<u8> = self.buffer.drain(..total).collect();
        if crc32fast::hash(&frame[..total - TRAILER]) != be_u32(&frame[total - TRAILER..]) {
            return Err("event stream message checksum does not match".to_owned());
        }
        let headers = parse_headers(&frame[PRELUDE..PRELUDE + headers_len])?;
        let payload = frame[PRELUDE + headers_len..total - TRAILER].to_vec();
        Ok(Some(Message { headers, payload }))
    }
}

/// The big-endian `u32` in the four bytes of `bytes`.
fn be_u32(bytes: &[u8]) -> u32 {
    u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
}

/// The string headers in the header block `bytes`.
///
/// # Errors
///
/// Fails when a header runs past the block or has an unknown type.
fn parse_headers(bytes: &[u8]) -> Result<Vec<(String, String)>, String> {
    let truncated = || "event stream header is cut short".to_owned();
    let mut headers = Vec::new();
    let mut at = 0;
    while at < bytes.len() {
        let name_len = *bytes.get(at).ok_or_else(truncated)? as usize;
        let name = bytes.get(at + 1..at + 1 + name_len).ok_or_else(truncated)?;
        let name = String::from_utf8_lossy(name).into_owned();
        at += 1 + name_len;
        let kind = *bytes.get(at).ok_or_else(truncated)?;
        at += 1;
        let fixed = match kind {
            0 | 1 => Some(0),
            2 => Some(1),
            3 => Some(2),
            4 => Some(4),
            5 | 8 => Some(8),
            9 => Some(16),
            6 | 7 => None,
            other => return Err(format!("event stream header has unknown type {other}")),
        };
        match fixed {
            Some(size) => {
                bytes.get(at..at + size).ok_or_else(truncated)?;
                at += size;
            }
            None => {
                let len_bytes = bytes.get(at..at + 2).ok_or_else(truncated)?;
                let len = u16::from_be_bytes([len_bytes[0], len_bytes[1]]) as usize;
                let value = bytes.get(at + 2..at + 2 + len).ok_or_else(truncated)?;
                if kind == 7 {
                    headers.push((name, String::from_utf8_lossy(value).into_owned()));
                }
                at += 2 + len;
            }
        }
    }
    Ok(headers)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Encodes a message with the string `headers` and `payload`, as AWS frames one.
    pub(crate) fn frame(headers: &[(&str, &str)], payload: &[u8]) -> Vec<u8> {
        let mut block = Vec::new();
        for (name, value) in headers {
            block.push(name.len() as u8);
            block.extend_from_slice(name.as_bytes());
            block.push(7);
            block.extend_from_slice(&(value.len() as u16).to_be_bytes());
            block.extend_from_slice(value.as_bytes());
        }
        let total = (PRELUDE + block.len() + payload.len() + TRAILER) as u32;
        let mut out = Vec::new();
        out.extend_from_slice(&total.to_be_bytes());
        out.extend_from_slice(&(block.len() as u32).to_be_bytes());
        let prelude_crc = crc32fast::hash(&out);
        out.extend_from_slice(&prelude_crc.to_be_bytes());
        out.extend_from_slice(&block);
        out.extend_from_slice(payload);
        let crc = crc32fast::hash(&out);
        out.extend_from_slice(&crc.to_be_bytes());
        out
    }

    /// Messages split anywhere across pushes come out whole, with their string headers and payloads.
    #[test]
    fn decodes_messages_split_across_reads() {
        let mut bytes = frame(
            &[(":event-type", "chunk"), (":message-type", "event")],
            b"{}",
        );
        bytes.extend(frame(&[(":message-type", "exception")], b"boom"));
        let mut decoder = Decoder::default();
        let mut messages = Vec::new();
        for piece in bytes.chunks(5) {
            decoder.push(piece);
            while let Some(message) = decoder.next().unwrap() {
                messages.push(message);
            }
        }
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].header(":event-type"), Some("chunk"));
        assert_eq!(messages[0].payload, b"{}");
        assert_eq!(messages[1].header(":message-type"), Some("exception"));
        assert_eq!(messages[1].payload, b"boom");
        assert_eq!(messages[1].header(":event-type"), None);
    }

    /// Headers of other types are skipped, and a damaged message is an error.
    #[test]
    fn skips_other_header_types_and_rejects_damage() {
        let block = [
            &[4u8][..],
            b"flag",
            &[0],
            &[3],
            b"num",
            &[4, 0, 0, 0, 7],
            &[4],
            b"name",
            &[7, 0, 1],
            b"x",
        ]
        .concat();
        assert_eq!(
            parse_headers(&block).unwrap(),
            [("name".to_owned(), "x".to_owned())]
        );
        let mut damaged = frame(&[(":message-type", "event")], b"payload");
        let last = damaged.len() - 6;
        damaged[last] ^= 0xff;
        let mut decoder = Decoder::default();
        decoder.push(&damaged);
        assert!(decoder.next().is_err());
        let mut short = Decoder::default();
        short.push(&frame(&[], b"abc")[..10]);
        assert_eq!(short.next().unwrap(), None);
    }
}
