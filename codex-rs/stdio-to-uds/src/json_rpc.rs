//! Adapts an IDE's JSON lines to a shared app-server connection without owning its lifecycle.

use std::io::Read;
use std::path::Path;

use anyhow::Context;
use anyhow::bail;
use bytes::Bytes;
use codex_uds::UnixStream;
use futures::SinkExt;
use futures::StreamExt;
use tokio::io::AsyncRead;
use tokio::io::AsyncWrite;
use tokio::io::AsyncWriteExt;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_util::codec::FramedRead;
use tokio_util::codec::LinesCodec;
use tokio_util::io::StreamReader;

// Match tungstenite's default message limit and bound incomplete stdin lines too.
const MAX_MESSAGE_BYTES: usize = 64 << 20;

/// Connect to an existing daemon and preserve JSON-RPC IDs, requests and notifications.
/// This never starts a private server or reconnects implicitly: the IDE must initialize
/// and resume its threads on a new connection after a transport failure.
pub async fn run_json_rpc(socket_path: &Path) -> anyhow::Result<()> {
    let stream = UnixStream::connect(socket_path).await.with_context(|| {
        format!(
            "failed to connect to shared app-server at {}",
            socket_path.display()
        )
    })?;
    let (websocket, _) = tokio_tungstenite::client_async("ws://localhost/", stream)
        .await
        .context("failed to initialize shared app-server WebSocket")?;
    // tokio::io::stdin uses an uncancellable runtime blocking task. If the daemon
    // exits while the IDE keeps stdin open, that task prevents process shutdown.
    // A dedicated reader thread is bounded by the channel and does not hold the
    // async runtime alive after the connection fails.
    let (sender, receiver) = tokio::sync::mpsc::channel(1);
    std::thread::Builder::new()
        .name("json-rpc-stdin".into())
        .spawn(move || {
            let mut stdin = std::io::stdin().lock();
            let mut buffer = [0; 8192];
            loop {
                match stdin.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(size) => {
                        if sender
                            .blocking_send(Ok(Bytes::copy_from_slice(&buffer[..size])))
                            .is_err()
                        {
                            break;
                        }
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(error) => {
                        let _ = sender.blocking_send(Err(error));
                        break;
                    }
                }
            }
        })?;
    let chunks = futures::stream::unfold(receiver, |mut receiver| async {
        receiver.recv().await.map(|chunk| (chunk, receiver))
    })
    .boxed();
    relay(StreamReader::new(chunks), tokio::io::stdout(), websocket).await
}

async fn relay<R, W, S>(
    input: R,
    mut output: W,
    websocket: WebSocketStream<S>,
) -> anyhow::Result<()>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
    S: AsyncRead + AsyncWrite + Unpin,
{
    let mut input = FramedRead::new(input, LinesCodec::new_with_max_length(MAX_MESSAGE_BYTES));
    let (mut sender, mut receiver) = websocket.split();
    let client_to_server = async {
        while let Some(line) = input.next().await {
            let line = line.context("failed to read JSON-RPC line")?;
            if line.trim().is_empty() {
                continue;
            }
            let _: serde_json::Value =
                serde_json::from_str(&line).context("invalid JSON on client stdin")?;
            sender
                .send(Message::Text(line.into()))
                .await
                .context("failed to send request to shared app-server")?;
        }
        // Only detach this IDE connection. The daemon and its tasks stay alive.
        sender
            .close()
            .await
            .context("failed to close shared app-server connection")?;
        anyhow::Ok(())
    };
    let server_to_client = async {
        while let Some(message) = receiver.next().await {
            match message.context("shared app-server transport failed")? {
                Message::Text(text) => {
                    // A WebSocket JSON value can span lines; stdio requires one line.
                    let value: serde_json::Value = serde_json::from_str(&text)
                        .context("invalid JSON from shared app-server")?;
                    let mut line = serde_json::to_vec(&value)?;
                    line.push(b'\n');
                    output
                        .write_all(&line)
                        .await
                        .context("failed to write JSON-RPC output")?;
                    output.flush().await?;
                }
                // Polling the stream again flushes tungstenite's queued pong.
                Message::Ping(_) | Message::Pong(_) => {}
                Message::Close(_) => break,
                Message::Binary(_) | Message::Frame(_) => {
                    bail!("unexpected non-text shared app-server message");
                }
            }
        }
        bail!("shared app-server disconnected; reconnect and resume the thread")
    };
    // Drive both directions independently so a blocked socket write cannot stop
    // response consumption. Either EOF or a transport failure detaches the bridge.
    tokio::select! {
        result = client_to_server => result,
        result = server_to_client => result,
    }
}

#[cfg(test)]
#[path = "json_rpc_tests.rs"]
mod tests;
