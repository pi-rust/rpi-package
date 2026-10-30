use rpi_mcp_core::{
    oauth::network::{
        authorization_metadata_url, protected_metadata_url, NetworkPolicy, OAuthNetwork,
    },
    AuthError, Control,
};
use serde_json::json;
use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    task::JoinHandle,
};
use url::Url;

struct Fixture {
    origin: String,
    requests: Arc<Mutex<Vec<Vec<u8>>>>,
    task: JoinHandle<()>,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.task.abort();
    }
}
impl Fixture {
    async fn new() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let captures = requests.clone();
        let task = tokio::spawn(async move {
            loop {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut bytes = Vec::new();
                let head_end = loop {
                    let mut buf = [0u8; 4096];
                    let n = stream.read(&mut buf).await.unwrap();
                    if n == 0 {
                        break None;
                    }
                    bytes.extend_from_slice(&buf[..n]);
                    if let Some(pos) = bytes.windows(4).position(|s| s == b"\r\n\r\n") {
                        break Some(pos + 4);
                    }
                };
                let Some(head_end) = head_end else { continue };
                let head = String::from_utf8_lossy(&bytes[..head_end]).to_ascii_lowercase();
                let size = head
                    .lines()
                    .find_map(|line| {
                        line.strip_prefix("content-length: ")
                            .and_then(|v| v.parse::<usize>().ok())
                    })
                    .unwrap_or(0);
                while bytes.len() < head_end + size {
                    let mut buf = [0u8; 4096];
                    let n = stream.read(&mut buf).await.unwrap();
                    if n == 0 {
                        break;
                    }
                    bytes.extend_from_slice(&buf[..n]);
                }
                let path = head.split_whitespace().nth(1).unwrap().to_owned();
                captures.lock().unwrap().push(bytes);
                if path == "/slow-head" {
                    tokio::time::sleep(Duration::from_secs(5)).await;
                    continue;
                }
                let status = if path.starts_with("/redirect/") {
                    path.trim_start_matches("/redirect/").parse().unwrap()
                } else if path == "/error" {
                    400
                } else if path == "/register" {
                    201
                } else {
                    200
                };
                if path == "/slow-body" {
                    let _ = stream
                        .write_all(
                            b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 100\r\nConnection: close\r\n\r\n{",
                        )
                        .await;
                    tokio::time::sleep(Duration::from_secs(5)).await;
                    continue;
                }
                let body = match path.as_str() {
                    "/huge" | "/chunked-huge" => format!("\"{}\"", "x".repeat(65535)),
                    "/exact" => format!("\"{}\"", "x".repeat(65534)),
                    "/bad" => "CANARY_not_json".into(),
                    "/error" => "{\"error\":\"invalid_grant\",\"secret\":\"CANARY\"}".into(),
                    _ => "{\"ok\":true}".into(),
                };
                let location = if (300..400).contains(&status) {
                    "Location: /target\r\n"
                } else {
                    ""
                };
                let content_type = match path.as_str() {
                    "/type/missing" => String::new(),
                    "/type/text" => "Content-Type: text/plain\r\n".into(),
                    "/type/malformed" => "Content-Type: application/json; broken\r\n".into(),
                    "/type/duplicate" => {
                        "Content-Type: application/json\r\nContent-Type: application/json\r\n"
                            .into()
                    }
                    "/type/conflicting" => {
                        "Content-Type: application/json\r\nContent-Type: text/html\r\n".into()
                    }
                    "/type/list" => "Content-Type: application/json, text/html\r\n".into(),
                    "/type/wildcard" => "Content-Type: application/*+json\r\n".into(),
                    "/type/large" => {
                        format!("Content-Type: application/json; x={}\r\n", "a".repeat(1024))
                    }
                    "/type/suffix" => {
                        "Content-Type: application/problem+json; charset=utf-8\r\n".into()
                    }
                    "/type/parameter" => {
                        "Content-Type: Application/JSON; charset=\"utf-8\"\r\n".into()
                    }
                    _ => "Content-Type: application/json\r\n".into(),
                };
                let wire = if path == "/chunked-huge" {
                    // No Content-Length: enforce the incremental cap.
                    format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n{:x}\r\n{}\r\n0\r\n\r\n", body.len(), body)
                } else {
                    format!("HTTP/1.1 {status} Test\r\n{content_type}{location}Content-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len())
                };
                let _ = stream.write_all(wire.as_bytes()).await;
            }
        });
        Self {
            origin,
            requests,
            task,
        }
    }
    fn network(&self) -> OAuthNetwork {
        let mut policy = NetworkPolicy::new(vec![]).unwrap();
        policy.allow_development_origin(&self.origin).unwrap();
        OAuthNetwork::new(policy).unwrap()
    }
    fn url(&self, path: &str) -> Url {
        Url::parse(&format!("{}{path}", self.origin)).unwrap()
    }
    fn count(&self) -> usize {
        self.requests.lock().unwrap().len()
    }
}
fn end() -> Instant {
    Instant::now() + Duration::from_secs(3)
}

#[test]
fn exact_origins_and_explicit_exceptions() {
    for origin in [
        "http://example.com",
        "https://example.com/path",
        "https://example.com?x",
        "https://u:p@example.com",
        "https://example.com/#f",
        "file:///x",
    ] {
        assert!(NetworkPolicy::new(vec![origin.into()]).is_err());
    }
    assert!(NetworkPolicy::new(vec!["HTTPS://EXAMPLE.COM:443/".into()]).is_ok());
    let mut policy = NetworkPolicy::new(vec![]).unwrap();
    for origin in [
        "http://localhost",
        "http://foo.localhost",
        "http://0.0.0.0",
        "http://192.168.0.1",
        "https://127.0.0.1",
        "http://[::ffff:127.0.0.1]",
        "http://127.0.0.1/path",
    ] {
        assert!(policy.allow_development_origin(origin).is_err());
    }
    assert!(policy
        .allow_development_origin("http://127.0.0.1:9876")
        .is_ok());
    assert!(policy.allow_development_origin("http://[::1]:9876").is_ok());
    assert!(policy
        .allow_private_origin("http://127.0.0.1", vec!["127.0.0.1".parse().unwrap()])
        .is_err());
    assert!(policy
        .allow_private_origin("https://private.example", vec![])
        .is_err());
}

#[test]
fn rfc_well_known_path_prepend() {
    let resource = Url::parse("https://example.com/tenant/a%2Fb/?x=1").unwrap();
    assert_eq!(
        protected_metadata_url(&resource).unwrap().as_str(),
        "https://example.com/.well-known/oauth-protected-resource/tenant/a%2Fb/"
    );
    let issuer = Url::parse("https://example.com/tenant").unwrap();
    assert_eq!(
        authorization_metadata_url(&issuer).unwrap().as_str(),
        "https://example.com/.well-known/oauth-authorization-server/tenant"
    );
    assert_eq!(
        authorization_metadata_url(&Url::parse("https://example.com/").unwrap())
            .unwrap()
            .path(),
        "/.well-known/oauth-authorization-server"
    );
    assert!(authorization_metadata_url(&resource).is_err());
    assert!(protected_metadata_url(&Url::parse("http://127.0.0.1/").unwrap()).is_err());
}

#[tokio::test]
async fn real_json_form_status_and_redaction_no_credential_headers_or_cache() {
    let fixture = Fixture::new().await;
    let net = fixture.network();
    let control = Control::default();
    for _ in 0..2 {
        assert_eq!(
            net.get_json(&fixture.url("/json"), &control, end())
                .await
                .unwrap()
                .json,
            json!({"ok":true})
        );
    }
    let reply = net
        .post_json(
            &fixture.url("/register"),
            &json!({"client":"a"}),
            &control,
            end(),
        )
        .await
        .unwrap();
    assert_eq!(reply.status, 201);
    let reply = net
        .post_form(
            &fixture.url("/error"),
            &[("code".into(), "a +&=é".into())],
            &control,
            end(),
        )
        .await
        .unwrap();
    assert_eq!(reply.status, 400);
    assert_eq!(reply.json["error"], "invalid_grant");
    assert!(!format!("{reply:?}").contains("CANARY"));
    assert_eq!(fixture.count(), 4);
    let requests = fixture.requests.lock().unwrap();
    for bytes in requests.iter() {
        let wire = String::from_utf8_lossy(bytes).to_ascii_lowercase();
        let head = wire.split("\r\n\r\n").next().unwrap();
        for forbidden in [
            "authorization:",
            "cookie:",
            "mcp-session-id:",
            "mcp-protocol-version:",
            "proxy-authorization:",
        ] {
            assert!(!head.contains(forbidden));
        }
    }
    let wire = String::from_utf8_lossy(&requests[2]);
    assert!(wire.contains("application/json"));
    assert!(wire.ends_with("{\"client\":\"a\"}"));
    let wire = String::from_utf8_lossy(&requests[3]);
    assert!(wire.contains("application/x-www-form-urlencoded"));
    assert!(wire.ends_with("code=a+%2B%26%3D%C3%A9"));
}

#[tokio::test]
async fn redirects_never_reach_location_target() {
    let fixture = Fixture::new().await;
    let net = fixture.network();
    for code in [300, 301, 302, 303, 304, 305, 307, 308, 399] {
        assert_eq!(
            net.get_json(
                &fixture.url(&format!("/redirect/{code}")),
                &Control::default(),
                end()
            )
            .await
            .unwrap_err(),
            AuthError::Exchange
        );
    }
    assert_eq!(fixture.count(), 9);
    assert!(fixture
        .requests
        .lock()
        .unwrap()
        .iter()
        .all(|r| !String::from_utf8_lossy(r).contains(" /target ")));
}

#[tokio::test]
async fn request_and_response_limits_and_invalid_json_are_bounded() {
    let fixture = Fixture::new().await;
    let net = fixture.network();
    let control = Control::default();
    assert!(net
        .get_json(&fixture.url("/exact"), &control, end())
        .await
        .is_ok());
    for path in ["/huge", "/chunked-huge", "/bad"] {
        let error = net
            .get_json(&fixture.url(path), &control, end())
            .await
            .unwrap_err();
        assert_eq!(error, AuthError::Exchange);
        assert!(!error.to_string().contains("CANARY"));
    }
    let before = fixture.count();
    assert!(net
        .post_json(
            &fixture.url("/json"),
            &json!("x".repeat(65536)),
            &control,
            end()
        )
        .await
        .is_err());
    assert!(net
        .post_form(
            &fixture.url("/json"),
            &[("x".into(), "%".repeat(30000))],
            &control,
            end()
        )
        .await
        .is_err());
    assert!(net
        .get_json(
            &fixture.url(&format!("/{}", "x".repeat(8192))),
            &control,
            end()
        )
        .await
        .is_err());
    assert_eq!(fixture.count(), before);
}

#[tokio::test]
async fn cancellation_and_deadline_cover_headers_and_body() {
    for path in ["/slow-head", "/slow-body"] {
        let fixture = Fixture::new().await;
        let net = fixture.network();
        let result = net
            .get_json(
                &fixture.url(path),
                &Control::default(),
                Instant::now() + Duration::from_millis(80),
            )
            .await;
        assert_eq!(result.unwrap_err(), AuthError::Timeout);
        assert_eq!(fixture.count(), 1);
        let fixture = Fixture::new().await;
        let net = fixture.network();
        let control = Control::default();
        let cancel = async {
            tokio::time::sleep(Duration::from_millis(50)).await;
            control.cancel();
        };
        let url = fixture.url(path);
        let (_, result) = tokio::join!(cancel, net.get_json(&url, &control, end()));
        assert_eq!(result.unwrap_err(), AuthError::Cancelled);
    }
}

#[tokio::test]
async fn resource_policy_hook_is_explicit_and_side_effect_free() {
    let fixture = Fixture::new().await;
    let net =
        OAuthNetwork::new(NetworkPolicy::new(vec!["https://approved.example".into()]).unwrap())
            .unwrap();
    assert!(net
        .validate_resource_url(&Url::parse("https://approved.example/resource").unwrap())
        .is_ok());
    for url in [
        "http://public.example/resource",
        "http://127.0.0.1/resource",
        "https://other.example/resource",
        "https://approved.example:444/resource",
        "https://u:p@approved.example/resource",
        "https://approved.example/resource#fragment",
    ] {
        assert_eq!(
            net.validate_resource_url(&Url::parse(url).unwrap()),
            Err(AuthError::UnsafeUrl)
        );
    }
    assert!(net
        .validate_resource_url(&fixture.url("/resource"))
        .is_err());
    assert!(fixture
        .network()
        .validate_resource_url(&fixture.url("/resource"))
        .is_ok());
    assert_eq!(fixture.count(), 0);
}

#[tokio::test]
async fn all_json_operations_require_one_bounded_valid_json_media_type() {
    let fixture = Fixture::new().await;
    let net = fixture.network();
    for kind in [
        "missing",
        "text",
        "malformed",
        "duplicate",
        "conflicting",
        "list",
        "wildcard",
        "large",
        "suffix",
        "parameter",
    ] {
        let url = fixture.url(&format!("/type/{kind}"));
        for operation in 0..3 {
            let result = match operation {
                0 => net.get_json(&url, &Control::default(), end()).await,
                1 => {
                    net.post_json(&url, &json!({}), &Control::default(), end())
                        .await
                }
                _ => net.post_form(&url, &[], &Control::default(), end()).await,
            };
            assert_eq!(
                result.is_ok(),
                matches!(kind, "suffix" | "parameter"),
                "{kind}, operation {operation}: {result:?}"
            );
        }
    }
    assert_eq!(fixture.count(), 30);
}

#[tokio::test]
async fn rejected_policy_and_pre_cancelled_work_make_no_request() {
    let fixture = Fixture::new().await;
    let net = OAuthNetwork::new(NetworkPolicy::new(vec![]).unwrap()).unwrap();
    assert_eq!(
        net.get_json(&fixture.url("/"), &Control::default(), end())
            .await
            .unwrap_err(),
        AuthError::UnsafeUrl
    );
    let net = fixture.network();
    let control = Control::default();
    control.cancel();
    assert_eq!(
        net.get_json(&fixture.url("/"), &control, end())
            .await
            .unwrap_err(),
        AuthError::Cancelled
    );
    assert_eq!(
        net.get_json(&fixture.url("/"), &Control::default(), Instant::now())
            .await
            .unwrap_err(),
        AuthError::Timeout
    );
    assert_eq!(fixture.count(), 0);
}
