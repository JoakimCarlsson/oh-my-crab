//! Helpers shared by the unit tests.

use std::path::PathBuf;

/// Creates an empty directory named `name` under the workspace's git-ignored
/// `build/tests/omc-tools` and returns its path.
pub fn scratch(name: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../build/tests/omc-tools")
        .join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir.canonicalize().unwrap()
}

/// A [`crate::Host`] that records what it is shown and answers as it is told.
#[derive(Default)]
pub struct RecordingHost {
    /// The plans shown, in order.
    pub plans: std::sync::Mutex<Vec<Vec<omc_wire::PlanStep>>>,
    /// The forms put, in order.
    pub asked: std::sync::Mutex<Vec<Vec<omc_wire::Question>>>,
    /// What the reader answers to forms, or nothing to abandon them.
    pub replies: std::sync::Mutex<Option<Vec<omc_wire::Reply>>>,
    /// The plans put for approval, in order.
    pub plans_proposed: std::sync::Mutex<Vec<String>>,
    /// The name the agent gave the conversation.
    pub named: std::sync::Mutex<Option<String>>,
    /// Whether the reader approves plans.
    pub approve: std::sync::Mutex<bool>,
}

#[async_trait::async_trait]
impl crate::Host for RecordingHost {
    /// Records `steps`.
    fn plan(&self, steps: Vec<omc_wire::PlanStep>) {
        self.plans.lock().unwrap().push(steps);
    }

    /// Records `questions` and answers with the scripted replies.
    async fn inquire(&self, questions: Vec<omc_wire::Question>) -> Option<Vec<omc_wire::Reply>> {
        self.asked.lock().unwrap().push(questions);
        self.replies.lock().unwrap().clone()
    }

    /// Records the name when none was recorded yet.
    fn name(&self, title: &str) -> bool {
        let mut named = self.named.lock().unwrap();
        if named.is_some() {
            return false;
        }
        *named = Some(title.to_owned());
        true
    }

    /// Records `plan` and answers with the scripted verdict.
    async fn approve_plan(&self, plan: &str) -> bool {
        self.plans_proposed.lock().unwrap().push(plan.to_owned());
        *self.approve.lock().unwrap()
    }
}

/// A request a [`serve`] server received: its method and path with query, its
/// headers with lowercase names, and its body.
#[derive(Debug, Clone, Default)]
pub struct Received {
    /// The method, such as `GET`.
    pub method: String,
    /// The path and query, such as `/search?q=x`.
    pub target: String,
    /// The headers, names lowercased.
    pub headers: Vec<(String, String)>,
    /// The body, as text.
    pub body: String,
}

impl Received {
    /// The value of the header `name`, given in lowercase, when it was sent.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }
}

/// What a [`serve`] server answers: a status, a content type and a body.
pub type Answer = (u16, &'static str, Vec<u8>);

/// Starts an HTTP/1.1 server on a free local port that answers every request
/// with what `answer` makes of it, one request per connection, and returns its
/// address and the requests it received so far.
///
/// The server runs until the test's runtime stops.
pub async fn serve(
    answer: impl Fn(&Received) -> Answer + Send + Sync + 'static,
) -> (
    std::net::SocketAddr,
    std::sync::Arc<std::sync::Mutex<Vec<Received>>>,
) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let received = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let log = received.clone();
    let answer = std::sync::Arc::new(answer);
    tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                return;
            };
            let mut bytes = Vec::new();
            let mut chunk = [0u8; 4096];
            let head_end = loop {
                if let Some(end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                    break end + 4;
                }
                let read = stream.read(&mut chunk).await.unwrap_or(0);
                if read == 0 {
                    break bytes.len();
                }
                bytes.extend_from_slice(&chunk[..read]);
            };
            let head = String::from_utf8_lossy(&bytes[..head_end]).into_owned();
            let mut lines = head.lines();
            let mut start = lines.next().unwrap_or_default().split_whitespace();
            let mut request = Received {
                method: start.next().unwrap_or_default().to_owned(),
                target: start.next().unwrap_or_default().to_owned(),
                ..Received::default()
            };
            request.headers = lines
                .filter_map(|line| line.split_once(':'))
                .map(|(key, value)| (key.trim().to_ascii_lowercase(), value.trim().to_owned()))
                .collect();
            let length: usize = request
                .header("content-length")
                .and_then(|value| value.parse().ok())
                .unwrap_or(0);
            while bytes.len() < head_end + length {
                let read = stream.read(&mut chunk).await.unwrap_or(0);
                if read == 0 {
                    break;
                }
                bytes.extend_from_slice(&chunk[..read]);
            }
            request.body =
                String::from_utf8_lossy(&bytes[head_end.min(bytes.len())..]).into_owned();
            let (status, content_type, body) = answer(&request);
            log.lock().unwrap().push(request);
            let head = format!(
                "HTTP/1.1 {status} X\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\n\
                 Connection: close\r\n\r\n",
                body.len()
            );
            let _ = stream.write_all(head.as_bytes()).await;
            let _ = stream.write_all(&body).await;
        }
    });
    (address, received)
}
