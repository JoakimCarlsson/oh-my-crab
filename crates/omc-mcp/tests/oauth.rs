//! Signing in to a tool server with OAuth, against a fake authorization server and tool server on a local port.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use oauth2::{PkceCodeChallenge, PkceCodeVerifier};
use omc_mcp::config::{Origin, Server, Transport};
use omc_mcp::{Grant, Hub, Notice, Tokens};
use omc_tools::Spill;
use omc_wire::Reach;
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

/// What the fake servers saw and hold.
#[derive(Default)]
struct Seen {
    /// The PKCE challenge the sign-in page was opened with.
    challenge: Option<String>,
    /// The redirect address the sign-in page was opened with.
    redirect: Option<String>,
    /// The resource the sign-in page was opened with.
    resource: Option<String>,
    /// How many clients registered.
    registered: usize,
    /// The grant types the token endpoint was asked for, in order.
    grants: Vec<String>,
    /// The tokens the tool server accepts.
    valid: Vec<String>,
}

/// One HTTP request: its method, target, headers (lowercase names) and body.
struct Request {
    /// The method.
    method: String,
    /// The path and query.
    target: String,
    /// The headers.
    headers: HashMap<String, String>,
    /// The body.
    body: String,
}

/// Reads one request from `stream`.
async fn read_request(stream: &mut TcpStream) -> Option<Request> {
    let mut bytes = Vec::new();
    let mut buffer = [0u8; 4096];
    let head_end = loop {
        if let Some(at) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
            break at;
        }
        let read = stream.read(&mut buffer).await.ok()?;
        if read == 0 {
            return None;
        }
        bytes.extend_from_slice(&buffer[..read]);
    };
    let head = String::from_utf8_lossy(&bytes[..head_end]).into_owned();
    let mut lines = head.lines();
    let mut first = lines.next()?.split_whitespace();
    let method = first.next()?.to_owned();
    let target = first.next()?.to_owned();
    let headers: HashMap<String, String> = lines
        .filter_map(|line| line.split_once(':'))
        .map(|(name, value)| (name.trim().to_ascii_lowercase(), value.trim().to_owned()))
        .collect();
    let length: usize = headers
        .get("content-length")
        .and_then(|value| value.parse().ok())
        .unwrap_or(0);
    let mut body = bytes[head_end + 4..].to_vec();
    while body.len() < length {
        let read = stream.read(&mut buffer).await.ok()?;
        if read == 0 {
            break;
        }
        body.extend_from_slice(&buffer[..read]);
    }
    Some(Request {
        method,
        target,
        headers,
        body: String::from_utf8_lossy(&body).into_owned(),
    })
}

/// Writes an answer with `status`, extra `headers` and `body`.
async fn write_response(
    stream: &mut TcpStream,
    status: &str,
    headers: &[(&str, String)],
    body: &str,
) {
    let mut text = format!(
        "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n",
        body.len()
    );
    for (name, value) in headers {
        text.push_str(&format!("{name}: {value}\r\n"));
    }
    text.push_str("\r\n");
    text.push_str(body);
    let _ = stream.write_all(text.as_bytes()).await;
    let _ = stream.shutdown().await;
}

/// The pairs of the form or query `text`.
fn pairs(text: &str) -> HashMap<String, String> {
    url::form_urlencoded::parse(text.as_bytes())
        .into_owned()
        .collect()
}

/// The token endpoint's answer to `form`, noting what it was asked in `seen`.
fn token_answer(form: &HashMap<String, String>, seen: &mut Seen) -> Value {
    let grant = form["grant_type"].clone();
    seen.grants.push(grant.clone());
    if grant == "authorization_code" {
        assert_eq!(form["code"], "the-code");
        let verifier = PkceCodeVerifier::new(form["code_verifier"].clone());
        let expected = PkceCodeChallenge::from_code_verifier_sha256(&verifier);
        assert_eq!(Some(expected.as_str()), seen.challenge.as_deref());
        assert_eq!(Some(&form["redirect_uri"]), seen.redirect.as_ref());
        seen.valid.push("token-1".into());
        json!({"access_token": "token-1", "refresh_token": "refresh-1", "expires_in": 3600, "token_type": "Bearer"})
    } else {
        assert_eq!(form["refresh_token"], "refresh-1");
        seen.valid.push("token-2".into());
        json!({"access_token": "token-2", "expires_in": 3600, "token_type": "Bearer"})
    }
}

/// Answers one request to the fake servers at `base`.
async fn answer(mut stream: TcpStream, base: String, seen: Arc<Mutex<Seen>>) {
    let Some(request) = read_request(&mut stream).await else {
        return;
    };
    let json_type = ("Content-Type", "application/json".to_owned());
    let (path, query) = request
        .target
        .split_once('?')
        .unwrap_or((&request.target, ""));
    match (request.method.as_str(), path) {
        ("GET", "/.well-known/oauth-protected-resource/mcp") => {
            let body = json!({"resource": format!("{base}/mcp"), "authorization_servers": [base], "scopes_supported": ["tools"]});
            write_response(&mut stream, "200 OK", &[json_type], &body.to_string()).await;
        }
        ("GET", "/.well-known/oauth-authorization-server") => {
            let body = json!({
                "issuer": base,
                "authorization_endpoint": format!("{base}/authorize"),
                "token_endpoint": format!("{base}/token"),
                "registration_endpoint": format!("{base}/register"),
                "code_challenge_methods_supported": ["S256"],
            });
            write_response(&mut stream, "200 OK", &[json_type], &body.to_string()).await;
        }
        ("POST", "/register") => {
            seen.lock().unwrap().registered += 1;
            let asked: Value = serde_json::from_str(&request.body).unwrap();
            assert_eq!(asked["token_endpoint_auth_method"], "none");
            write_response(
                &mut stream,
                "201 Created",
                &[json_type],
                r#"{"client_id":"client-1"}"#,
            )
            .await;
        }
        ("GET", "/authorize") => {
            let asked = pairs(query);
            assert_eq!(asked["client_id"], "client-1");
            assert_eq!(asked["code_challenge_method"], "S256");
            assert_eq!(asked["scope"], "tools");
            {
                let mut seen = seen.lock().unwrap();
                seen.challenge = Some(asked["code_challenge"].clone());
                seen.redirect = Some(asked["redirect_uri"].clone());
                seen.resource = Some(asked["resource"].clone());
            }
            let back = format!(
                "{}?code=the-code&state={}",
                asked["redirect_uri"], asked["state"]
            );
            write_response(&mut stream, "302 Found", &[("Location", back)], "").await;
        }
        ("POST", "/token") => {
            let form = pairs(&request.body);
            let body = token_answer(&form, &mut seen.lock().unwrap());
            write_response(&mut stream, "200 OK", &[json_type], &body.to_string()).await;
        }
        ("POST", "/mcp") => {
            let token = request
                .headers
                .get("authorization")
                .and_then(|value| value.strip_prefix("Bearer "))
                .map(str::to_owned);
            let valid = token.is_some_and(|token| seen.lock().unwrap().valid.contains(&token));
            if !valid {
                let challenge = format!(
                    r#"Bearer resource_metadata="{base}/.well-known/oauth-protected-resource/mcp""#
                );
                write_response(
                    &mut stream,
                    "401 Unauthorized",
                    &[("WWW-Authenticate", challenge)],
                    "",
                )
                .await;
                return;
            }
            let message: Value = serde_json::from_str(&request.body).unwrap();
            let Some(id) = message.get("id").cloned() else {
                write_response(&mut stream, "202 Accepted", &[], "").await;
                return;
            };
            let result = match message["method"].as_str().unwrap() {
                "initialize" => {
                    json!({"protocolVersion": "2025-06-18", "capabilities": {"tools": {}}, "serverInfo": {"name": "fake"}})
                }
                "tools/list" => {
                    json!({"tools": [{"name": "whoami", "description": "Says who", "inputSchema": {"type": "object"}}]})
                }
                "tools/call" => json!({"content": [{"type": "text", "text": "signed in"}]}),
                other => panic!("unexpected {other}"),
            };
            let body = json!({"jsonrpc": "2.0", "id": id, "result": result});
            write_response(&mut stream, "200 OK", &[json_type], &body.to_string()).await;
        }
        _ => write_response(&mut stream, "404 Not Found", &[], "").await,
    }
}

/// Starts the fake servers and returns their address and what they saw.
async fn fake() -> (String, Arc<Mutex<Seen>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let seen = Arc::new(Mutex::new(Seen::default()));
    let (held, address) = (seen.clone(), base.clone());
    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                return;
            };
            tokio::spawn(answer(stream, address.clone(), held.clone()));
        }
    });
    (base, seen)
}

/// A scratch folder for the test called `name`, emptied.
fn scratch(name: &str) -> PathBuf {
    let folder = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../build/tests")
        .join(name);
    let _ = std::fs::remove_dir_all(&folder);
    std::fs::create_dir_all(&folder).unwrap();
    folder
}

/// The tool server at `base`.
fn server(base: &str) -> Server {
    Server {
        name: "secure".into(),
        reach: Reach::Http {
            url: format!("{base}/mcp"),
            headers: vec![],
        },
        transport: Transport::Streamable,
        folder: None,
        oauth: None,
        origin: Origin::Project,
        file: None,
        summary: format!("{base}/mcp"),
    }
}

/// A server that answers 401 gets a sign-in page through discovery and registration; following it as a browser would brings the code back, the tokens are kept, and the server's tools arrive and work. A later hub signs in with the kept token, and an expired one is refreshed.
#[tokio::test(flavor = "multi_thread")]
async fn sign_in_end_to_end() {
    let (base, seen) = fake().await;
    let folder = scratch("mcp-oauth");
    let tokens = Tokens::new(folder.join("mcp-auth.json"));
    let notices = Arc::new(Mutex::new(Vec::new()));
    let heard = notices.clone();
    let hub = Hub::new(
        Spill::disabled(),
        Some(tokens.clone()),
        Arc::new(move |notice| heard.lock().unwrap().push(notice)),
    );
    let reached = hub.connect(&[server(&base)]).await;
    assert!(reached.tools.is_empty());
    assert_eq!(
        reached.failed,
        ["secure: waiting for the reader to sign in"]
    );
    let page = match notices.lock().unwrap().first().cloned() {
        Some(Notice::SignIn { server, url }) => {
            assert_eq!(server, "secure");
            url
        }
        other => panic!("no sign-in page: {other:?}"),
    };
    assert!(page.starts_with(&format!("{base}/authorize?")));
    let browser = reqwest::Client::new();
    let landed = browser.get(&page).send().await.unwrap();
    assert!(landed.status().is_success());
    assert!(landed.text().await.unwrap().contains("Signed in"));
    let mut arrived = Vec::new();
    for _ in 0..100 {
        arrived = hub.arrived();
        if !arrived.is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(arrived.len(), 1);
    assert_eq!(arrived[0].name(), "mcp__secure__whoami");
    let output = arrived[0].call(json!({})).await.unwrap();
    assert_eq!(output.text, "signed in");
    assert!(matches!(
        notices.lock().unwrap().last(),
        Some(Notice::SignedIn { .. })
    ));
    {
        let seen = seen.lock().unwrap();
        assert_eq!(seen.registered, 1);
        assert_eq!(
            seen.resource.as_deref(),
            Some(format!("{base}/mcp").as_str())
        );
        assert_eq!(seen.grants, ["authorization_code"]);
    }
    let kept = tokens.get(&format!("{base}/mcp")).unwrap();
    assert_eq!(kept.access, "token-1");
    assert_eq!(kept.client_id, "client-1");

    let again = Hub::new(Spill::disabled(), Some(tokens.clone()), Arc::new(|_| {}));
    let reached = again.connect(&[server(&base)]).await;
    assert_eq!(reached.tools.len(), 1, "{:?}", reached.failed);

    tokens
        .set(
            &format!("{base}/mcp"),
            Grant {
                expires_at_ms: Some(1),
                ..kept
            },
        )
        .unwrap();
    let refreshed = Hub::new(Spill::disabled(), Some(tokens.clone()), Arc::new(|_| {}));
    let reached = refreshed.connect(&[server(&base)]).await;
    assert_eq!(reached.tools.len(), 1, "{:?}", reached.failed);
    assert_eq!(
        seen.lock().unwrap().grants,
        ["authorization_code", "refresh_token"]
    );
    assert_eq!(
        tokens.get(&format!("{base}/mcp")).unwrap().access,
        "token-2"
    );
    let _ = std::fs::remove_dir_all(&folder);
}

/// Without a place to keep sign-ins, a server that wants one is reported as unreachable and no page is opened.
#[tokio::test(flavor = "multi_thread")]
async fn no_store_no_sign_in() {
    let (base, _) = fake().await;
    let hub = Hub::new(
        Spill::disabled(),
        None,
        Arc::new(|_| panic!("no notice expected")),
    );
    let reached = hub.connect(&[server(&base)]).await;
    assert!(reached.tools.is_empty());
    assert!(reached.failed[0].contains("401"), "{:?}", reached.failed);
}
