//! A tool server of the older transport: one long event stream that names where messages go and carries the answers.

use std::sync::Arc;

use omc_mcp::Hub;
use omc_mcp::config::{Origin, Server, Transport};
use omc_tools::Spill;
use omc_wire::Reach;
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Mutex, mpsc};

/// The head and body of one request read from `stream`.
async fn read_request(stream: &mut TcpStream) -> (String, String) {
    let mut bytes = Vec::new();
    let mut buffer = [0u8; 4096];
    let end = loop {
        if let Some(at) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
            break at;
        }
        let read = stream.read(&mut buffer).await.unwrap();
        assert!(read > 0);
        bytes.extend_from_slice(&buffer[..read]);
    };
    let head = String::from_utf8_lossy(&bytes[..end]).into_owned();
    let length: usize = head
        .lines()
        .find_map(|line| {
            line.to_ascii_lowercase()
                .strip_prefix("content-length:")
                .map(|v| v.trim().parse().unwrap())
        })
        .unwrap_or(0);
    let mut body = bytes[end + 4..].to_vec();
    while body.len() < length {
        let read = stream.read(&mut buffer).await.unwrap();
        body.extend_from_slice(&buffer[..read]);
    }
    (head, String::from_utf8_lossy(&body).into_owned())
}

/// The answer the fake server gives to `message`, when it is a request.
fn answer(message: &Value) -> Option<Value> {
    let id = message.get("id")?.clone();
    let result = match message["method"].as_str()? {
        "initialize" => {
            json!({"protocolVersion": "2025-06-18", "capabilities": {"tools": {}}, "serverInfo": {"name": "old"}})
        }
        "tools/list" => {
            json!({"tools": [{"name": "hello", "description": "Greets", "inputSchema": {"type": "object"}}]})
        }
        "tools/call" => json!({"content": [{"type": "text", "text": "hello over sse"}]}),
        _ => return None,
    };
    Some(json!({"jsonrpc": "2.0", "id": id, "result": result}))
}

/// Serves one connection: the event stream on `GET /sse`, fed by `feed`, or a message on `POST /messages`, whose answer goes to `send`.
async fn serve(
    mut stream: TcpStream,
    send: mpsc::UnboundedSender<Value>,
    feed: Arc<Mutex<mpsc::UnboundedReceiver<Value>>>,
) {
    let (head, body) = read_request(&mut stream).await;
    if head.starts_with("GET /sse") {
        let start = "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\n\r\nevent: endpoint\ndata: /messages?session=1\n\n";
        stream.write_all(start.as_bytes()).await.unwrap();
        let mut feed = feed.lock().await;
        while let Some(message) = feed.recv().await {
            let event = format!("event: message\ndata: {message}\n\n");
            if stream.write_all(event.as_bytes()).await.is_err() {
                return;
            }
        }
    } else if head.starts_with("POST /messages?session=1") {
        let message: Value = serde_json::from_str(&body).unwrap();
        if let Some(reply) = answer(&message) {
            send.send(reply).unwrap();
        }
        let _ = stream
            .write_all(b"HTTP/1.1 202 Accepted\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
            .await;
    } else {
        let _ = stream
            .write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
            .await;
    }
}

/// The stream names the endpoint, messages posted there are answered on the stream, and the server's tools work.
#[tokio::test(flavor = "multi_thread")]
async fn tools_over_the_older_transport() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let (send, feed) = mpsc::unbounded_channel();
    let feed = Arc::new(Mutex::new(feed));
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            tokio::spawn(serve(stream, send.clone(), feed.clone()));
        }
    });
    let server = Server {
        name: "old".into(),
        reach: Reach::Http {
            url: format!("{base}/sse"),
            headers: vec![],
        },
        transport: Transport::Sse,
        folder: None,
        oauth: None,
        origin: Origin::User,
        file: None,
        summary: "old".into(),
    };
    let hub = Hub::new(Spill::disabled(), None, Arc::new(|_| {}));
    let reached = hub.connect(&[server]).await;
    assert!(reached.failed.is_empty(), "{:?}", reached.failed);
    assert_eq!(reached.tools.len(), 1);
    let output = reached.tools[0].call(json!({})).await.unwrap();
    assert_eq!(output.text, "hello over sse");
}
