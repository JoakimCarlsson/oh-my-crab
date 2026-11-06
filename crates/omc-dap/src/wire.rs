//! The framing of debug adapter protocol messages: a `Content-Length` header, a blank line, and a JSON body.

use serde_json::Value;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// The largest message body read, so that an adapter that sends a wrong length cannot make the harness hold gigabytes.
const LARGEST_BODY: usize = 64 * 1024 * 1024;

/// The next message on `reader`, framed by its `Content-Length` header; nothing once the stream ends or holds something that is not a message.
pub(crate) async fn read_message<R: AsyncBufRead + Unpin>(reader: &mut R) -> Option<Value> {
    let mut length = None;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).await.ok()? == 0 {
            return None;
        }
        let line = line.trim_end();
        if line.is_empty() {
            if length.is_some() {
                break;
            }
            continue;
        }
        if let Some((name, value)) = line.split_once(':')
            && name.trim().eq_ignore_ascii_case("content-length")
        {
            length = value.trim().parse::<usize>().ok();
        }
    }
    let length = length.filter(|length| *length <= LARGEST_BODY)?;
    let mut body = vec![0; length];
    reader.read_exact(&mut body).await.ok()?;
    serde_json::from_slice(&body).ok()
}

/// Writes `message` to `writer` with its `Content-Length` header, and flushes it.
///
/// # Errors
///
/// Returns the error of the stream when it cannot be written.
pub(crate) async fn write_message<W: AsyncWrite + Unpin + ?Sized>(
    writer: &mut W,
    message: &Value,
) -> std::io::Result<()> {
    let body = message.to_string();
    let mut framed = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
    framed.extend_from_slice(body.as_bytes());
    writer.write_all(&framed).await?;
    writer.flush().await
}

#[cfg(test)]
/// Tests for the framing.
mod tests {
    use serde_json::json;
    use tokio::io::BufReader;

    use super::*;

    /// A message written is read back whole, and several in a row are read one at a time.
    #[tokio::test]
    async fn messages_round_trip() {
        let (mut near, far) = tokio::io::duplex(4096);
        write_message(
            &mut near,
            &json!({"seq": 1, "type": "event", "event": "initialized"}),
        )
        .await
        .unwrap();
        write_message(&mut near, &json!({"seq": 2, "text": "ünïcode"}))
            .await
            .unwrap();
        drop(near);
        let mut reader = BufReader::new(far);
        assert_eq!(
            read_message(&mut reader).await.unwrap()["event"],
            "initialized"
        );
        assert_eq!(read_message(&mut reader).await.unwrap()["text"], "ünïcode");
        assert!(read_message(&mut reader).await.is_none());
    }

    /// Other headers and a header in another case are understood, and a body that is not JSON ends the stream.
    #[tokio::test]
    async fn headers_are_read_loosely() {
        let raw = b"Content-Type: x\r\ncontent-length: 2\r\n\r\n{}Content-Length: 3\r\n\r\nnot";
        let mut reader = BufReader::new(&raw[..]);
        assert_eq!(read_message(&mut reader).await.unwrap(), json!({}));
        assert!(read_message(&mut reader).await.is_none());
    }

    /// A body longer than the largest allowed is refused.
    #[tokio::test]
    async fn huge_bodies_are_refused() {
        let raw = format!("Content-Length: {}\r\n\r\n{{}}", LARGEST_BODY + 1);
        let mut reader = BufReader::new(raw.as_bytes());
        assert!(read_message(&mut reader).await.is_none());
    }
}
