//! Module-level parity with the pinned Pi fixtures, plus OAuth and isolation units.
//! Fixtures under `tests/fixtures/antigravity/` are deterministic subsets of the full suites,
//! which live in github.com/yansigit/rpi-provider-plugins; the selection script is kept outside
//! this repository. Each case is copied as parsed, key order intact. Cases that encode the
//! fork-only `durationMs` terminal field are left out (see the gap notes); the physical
//! request bodies in `stream.json` do not depend on that field and are still compared.
//! catalog.json: 133 of 20,376 cases; the first of each category (retirement, reasoning,
//! effort presence, model family, resolved keys), then every 352nd remaining case.
//! conversion.json: 75 of 855 traces; the first of each (function, source, caller file,
//! argument kind, result kind), then every 14th remaining trace.
//! metadata.json: 91 of 898 cases; the first of each category per list, then every 163rd
//! remaining case. The three `stores` traces are kept whole because each one is stateful.
//! stream.json: 15 of 65 cases; the first of each (model, stop reason, error, id and request
//! counts, status, body presence) category. A kept case keeps the earlier cases that share its
//! session.
use super::{
    auth::{self, BuildClient, ClientSource, Endpoints},
    catalog, convert,
    metadata::{self, SessionContext, SessionStore, StepCountMode},
    redaction, request, schema, transport,
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    future::Future,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Mutex,
    },
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};

const FIXTURES: &str = "tests/fixtures/antigravity";
const FAKE_ID: &str = "fake-client.invalid";
const FAKE_SECRET: &str = "fake-secret-value";

fn fixture(name: &str) -> Value {
    let path = format!("{}/{FIXTURES}/{name}", env!("CARGO_MANIFEST_DIR"));
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

fn equal(actual: Value, expected: &Value) {
    assert_eq!(&actual, expected);
    assert_eq!(
        serde_json::to_string(&actual).unwrap(),
        serde_json::to_string(expected).unwrap(),
        "field order"
    );
}

#[test]
fn catalog_and_routing_match_pinned_original() {
    let fixture = fixture("catalog.json");
    assert_eq!(fixture["api"], catalog::API);
    assert_eq!(fixture["baseUrl"], catalog::BASE_URL);
    assert_eq!(fixture["retiredMessage"], catalog::RETIRED_MESSAGE);
    assert_eq!(
        fixture["catalog"],
        Value::Array(catalog::entries().to_vec())
    );
    for case in fixture["cases"].as_array().unwrap() {
        let id = case["id"].as_str().unwrap();
        assert_eq!(case["retired"], catalog::is_retired(id), "retirement: {id}");
        let resolved = catalog::resolve_pi(
            id,
            case["reasoning"].as_bool().unwrap(),
            case["effort"].as_str(),
        );
        assert_eq!(
            case["resolved"],
            serde_json::to_value(resolved).unwrap(),
            "case: {case}"
        );
    }
}

#[test]
fn request_and_schema_match_original_tests_with_pi_1_3() {
    let fixture = fixture("conversion.json");
    assert_eq!(
        fixture["reference"],
        "a97f9055017f3c1546afbfe61a19d50b428be3d8"
    );
    let mut counts = (0, 0);
    for trace in fixture["traces"].as_array().unwrap() {
        let args = trace["args"].as_array().unwrap();
        let options = args
            .get(1)
            .and_then(|arg| arg.get("value"))
            .cloned()
            .unwrap_or(json!({}));
        let actual = match trace["function"].as_str().unwrap() {
            "toGeminiSchema" => {
                counts.0 += 1;
                match args[0].get("value") {
                    Some(value) => {
                        json!({"value": schema::to_gemini_schema(value, serde_json::from_value(options).unwrap())})
                    }
                    None => json!({"undefined": true}),
                }
            }
            "buildGeminiRequest" => {
                counts.1 += 1;
                json!({"value": convert::build_gemini_request(&args[0]["value"], serde_json::from_value(options).unwrap()).unwrap()})
            }
            other => panic!("Unknown fixture function: {other}"),
        };
        equal(actual, &trace["result"]);
    }
    // Counts come from the subset file, so they track its selection rather than a fixed total.
    let traces = fixture["traces"].as_array().unwrap();
    assert!(!traces.is_empty());
    assert_eq!(counts.0 + counts.1, traces.len(), "every trace is compared");
    assert!(
        counts.0 > 0 && counts.1 > 0,
        "both functions are exercised: {counts:?}"
    );
}

#[test]
fn metadata_and_generation_match_pinned_source() {
    let fixture = fixture("metadata.json");
    for case in fixture["fnv"].as_array().unwrap() {
        equal(
            json!(metadata::fnv1a64_signed(case["input"].as_str().unwrap())),
            &case["result"],
        );
    }
    for case in fixture["steps"].as_array().unwrap() {
        equal(
            json!(metadata::count_steps(
                &case["payload"],
                serde_json::from_value(case["mode"].clone()).unwrap()
            )),
            &case["result"],
        );
    }
    for case in fixture["calls"].as_array().unwrap() {
        let mut session: SessionContext = serde_json::from_value(case["before"].clone()).unwrap();
        equal(
            metadata::build(
                &mut session,
                &case["payload"],
                case["model"].as_str().unwrap(),
                1784285195116,
                serde_json::from_value::<StepCountMode>(case["mode"].clone()).unwrap(),
            ),
            &case["result"],
        );
        equal(serde_json::to_value(session).unwrap(), &case["after"]);
    }
    for case in fixture["generations"].as_array().unwrap() {
        let mut config = case["config"].clone();
        metadata::apply_generation(case["model"].as_str().unwrap(), &mut config);
        equal(config, &case["result"]);
    }
    for case in fixture["finalizations"].as_array().unwrap() {
        let mut request = case["before"].clone();
        let mut session = SessionContext::new("", "conversation".into(), "trajectory".into());
        let id = metadata::finalize(
            &mut request,
            case["model"].as_str().unwrap(),
            &mut session,
            1000,
        );
        equal(json!(id), &case["result"]);
        equal(request, &case["request"]);
        equal(serde_json::to_value(session).unwrap(), &case["session"]);
    }
}

#[test]
fn session_lifetime_and_ids_match_original() {
    let fixture = fixture("metadata.json");
    for trace in fixture["stores"].as_array().unwrap() {
        let mut store = SessionStore::with_limits(
            "file:///workspace".into(),
            trace["limits"]["ttlMs"].as_i64().unwrap(),
            trace["limits"]["maxEntries"].as_u64().unwrap() as usize,
        );
        for op in trace["operations"].as_array().unwrap() {
            let key = op["key"].as_str().unwrap();
            let mut ids = op["ids"].as_array().unwrap().iter();
            let mut next_id = || {
                ids.next()
                    .expect("unexpected ID allocation")
                    .as_str()
                    .unwrap()
                    .to_owned()
            };
            let now = op["now"].as_i64().unwrap();
            match op["op"].as_str().unwrap() {
                "getOrCreate" => equal(
                    serde_json::to_value(store.get_or_create(key, now, &mut next_id)).unwrap(),
                    &op["result"],
                ),
                "beginRequest" => {
                    let (session, timestamp) = store.begin_request(key, now, &mut next_id);
                    equal(
                        json!({"session":session,"timestamp":timestamp}),
                        &op["result"],
                    );
                }
                "completeExecution" => store.complete_execution(key, &mut next_id),
                "delete" => store.delete(key),
                "clear" => store.clear(),
                other => panic!("unrecognized store operation {other}"),
            }
            assert_eq!(ids.next(), None, "unused ID allocation: {op}");
            assert_eq!(store.len(), op["size"].as_u64().unwrap() as usize, "{op}");
            for pair in op["has"].as_array().unwrap() {
                assert_eq!(
                    store.has(pair[0].as_str().unwrap()),
                    pair[1].as_bool().unwrap(),
                    "{op}"
                );
            }
        }
    }
}

#[test]
fn physical_request_bodies_match_original_provider() {
    let fixture = fixture("stream.json");
    let now = fixture["now"].as_i64().unwrap();
    let mut sessions = SessionStore::new(String::new());
    for case in fixture["cases"].as_array().unwrap() {
        let mut ids = case["ids"].as_array().unwrap().iter();
        let result = request::prepare(
            &case["model"],
            &case["context"],
            &case["options"],
            &mut sessions,
            now,
            &mut || {
                ids.next()
                    .expect("unexpected request ID")
                    .as_str()
                    .unwrap()
                    .into()
            },
        );
        if case["requests"].as_array().unwrap().is_empty() {
            assert_eq!(
                result.err().unwrap(),
                case["result"]["errorMessage"].as_str().unwrap(),
                "{}",
                case["name"]
            );
            assert_eq!(ids.next(), None, "refused request allocated IDs");
        } else {
            let request = result.unwrap();
            assert_eq!(
                request.body("fixture-project"),
                case["requests"][0]["body"].as_str().unwrap(),
                "{}",
                case["name"]
            );
            if matches!(
                case["result"]["stopReason"].as_str(),
                Some("stop" | "length")
            ) {
                let execution = ids
                    .next_back()
                    .expect("execution ID")
                    .as_str()
                    .unwrap()
                    .to_owned();
                sessions.complete_execution(
                    &metadata::session_key(&case["context"], &case["options"]),
                    &mut || execution.clone(),
                );
            }
        }
    }
}

fn client(id: &'static str, secret: &'static str) -> ClientSource {
    ClientSource {
        build: BuildClient {
            id: Some(id),
            secret: Some(secret),
        },
        runtime: |_| None,
    }
}

fn unconfigured() -> ClientSource {
    ClientSource {
        build: BuildClient::NONE,
        runtime: |_| None,
    }
}

fn form_pairs(body: &str) -> HashMap<String, String> {
    url::form_urlencoded::parse(body.as_bytes())
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect()
}

/// Issues a login with fixed entropy and parks it in `logins`; returns its state and verifier.
fn issue(
    logins: &mut auth::PendingLogins,
    verifier: u8,
    nonce: u8,
    now: Instant,
) -> (String, String) {
    let issued =
        auth::authorize_with_entropy(&[verifier; 32], &[nonce; 32], client(FAKE_ID, FAKE_SECRET))
            .unwrap();
    let kept = issued.login.verifier.clone();
    logins.insert(issued.state.clone(), issued.login, now);
    (issued.state, kept)
}

fn callback(code: &str, state: &str) -> auth::Pasted {
    auth::parse_pasted(&format!(
        "http://localhost:51121/oauth-callback?code={code}&state={state}"
    ))
}

#[test]
fn authorize_url_never_carries_the_verifier() {
    let authorization =
        auth::authorize_with_entropy(&[9; 32], &[7; 32], client(FAKE_ID, FAKE_SECRET)).unwrap();
    let verifier = authorization.login.verifier.clone();
    assert_eq!(authorization.login.project, "", "begin binds no project");
    assert!(
        !authorization.url.contains(&verifier),
        "verifier in the URL"
    );
    let state = URL_SAFE_NO_PAD.decode(&authorization.state).unwrap();
    assert_eq!(state.len(), 32, "state is a 32-byte nonce");
    assert!(
        !state
            .windows(verifier.len())
            .any(|window| window == verifier.as_bytes()),
        "verifier inside the state"
    );
    let url = url::Url::parse(&authorization.url).unwrap();
    let query = form_pairs(url.query().unwrap());
    assert_eq!(query["state"], authorization.state);
    assert_eq!(query["code_challenge_method"], "S256");
    assert_eq!(query["redirect_uri"], auth::REDIRECT_URI);
    assert!(
        query.values().all(|value| !value.contains(&verifier)),
        "verifier in a query value"
    );
}

#[test]
fn challenge_is_the_s256_digest_of_the_verifier() {
    let authorization =
        auth::authorize_with_entropy(&[9; 32], &[7; 32], client(FAKE_ID, FAKE_SECRET)).unwrap();
    let url = url::Url::parse(&authorization.url).unwrap();
    let query = form_pairs(url.query().unwrap());
    let digest = <sha2::Sha256 as sha2::Digest>::digest(authorization.login.verifier.as_bytes());
    assert_eq!(query["code_challenge"], URL_SAFE_NO_PAD.encode(digest));
}

#[test]
fn mismatched_state_is_rejected_and_takes_nothing() {
    let now = Instant::now();
    let mut logins = auth::PendingLogins::default();
    let (state, verifier) = issue(&mut logins, 1, 2, now);
    assert_eq!(
        logins
            .redeem(&callback("code-1", "forged-state"), now)
            .err()
            .as_deref(),
        Some(auth::MISMATCH)
    );
    let without_state = auth::parse_pasted("http://localhost:51121/oauth-callback?code=code-1");
    assert_eq!(
        logins.redeem(&without_state, now).err().as_deref(),
        Some(auth::MISMATCH),
        "a callback without state matches nothing"
    );
    let login = logins
        .redeem(&callback("code-1", &state), now)
        .ok()
        .expect("the real login is still pending");
    assert_eq!(login.verifier, verifier);
}

#[test]
fn used_and_expired_logins_are_rejected() {
    let now = Instant::now();
    let mut logins = auth::PendingLogins::default();
    let (state, _) = issue(&mut logins, 1, 2, now);
    assert!(logins.redeem(&callback("code-1", &state), now).is_ok());
    assert_eq!(
        logins
            .redeem(&callback("code-1", &state), now)
            .err()
            .as_deref(),
        Some(auth::MISMATCH),
        "single-use"
    );

    let (state, _) = issue(&mut logins, 3, 4, now);
    let later = now + auth::PENDING_TTL;
    assert_eq!(
        logins
            .redeem(&callback("code-2", &state), later)
            .err()
            .as_deref(),
        Some(auth::MISMATCH),
        "expires after the TTL"
    );
    issue(&mut logins, 5, 6, now);
    assert_eq!(
        logins
            .redeem(&auth::Pasted::Bare("code-3".into()), later)
            .err()
            .as_deref(),
        Some(auth::NO_LOGIN),
        "a bare code sees no expired login"
    );
}

#[test]
fn concurrent_begins_keep_separate_verifiers() {
    let now = Instant::now();
    let mut logins = auth::PendingLogins::default();
    let (first_state, first_verifier) = issue(&mut logins, 1, 11, now);
    let (second_state, second_verifier) = issue(&mut logins, 2, 12, now);
    assert_ne!(first_verifier, second_verifier);
    let second = logins
        .redeem(&callback("code", &second_state), now)
        .ok()
        .expect("second login");
    assert_eq!(second.verifier, second_verifier);
    let first = logins
        .redeem(&callback("code", &first_state), now)
        .ok()
        .expect("first login");
    assert_eq!(first.verifier, first_verifier);
}

#[test]
fn bare_code_redeems_the_most_recent_live_login() {
    let now = Instant::now();
    let mut logins = auth::PendingLogins::default();
    let (_, older) = issue(&mut logins, 1, 2, now);
    let (_, newer) = issue(&mut logins, 3, 4, now);
    let bare = auth::parse_pasted("  4/0AbC-code  ");
    assert!(matches!(&bare, auth::Pasted::Bare(code) if code == "4/0AbC-code"));
    assert_eq!(
        logins.redeem(&bare, now).ok().expect("newest").verifier,
        newer
    );
    assert_eq!(
        logins
            .redeem(&bare, now)
            .ok()
            .expect("then the older one")
            .verifier,
        older
    );
    assert_eq!(
        logins.redeem(&bare, now).err().as_deref(),
        Some(auth::NO_LOGIN)
    );
}

#[test]
fn pending_table_is_bounded_and_evicts_the_oldest() {
    let now = Instant::now();
    let mut logins = auth::PendingLogins::default();
    let issued: Vec<_> = (0..=auth::PENDING_LIMIT as u8)
        .map(|index| issue(&mut logins, index + 1, index + 50, now))
        .collect();
    let (oldest, _) = &issued[0];
    assert_eq!(
        logins
            .redeem(&callback("code", oldest), now)
            .err()
            .as_deref(),
        Some(auth::MISMATCH),
        "the oldest entry is evicted"
    );
    for (state, verifier) in &issued[1..] {
        let login = logins
            .redeem(&callback("code", state), now)
            .ok()
            .expect("kept within the limit");
        assert_eq!(&login.verifier, verifier);
    }
}

/// A port that counts the requests it is asked to send and answers none of them.
#[derive(Default)]
struct Silent(AtomicUsize);
struct Unread;
impl auth::Body for Unread {
    fn text(self) -> impl Future<Output = Result<String, String>> + Send {
        async { Err("unread".into()) }
    }
}
impl auth::Port for Silent {
    type Body = Unread;
    fn send(
        &self,
        _request: auth::Request,
    ) -> impl Future<Output = Result<auth::Reply<Unread>, String>> + Send {
        self.0.fetch_add(1, Ordering::SeqCst);
        async { Err("no network in this test".into()) }
    }
    fn now_ms(&self) -> f64 {
        0.0
    }
    fn user_agent(&self) -> String {
        "silent".into()
    }
}

#[tokio::test]
async fn rejected_or_replayed_inputs_never_reach_the_network() {
    let now = Instant::now();
    let logins = Mutex::new(auth::PendingLogins::default());
    let (state, _) = issue(&mut logins.lock().unwrap(), 1, 2, now);
    let port = Silent::default();
    let endpoints = Endpoints::production();
    let source = || client(FAKE_ID, FAKE_SECRET);

    let forged = "http://localhost:51121/oauth-callback?code=code-1&state=forged-state";
    let error = auth::complete(&port, source(), &endpoints, &logins, forged, now)
        .await
        .err()
        .unwrap();
    assert_eq!(error, auth::MISMATCH);
    let error = auth::complete(&port, source(), &endpoints, &logins, "  ", now)
        .await
        .err()
        .unwrap();
    assert_eq!(error, "Antigravity OAuth input is empty");
    assert_eq!(
        port.0.load(Ordering::SeqCst),
        0,
        "rejected inputs sent nothing"
    );

    // The matching callback redeems the login and its exchange fails, which still uses it up.
    let real = format!("http://localhost:51121/oauth-callback?code=code-1&state={state}");
    let value = auth::complete(&port, source(), &endpoints, &logins, &real, now)
        .await
        .unwrap();
    assert_eq!(value["type"], "failed");
    assert_eq!(port.0.load(Ordering::SeqCst), 1, "one exchange attempt");
    let error = auth::complete(&port, source(), &endpoints, &logins, &real, now)
        .await
        .err()
        .unwrap();
    assert_eq!(error, auth::MISMATCH, "a failed exchange is not retryable");
    assert_eq!(port.0.load(Ordering::SeqCst), 1, "the replay sent nothing");
}

#[test]
fn refresh_packing_follows_pi() {
    let parts = auth::parse_refresh("refresh-token|proj|managed");
    assert_eq!(parts.refresh_token, "refresh-token");
    assert_eq!(parts.project_id.as_deref(), Some("proj"));
    assert_eq!(parts.managed_project_id.as_deref(), Some("managed"));
    assert_eq!(auth::format_refresh(&parts), "refresh-token|proj|managed");
    assert_eq!(
        auth::format_refresh(&auth::parse_refresh("bare")),
        "bare|",
        "no project segment keeps Pi's separator"
    );
    assert_eq!(auth::expires_at(1_000.0, &json!(3600)), 3_601_000.0);
    assert_eq!(auth::expires_at(1_000.0, &json!(0)), 1_000.0);
}

#[test]
fn oauth_client_resolution_fails_closed_and_prefers_build_values() {
    let error = auth::authorize_with_entropy(&[1; 32], &[2; 32], unconfigured())
        .err()
        .unwrap();
    assert_eq!(
        error,
        "Antigravity OAuth is not configured: set RPI_ANTIGRAVITY_OAUTH_CLIENT_ID and RPI_ANTIGRAVITY_OAUTH_CLIENT_SECRET when building rpi-providers, or export both in the runtime environment."
    );
    let blank = ClientSource {
        build: BuildClient::NONE,
        runtime: |_| Some(" \t".into()),
    };
    assert!(blank.resolve().is_err(), "blank values count as unset");
    let runtime = ClientSource {
        build: BuildClient::NONE,
        runtime: |name| match name {
            auth::CLIENT_ID_ENV => Some("runtime-id.invalid".into()),
            _ => Some("runtime-secret".into()),
        },
    };
    assert!(
        runtime.resolve().is_ok(),
        "runtime values complete the client"
    );
    let mixed = ClientSource {
        build: BuildClient {
            id: Some("build-id.invalid"),
            secret: None,
        },
        runtime: |name| (name == auth::CLIENT_SECRET_ENV).then(|| "runtime-secret".into()),
    };
    let resolved = mixed.resolve().unwrap();
    assert_eq!(resolved.id(), "build-id.invalid", "build-time value wins");
}

/// Loopback endpoints: each `(path, status, body)` route answers once, in any order.
async fn token_server(
    routes: Vec<(&'static str, u16, &'static str)>,
) -> (Endpoints, tokio::task::JoinHandle<Vec<(String, String)>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let endpoints = Endpoints {
        token: format!("{base}/token"),
        userinfo: format!("{base}/userinfo"),
        revoke: format!("{base}/revoke"),
        load: vec![base],
    };
    let handle = tokio::spawn(async move {
        let mut seen = Vec::new();
        for (path, status, body) in routes {
            let (mut socket, _) = listener.accept().await.unwrap();
            let (request_path, request_body) = read_post(&mut socket).await;
            assert!(request_path.ends_with(path), "{request_path} vs {path}");
            seen.push((request_path, request_body));
            let head = format!(
                "HTTP/1.1 {status} Test\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            socket.write_all(head.as_bytes()).await.unwrap();
            socket.write_all(body.as_bytes()).await.unwrap();
        }
        seen
    });
    (endpoints, handle)
}

async fn read_post(socket: &mut TcpStream) -> (String, String) {
    let mut bytes = Vec::new();
    let end = loop {
        let mut buffer = [0; 4096];
        let count = socket.read(&mut buffer).await.unwrap();
        assert_ne!(count, 0);
        bytes.extend_from_slice(&buffer[..count]);
        if let Some(end) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
            break end;
        }
    };
    let head = String::from_utf8_lossy(&bytes[..end]).to_string();
    let length: usize = head
        .lines()
        .find_map(|line| {
            let lower = line.to_ascii_lowercase();
            lower
                .strip_prefix("content-length:")
                .map(|v| v.trim().parse().unwrap())
        })
        .unwrap_or(0);
    while bytes.len() < end + 4 + length {
        let mut buffer = [0; 4096];
        let count = socket.read(&mut buffer).await.unwrap();
        bytes.extend_from_slice(&buffer[..count]);
    }
    let path = head
        .lines()
        .next()
        .unwrap()
        .split_whitespace()
        .nth(1)
        .unwrap()
        .to_owned();
    let body = String::from_utf8_lossy(&bytes[end + 4..end + 4 + length]).to_string();
    (path, body)
}

async fn respond(socket: &mut TcpStream, status: u16, body: &str) {
    let head = format!(
        "HTTP/1.1 {status} Test\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    socket.write_all(head.as_bytes()).await.unwrap();
    socket.write_all(body.as_bytes()).await.unwrap();
}

/// The provider called directly, without the host's FFI bridge (which adds its own abort).
fn direct() -> super::AntigravityProvider {
    super::AntigravityProvider {
        client: reqwest::Client::new(),
        models: super::models(),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn direct_cancellation_reports_aborted_and_keeps_partial_text() {
    use rpi_ai::{
        AssistantMessageEvent, Context, Message, Provider, SimpleStreamOptions, StopReason,
        UserMessage,
    };
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            let (path, _) = read_post(&mut socket).await;
            if path.ends_with(":loadCodeAssist") {
                respond(
                    &mut socket,
                    200,
                    r#"{"cloudaicompanionProject":"project-direct"}"#,
                )
                .await;
                continue;
            }
            socket
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n")
                .await
                .unwrap();
            let data =
                "data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"partial\"}]}}]}\n\n";
            socket
                .write_all(format!("{:x}\r\n{data}\r\n", data.len()).as_bytes())
                .await
                .unwrap();
            // Hold the response until the client drops it.
            let mut buffer = [0; 64];
            let _ = socket.read(&mut buffer).await;
            break;
        }
    });
    let mut model = super::models()[1].clone();
    model.base_url = base;
    let options = SimpleStreamOptions {
        api_key: Some("access-direct".into()),
        ..Default::default()
    };
    let signal = options.signal.clone();
    let context = Context::new(vec![Message::User(UserMessage::new("hi", 0))]);
    let mut stream = direct().stream_simple(&model, &context, &options).await;
    let mut terminal = None;
    let mut terminals = 0;
    while let Some(event) = stream.next().await {
        if matches!(&event, AssistantMessageEvent::TextDelta { delta, .. } if delta == "partial") {
            signal.cancel();
        }
        if event.is_terminal() {
            terminals += 1;
            terminal = Some(event);
        }
    }
    tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(terminals, 1, "exactly one terminal");
    let Some(AssistantMessageEvent::Error { reason, error }) = terminal else {
        panic!("expected an aborted terminal");
    };
    assert_eq!(reason, rpi_ai::ErrorReason::Aborted);
    assert_eq!(error.stop_reason, StopReason::Aborted);
    let text: String = error
        .content
        .iter()
        .filter_map(|content| match content {
            rpi_ai::Content::Text(text) => Some(text.text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(text, "partial");
}

fn net(endpoints: Endpoints) -> transport::Net {
    transport::Net {
        client: reqwest::Client::new(),
        endpoints,
        user_agent: "antigravity-test".into(),
    }
}

#[tokio::test]
async fn refresh_packs_the_credential_with_expiry_in_seconds() {
    let (endpoints, server) = token_server(vec![(
        "/token",
        200,
        r#"{"access_token":"new-access","refresh_token":"rotated","expires_in":3600}"#,
    )])
    .await;
    let port = net(endpoints);
    let before = transport::now_ms();
    let refreshed = auth::refresh(
        &port,
        client(FAKE_ID, FAKE_SECRET),
        &port.endpoints,
        "old|proj|managed",
    )
    .await
    .unwrap();
    let seen = server.await.unwrap();
    let form = form_pairs(&seen[0].1);
    assert_eq!(form["refresh_token"], "old");
    assert_eq!(form["client_id"], FAKE_ID);
    assert_eq!(form["client_secret"], FAKE_SECRET);
    assert_eq!(form["grant_type"], "refresh_token");
    assert_eq!(refreshed["access"], "new-access");
    assert_eq!(
        refreshed["refresh"], "rotated|proj|managed",
        "project segments kept"
    );

    // The host envelope carries whole seconds; Pi's expiry was milliseconds.
    let expires_ms = refreshed["expires"].as_f64().unwrap();
    assert!(expires_ms >= (before + 3_600_000) as f64);
    let envelope =
        super::credential_envelope("new-access", "rotated|proj|managed", &refreshed["expires"]);
    let expires_at = envelope["payload"]["expires_at"].as_i64().unwrap();
    assert_eq!(expires_at, (expires_ms / 1000.0) as i64);
    assert!(
        (expires_at - before / 1000 - 3600).abs() <= 2,
        "seconds, not milliseconds"
    );
}

#[tokio::test]
async fn exchange_returns_the_packed_credential_and_stays_in_the_project() {
    let (endpoints, server) = token_server(vec![
        (
            "/token",
            200,
            r#"{"access_token":"login-access","refresh_token":"login-refresh","expires_in":60}"#,
        ),
        ("/userinfo", 200, r#"{"email":"user@example.invalid"}"#),
    ])
    .await;
    let port = net(endpoints);
    let login = auth::PendingLogin {
        verifier: "verifier-7".into(),
        project: "proj-7".into(),
    };
    let result = auth::exchange(
        &port,
        client(FAKE_ID, FAKE_SECRET),
        &port.endpoints,
        "code-1",
        &login,
    )
    .await;
    let seen = server.await.unwrap();
    assert_eq!(
        seen.len(),
        2,
        "no project discovery when the login carries one"
    );
    let form = form_pairs(&seen[0].1);
    assert_eq!(form["code"], "code-1");
    assert_eq!(form["grant_type"], "authorization_code");
    assert_eq!(form["code_verifier"], "verifier-7");
    assert_eq!(result["type"], "success");
    assert_eq!(result["refresh"], "login-refresh|proj-7");
    assert_eq!(result["access"], "login-access");
    assert_eq!(result["projectId"], "proj-7");
}

#[tokio::test]
async fn revoke_sends_the_refresh_token_and_reports_failures_without_it() {
    let (endpoints, server) = token_server(vec![("/revoke", 200, "")]).await;
    let port = net(endpoints);
    auth::revoke(&port, &port.endpoints, "keep-me|proj", "access-1")
        .await
        .unwrap();
    let seen = server.await.unwrap();
    assert_eq!(form_pairs(&seen[0].1)["token"], "keep-me");

    let (endpoints, server) = token_server(vec![("/revoke", 400, "keep-me echoed")]).await;
    let port = net(endpoints);
    let error = auth::revoke(&port, &port.endpoints, "keep-me|proj", "access-1")
        .await
        .unwrap_err();
    server.await.unwrap();
    assert!(error.contains("400"), "{error}");
    assert!(
        !error.contains("keep-me"),
        "response body is not echoed: {error}"
    );
}

#[tokio::test]
async fn refresh_failure_reports_status_and_never_the_client_secret() {
    let (endpoints, server) = token_server(vec![(
        "/token",
        400,
        r#"{"error":"invalid_grant","client_secret":"fake-secret-value"}"#,
    )])
    .await;
    let port = net(endpoints);
    let error = auth::refresh(&port, client(FAKE_ID, FAKE_SECRET), &port.endpoints, "old")
        .await
        .err()
        .unwrap();
    server.await.unwrap();
    assert!(error.contains("400"), "{error}");
    assert!(!error.contains(FAKE_SECRET), "{error}");
    assert!(!error.contains("old"), "{error}");
}

#[test]
fn credential_bridge_is_keyed_per_access_token_and_bounded() {
    let mut cache = auth::CredentialCache::default();
    cache.remember("access-a", "refresh-a|pa");
    cache.remember("access-b", "refresh-b|pb");
    assert_eq!(cache.get("access-a"), Some("refresh-a|pa"));
    assert_eq!(cache.get("access-b"), Some("refresh-b|pb"));
    assert_eq!(cache.get("access-c"), None, "unknown tokens never match");
    for index in 0..4 {
        cache.remember(&format!("filler-{index}"), "r");
    }
    assert_eq!(cache.get("access-a"), None, "oldest entry evicted at four");
    cache.forget("filler-3");
    assert_eq!(cache.get("filler-3"), None);
    cache.remember("", "ignored");
    assert_eq!(cache.get(""), None);
}

#[test]
fn redaction_masks_credentials_projects_and_bearer_values() {
    let text = redaction::text(
        "Bearer abc.def-ghi and token-secret-value in project project-long-name",
        &["token-secret-value".into()],
        &["project-long-name".into()],
    );
    assert!(!text.contains("token-secret-value"), "{text}");
    assert!(!text.contains("project-long-name"), "{text}");
    assert!(!text.contains("abc.def-ghi"), "{text}");
    assert!(text.contains("[credential]"), "{text}");
}

#[test]
fn generated_identifiers_are_version_four_uuids() {
    let id = transport::uuid_v4();
    assert_eq!(id.len(), 36);
    assert_eq!(&id[14..15], "4");
    assert!(matches!(&id[19..20], "8" | "9" | "a" | "b"));
    assert_ne!(id, transport::uuid_v4());
}

#[test]
fn default_user_agent_matches_the_cli_identity_shape() {
    let agent = transport::default_user_agent();
    assert!(agent.starts_with("antigravity/cli/1.3.5 (aidev_client; os_type="));
    assert!(agent.ends_with("; auth_method=consumer)"));
}
