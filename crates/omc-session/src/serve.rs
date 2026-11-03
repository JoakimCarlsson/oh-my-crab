//! The conversation over a pair of byte streams, one JSON message per line.

use omc_wire::{Command, Event, decode, encode};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::mpsc::unbounded_channel;

/// Runs a conversation with a client that sends commands on `input` and reads events from `output`, until the client closes `input` or sends [`Command::Close`].
///
/// # Errors
///
/// Fails when `input` cannot be read or `output` cannot be written.
pub async fn serve(
    input: impl AsyncRead + Unpin,
    mut output: impl AsyncWrite + Unpin,
) -> std::io::Result<()> {
    let (commands, received) = unbounded_channel::<Command>();
    let mut commands = Some(commands);
    let (out, mut events) = unbounded_channel::<Event>();
    let session = tokio::spawn(crate::session::run(received, out));
    let mut lines = BufReader::new(input).lines();
    loop {
        tokio::select! {
            line = lines.next_line(), if commands.is_some() => match line? {
                Some(line) => match decode::<Command>(&line) {
                    Ok(command) => {
                        if let Some(commands) = &commands {
                            let _ = commands.send(command);
                        }
                    }
                    Err(error) => {
                        write(&mut output, &Event::Failed(format!("bad command: {error}"))).await?;
                    }
                },
                None => commands = None,
            },
            event = events.recv() => match event {
                Some(event) => write(&mut output, &event).await?,
                None => break,
            },
        }
    }
    let _ = session.await;
    Ok(())
}

/// Writes `event` to `output` as one line and flushes it.
///
/// # Errors
///
/// Fails when `output` cannot be written.
async fn write(output: &mut (impl AsyncWrite + Unpin), event: &Event) -> std::io::Result<()> {
    let line = encode(event).map_err(std::io::Error::other)?;
    output.write_all(line.as_bytes()).await?;
    output.flush().await
}
