//! Antigravity through the real cdylib: loading, OAuth begin, Gemini SSE streaming,
//! cancellation, HTTP errors, lifecycle shutdown, credential isolation and the project
//! binding across a restart.
//! Token and project endpoints are unit-tested inside the module; this file never reaches
//! Google. Every loopback server here answers project discovery itself.
use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use rpi_ai::{
    AssistantMessageEvent, Content, Context, Message, Model, Provider, SimpleStreamOptions,
    StopReason, UserMessage,
};
use rpi_providers::antigravity::{models, PROVIDER_ID};
use serde_json::{json, Value};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::oneshot,
};
use url::Url;

const CLIENT_ID_ENV: &str = "RPI_ANTIGRAVITY_OAUTH_CLIENT_ID";
const CLIENT_SECRET_ENV: &str = "RPI_ANTIGRAVITY_OAUTH_CLIENT_SECRET";
const STREAM_PATH: &str = "/v1internal:streamGenerateContent?alt=sse";
const RETIRED_MESSAGE: &str = "This Antigravity model is no longer offered. Choose another model.";

fn library_path() -> PathBuf {
    let profile = std::env::current_exe()
        .unwrap()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf();
    let filename = format!(
        "{}rpi_providers{}",
        std::env::consts::DLL_PREFIX,
        std::env::consts::DLL_SUFFIX
    );
    [profile.join(&filename), profile.join("deps").join(filename)]
        .into_iter()
        .find(|path| path.is_file())
        .expect("first cargo build -p rpi-providers")
}

fn load() -> (rpi_extensions::ExtensionSession, Vec<Arc<dyn Provider>>) {
    load_from(library_path())
}

fn load_from(path: PathBuf) -> (rpi_extensions::ExtensionSession, Vec<Arc<dyn Provider>>) {
    let session = rpi_extensions::load_session_mixed(
        &[],
        &[path],
        Arc::new(rpi_extensions::NullDiagnostics),
        None,
    );
    let providers = rpi_extensions::PluggableProvider::from_session(
        &session,
        tokio::runtime::Handle::current(),
    );
    (session, providers)
}

/// A byte-identical copy of the library at a new path. Loading it gives a separate image with
/// its own statics, so a restart starts with an empty credential bridge and project cache.
fn restarted_library(dir: &Path) -> PathBuf {
    let copy = dir.join(library_path().file_name().unwrap());
    std::fs::copy(library_path(), &copy).expect("copy the library for a restart");
    copy
}

fn antigravity(providers: &[Arc<dyn Provider>]) -> Arc<dyn Provider> {
    providers
        .iter()
        .find(|provider| provider.id() == PROVIDER_ID)
        .cloned()
        .expect("antigravity loaded")
}

/// A catalog model pointed at a loopback base URL.
fn model(base: &str) -> Model {
    let mut model = models()
        .into_iter()
        .find(|model| model.id == "antigravity-gemini-3.8-flash")
        .unwrap();
    model.base_url = base.into();
    model
}

fn options(token: &str) -> SimpleStreamOptions {
    SimpleStreamOptions {
        api_key: Some(token.into()),
        timeout: Some(Duration::from_secs(10)),
        ..Default::default()
    }
}

fn context() -> Context {
    Context::new(vec![Message::User(UserMessage::new("hello", 0))])
}

#[derive(Debug, Default)]
struct Seen {
    path: String,
    authorization: String,
    body: Value,
}

async fn read_request(socket: &mut TcpStream) -> Seen {
    let mut bytes = Vec::new();
    let head_end = loop {
        let mut buffer = [0; 4096];
        let count = socket.read(&mut buffer).await.unwrap();
        assert_ne!(count, 0, "client closed before the request head");
        bytes.extend_from_slice(&buffer[..count]);
        if let Some(end) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
            break end;
        }
    };
    let head = String::from_utf8_lossy(&bytes[..head_end]).to_string();
    let length: usize = head
        .lines()
        .find_map(|line| {
            let lower = line.to_ascii_lowercase();
            lower
                .strip_prefix("content-length:")
                .map(|value| value.trim().parse().unwrap())
        })
        .unwrap_or(0);
    while bytes.len() < head_end + 4 + length {
        let mut buffer = [0; 4096];
        let count = socket.read(&mut buffer).await.unwrap();
        assert_ne!(count, 0, "client closed before the request body");
        bytes.extend_from_slice(&buffer[..count]);
    }
    let body = if length == 0 {
        Value::Null
    } else {
        serde_json::from_slice(&bytes[head_end + 4..head_end + 4 + length]).unwrap()
    };
    let path = head
        .lines()
        .next()
        .unwrap()
        .split_whitespace()
        .nth(1)
        .unwrap()
        .to_owned();
    let authorization = head
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("authorization")
                .then(|| value.trim().to_owned())
        })
        .unwrap_or_default();
    Seen {
        path,
        authorization,
        body,
    }
}

/// The project a token's loopback account resolves to.
fn project_for(authorization: &str) -> String {
    format!(
        "project-{}",
        authorization.trim_start_matches("Bearer ").trim()
    )
}

async fn respond_json(socket: &mut TcpStream, status: u16, body: &str) {
    let head = format!(
        "HTTP/1.1 {status} Test\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    socket.write_all(head.as_bytes()).await.unwrap();
    socket.write_all(body.as_bytes()).await.unwrap();
}

async fn start_stream(socket: &mut TcpStream) {
    socket
        .write_all(
            b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n",
        )
        .await
        .unwrap();
}

async fn send_chunk(socket: &mut TcpStream, data: &str) {
    let wire = format!("{:x}\r\n{data}\r\n", data.len());
    socket.write_all(wire.as_bytes()).await.unwrap();
}

async fn finish_chunks(socket: &mut TcpStream) {
    socket.write_all(b"0\r\n\r\n").await.unwrap();
}

fn event(value: Value) -> String {
    format!("data: {value}\n\n")
}

/// Answers project discovery for each connection and returns the first streaming request.
async fn next_stream(listener: &TcpListener) -> (TcpStream, Seen) {
    loop {
        let (mut socket, _) = listener.accept().await.unwrap();
        let seen = read_request(&mut socket).await;
        if seen.path.ends_with(":loadCodeAssist") {
            let body = json!({"cloudaicompanionProject": project_for(&seen.authorization)});
            respond_json(&mut socket, 200, &body.to_string()).await;
            continue;
        }
        return (socket, seen);
    }
}

/// Answers each project-discovery or onboarding call with `discovery`, or refuses it with 503
/// when `None`, then streams one reply to the first streaming request. Returns that request
/// and how many discovery calls arrived before it.
async fn serve_stream(listener: &TcpListener, discovery: Option<&str>) -> (Seen, usize) {
    let mut calls = 0;
    loop {
        let (mut socket, _) = listener.accept().await.unwrap();
        let seen = read_request(&mut socket).await;
        if seen.path.ends_with(":loadCodeAssist") || seen.path.ends_with(":onboardUser") {
            calls += 1;
            match discovery {
                Some(project) => {
                    let body = json!({"cloudaicompanionProject": project});
                    respond_json(&mut socket, 200, &body.to_string()).await;
                }
                None => respond_json(&mut socket, 503, r#"{"error":"discovery is down"}"#).await,
            }
            continue;
        }
        assert_eq!(seen.path, STREAM_PATH);
        start_stream(&mut socket).await;
        send_chunk(
            &mut socket,
            &event(
                json!({"candidates":[{"content":{"parts":[{"text":"ok"}]},"finishReason":"STOP"}]}),
            ),
        )
        .await;
        finish_chunks(&mut socket).await;
        return (seen, calls);
    }
}

fn kind(event: &AssistantMessageEvent) -> &'static str {
    match event {
        AssistantMessageEvent::Start { .. } => "start",
        AssistantMessageEvent::TextStart { .. } => "text_start",
        AssistantMessageEvent::TextDelta { .. } => "text_delta",
        AssistantMessageEvent::TextEnd { .. } => "text_end",
        AssistantMessageEvent::ThinkingStart { .. } => "thinking_start",
        AssistantMessageEvent::ThinkingDelta { .. } => "thinking_delta",
        AssistantMessageEvent::ThinkingEnd { .. } => "thinking_end",
        AssistantMessageEvent::ToolCallStart { .. } => "toolcall_start",
        AssistantMessageEvent::ToolCallDelta { .. } => "toolcall_delta",
        AssistantMessageEvent::ToolCallEnd { .. } => "toolcall_end",
        AssistantMessageEvent::Done { .. } => "done",
        AssistantMessageEvent::Error { .. } => "error",
    }
}

/// Drains a stream, returning every event and the number of terminal events seen.
async fn drain(
    stream: &mut rpi_ai::AssistantMessageEventStream,
    mut on_event: impl FnMut(&AssistantMessageEvent),
) -> (Vec<AssistantMessageEvent>, usize) {
    let mut events = Vec::new();
    let mut terminals = 0;
    while let Some(event) = stream.next().await {
        on_event(&event);
        if event.is_terminal() {
            terminals += 1;
        }
        events.push(event);
    }
    (events, terminals)
}

fn text_of(content: &[Content]) -> String {
    content
        .iter()
        .filter_map(|content| match content {
            Content::Text(text) => Some(text.text.as_str()),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn catalog_and_registrations_load_beside_the_other_providers() {
    let (session, providers) = load();
    assert_eq!(providers.len(), 5);
    let snapshot = session.snapshot().expect("bundle loaded");
    assert_eq!(snapshot.providers().len(), 5);

    let manifest = snapshot
        .provider_manifest(PROVIDER_ID)
        .expect("antigravity manifest");
    assert_eq!(
        manifest.api_key_env, None,
        "OAuth only: no API-key variable"
    );
    assert!(manifest.requires_auth);
    let ids: Vec<_> = manifest
        .models
        .iter()
        .map(|model| model.id.as_str())
        .collect();
    assert_eq!(
        ids,
        [
            "antigravity-gemini-4-argon",
            "antigravity-gemini-3.8-flash",
            "antigravity-gemini-3.5-flash",
            "antigravity-claude-sonnet-5-5",
            "antigravity-claude-opus-5-5",
            "antigravity-gpt-oss-120b-medium",
        ]
    );
    assert!(manifest
        .models
        .iter()
        .all(|model| model.provider == PROVIDER_ID && model.headers.is_none()));

    assert!(snapshot
        .providers()
        .iter()
        .any(|provider| provider.provider_id == PROVIDER_ID && provider.stream_fn.is_some()));
    let oauth = rpi_extensions::list_oauth_providers(&snapshot);
    assert!(oauth.iter().any(|provider| provider.id == PROVIDER_ID));
    assert_eq!(antigravity(&providers).models().len(), 6);
}

#[tokio::test]
async fn oauth_begin_requires_configuration_then_issues_an_s256_pkce_url() {
    let (session, providers) = load();
    let snapshot = session.snapshot_arc().expect("bundle loaded");

    // Unconfigured: begin fails with a configuration error and every other provider still loads.
    std::env::remove_var(CLIENT_ID_ENV);
    std::env::remove_var(CLIENT_SECRET_ENV);
    let error = rpi_extensions::request_oauth(&snapshot, PROVIDER_ID, "begin", json!({}))
        .unwrap_err()
        .to_string();
    assert!(error.contains("not configured"), "{error}");
    assert!(error.contains(CLIENT_ID_ENV) && error.contains(CLIENT_SECRET_ENV));
    assert_eq!(providers.len(), 5);

    // Configured from the runtime environment with obviously fake values.
    std::env::set_var(CLIENT_ID_ENV, "fake-client.invalid");
    std::env::set_var(CLIENT_SECRET_ENV, "fake-secret");
    let response = rpi_extensions::request_oauth(&snapshot, PROVIDER_ID, "begin", json!({}))
        .expect("begin succeeds once configured");
    std::env::remove_var(CLIENT_ID_ENV);
    std::env::remove_var(CLIENT_SECRET_ENV);
    assert_eq!(response["kind"], "interaction");

    let login = Url::parse(response["payload"]["display"]["url"].as_str().unwrap()).unwrap();
    assert_eq!(login.host_str(), Some("accounts.google.com"));
    let query: std::collections::HashMap<_, _> = login.query_pairs().into_owned().collect();
    assert_eq!(query["client_id"], "fake-client.invalid");
    assert_eq!(query["response_type"], "code");
    assert_eq!(query["code_challenge_method"], "S256");
    assert_eq!(
        query["redirect_uri"],
        "http://localhost:51121/oauth-callback"
    );
    assert_eq!(query["access_type"], "offline");
    // The state is a 32-byte nonce; the verifier never leaves the process, so the S256
    // digest is checked against it in the module's unit tests.
    assert_eq!(URL_SAFE_NO_PAD.decode(&query["state"]).unwrap().len(), 32);
    assert_eq!(query["code_challenge"].len(), 43, "S256 digest, base64url");
}

/// The access value a login stores for a project-bound account, `<token>|<project>`. The
/// module's unit tests pin how the login produces it.
const BOUND_ACCESS: &str = "restart-token|bound-project";

/// A project bound at login survives a restart. After the restart the stored access supplies
/// the project: the stream makes no discovery call, even while discovery is refusing, and only
/// the plain token reaches Google.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn restart_reuses_the_project_bound_at_login() {
    let dir = tempfile::tempdir().unwrap();
    let (_session, providers) = load_from(restarted_library(dir.path()));
    let cloud = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", cloud.local_addr().unwrap());
    let server = tokio::spawn(async move { serve_stream(&cloud, None).await });
    let mut stream = antigravity(&providers)
        .stream_simple(&model(&base), &context(), &options(BOUND_ACCESS))
        .await;
    let (events, terminals) = drain(&mut stream, |_| {}).await;
    let (seen, discovery_calls) = server.await.unwrap();

    assert_eq!(terminals, 1);
    assert!(
        matches!(events.last(), Some(AssistantMessageEvent::Done { .. })),
        "{events:?}"
    );
    assert_eq!(
        seen.body["project"], "bound-project",
        "the bound project, not the default project"
    );
    assert_eq!(discovery_calls, 0, "no project discovery after the restart");
    assert_eq!(
        seen.authorization, "Bearer restart-token",
        "only the plain token reaches Google"
    );
}

/// A credential stored before the binding existed has a plain access token. After a restart
/// it keeps the fallback: project discovery decides the project.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn legacy_access_without_a_binding_still_uses_discovery_after_restart() {
    let dir = tempfile::tempdir().unwrap();
    let (_session, providers) = load_from(restarted_library(dir.path()));
    let cloud = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", cloud.local_addr().unwrap());
    let server =
        tokio::spawn(async move { serve_stream(&cloud, Some("discovered-project")).await });
    let mut stream = antigravity(&providers)
        .stream_simple(&model(&base), &context(), &options("legacy-token"))
        .await;
    let (events, terminals) = drain(&mut stream, |_| {}).await;
    let (seen, discovery_calls) = server.await.unwrap();

    assert_eq!(terminals, 1);
    assert!(
        matches!(events.last(), Some(AssistantMessageEvent::Done { .. })),
        "{events:?}"
    );
    assert_eq!(
        discovery_calls, 1,
        "a plain token still discovers its project"
    );
    assert_eq!(seen.body["project"], "discovered-project");
    assert_eq!(seen.authorization, "Bearer legacy-token");
}

#[tokio::test]
async fn retired_model_is_refused_before_any_network_call() {
    let (_session, providers) = load();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let mut retired = model(&base);
    retired.id = "antigravity-gemini-3.7-flash".into();
    let mut stream = antigravity(&providers)
        .stream_simple(&retired, &context(), &options("access-retired"))
        .await;
    let (events, terminals) = drain(&mut stream, |_| {}).await;
    let AssistantMessageEvent::Error { error, .. } = events.last().unwrap() else {
        panic!("expected an error terminal: {events:?}");
    };
    assert_eq!(error.error_message.as_deref(), Some(RETIRED_MESSAGE));
    assert_eq!(terminals, 1);
    assert!(
        tokio::time::timeout(Duration::from_millis(300), listener.accept())
            .await
            .is_err(),
        "no connection may be opened for a retired model"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn streams_thinking_text_and_tool_calls_incrementally() {
    let (_session, providers) = load();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let (release_tx, release_rx) = oneshot::channel::<()>();
    let server = tokio::spawn(async move {
        let (mut socket, seen) = next_stream(&listener).await;
        assert_eq!(seen.path, STREAM_PATH);
        assert_eq!(seen.authorization, "Bearer access-stream");
        start_stream(&mut socket).await;
        send_chunk(
            &mut socket,
            &event(
                json!({"candidates":[{"content":{"parts":[{"text":"ponder","thought":true}]}}]}),
            ),
        )
        .await;
        send_chunk(
            &mut socket,
            &event(json!({"candidates":[{"content":{"parts":[{"text":"Hel"}]}}]})),
        )
        .await;
        // The rest arrives only after the client has seen the first text delta.
        release_rx.await.unwrap();
        for data in [
            json!({"candidates":[{"content":{"parts":[{"text":"lo"}]}}]}),
            json!({"candidates":[{"content":{"parts":[{"functionCall":{"id":"call_1","name":"lookup","args":{"q":"x"}}}]}}]}),
            json!({"candidates":[{"finishReason":"STOP"}],"usageMetadata":{"promptTokenCount":12,"candidatesTokenCount":3}}),
        ] {
            send_chunk(&mut socket, &event(data)).await;
        }
        finish_chunks(&mut socket).await;
    });

    let mut stream = antigravity(&providers)
        .stream_simple(&model(&base), &context(), &options("access-stream"))
        .await;
    let mut release = Some(release_tx);
    let (events, terminals) = drain(&mut stream, |event| {
        if let AssistantMessageEvent::TextDelta { delta, .. } = event {
            if delta == "Hel" {
                if let Some(release) = release.take() {
                    release.send(()).unwrap();
                }
            }
        }
    })
    .await;
    server.await.unwrap();

    let kinds: Vec<_> = events.iter().map(kind).collect();
    assert_eq!(
        kinds,
        [
            "start",
            "thinking_start",
            "thinking_delta",
            "thinking_end",
            "text_start",
            "text_delta",
            "text_delta",
            "text_end",
            "toolcall_start",
            "toolcall_end",
            "done",
        ]
    );
    assert_eq!(terminals, 1, "exactly one terminal event");
    let AssistantMessageEvent::ToolCallEnd { tool_call, .. } = &events[9] else {
        panic!("tool call end: {:?}", events[9]);
    };
    assert_eq!(tool_call.name, "lookup");
    assert_eq!(tool_call.id, "call_1");
    assert_eq!(tool_call.arguments, json!({"q":"x"}));
    let AssistantMessageEvent::Done { reason, message } = events.last().unwrap() else {
        panic!("done");
    };
    assert_eq!(*reason, rpi_ai::DoneReason::ToolUse);
    assert_eq!(message.stop_reason, StopReason::ToolUse);
    assert_eq!(text_of(&message.content), "Hello");
    assert_eq!(message.usage.input, 12);
    assert_eq!(message.usage.output, 3);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancellation_mid_stream_keeps_partial_text_and_aborts() {
    let (_session, providers) = load();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let (mut socket, _) = next_stream(&listener).await;
        start_stream(&mut socket).await;
        send_chunk(
            &mut socket,
            &event(json!({"candidates":[{"content":{"parts":[{"text":"partial"}]}}]})),
        )
        .await;
        // Hold the response open until the client drops the connection.
        let mut buffer = [0; 64];
        let _ = socket.read(&mut buffer).await;
    });

    let options = options("access-cancel");
    let signal = options.signal.clone();
    let mut stream = antigravity(&providers)
        .stream_simple(&model(&base), &context(), &options)
        .await;
    let (events, terminals) = drain(&mut stream, |event| {
        if matches!(event, AssistantMessageEvent::TextDelta { delta, .. } if delta == "partial") {
            signal.cancel();
        }
    })
    .await;
    tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .unwrap()
        .unwrap();

    assert_eq!(terminals, 1);
    let AssistantMessageEvent::Error { reason, error } = events.last().unwrap() else {
        panic!("expected an aborted terminal: {events:?}");
    };
    assert_eq!(*reason, rpi_ai::ErrorReason::Aborted);
    assert_eq!(error.stop_reason, StopReason::Aborted);
    assert_eq!(text_of(&error.content), "partial");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn http_errors_become_one_terminal_without_leaking_credentials() {
    let (_session, providers) = load();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        for (status, body) in [
            (
                401,
                r#"{"error":{"code":401,"message":"Request had invalid authentication credentials. Bearer access-denied-token project project-access-denied-token","status":"UNAUTHENTICATED"}}"#,
            ),
            (500, "upstream exploded for access-denied-token"),
        ] {
            let (mut socket, _) = next_stream(&listener).await;
            respond_json(&mut socket, status, body).await;
        }
    });

    for (token, expected_status) in [
        ("access-denied-token", "HTTP 401"),
        ("access-denied-token", "HTTP 500"),
    ] {
        let mut stream = antigravity(&providers)
            .stream_simple(&model(&base), &context(), &options(token))
            .await;
        let (events, terminals) = drain(&mut stream, |_| {}).await;
        assert_eq!(terminals, 1, "one terminal for an HTTP error");
        let AssistantMessageEvent::Error { reason, error } = events.last().unwrap() else {
            panic!("expected an error terminal: {events:?}");
        };
        assert_eq!(*reason, rpi_ai::ErrorReason::Error);
        assert_eq!(error.stop_reason, StopReason::Error);
        let message = error.error_message.as_deref().unwrap();
        assert!(message.contains(expected_status), "{message}");
        assert!(
            !message.contains("access-denied-token"),
            "token leaked: {message}"
        );
        assert!(
            !message.contains("project-access-denied-token"),
            "project leaked: {message}"
        );
    }
    server.await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shutdown_cancels_active_streams_and_rejects_new_ones() {
    let (session, providers) = load();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let (mut socket, _) = next_stream(&listener).await;
        start_stream(&mut socket).await;
        send_chunk(
            &mut socket,
            &event(json!({"candidates":[{"content":{"parts":[{"text":"partial"}]}}]})),
        )
        .await;
        let mut buffer = [0; 64];
        let _ = socket.read(&mut buffer).await;
    });

    let provider = antigravity(&providers);
    let mut active = provider
        .stream_simple(&model(&base), &context(), &options("access-shutdown"))
        .await;
    loop {
        match tokio::time::timeout(Duration::from_secs(5), active.next())
            .await
            .unwrap()
        {
            Some(AssistantMessageEvent::TextDelta { .. }) => break,
            Some(_) => continue,
            None => panic!("stream ended before the first text delta"),
        }
    }

    session.begin_shutdown();
    session.begin_shutdown();
    let (events, terminals) = drain(&mut active, |_| {}).await;
    assert_eq!(terminals, 1);
    let AssistantMessageEvent::Error { reason, error } = events.last().unwrap() else {
        panic!("expected an aborted terminal: {events:?}");
    };
    assert_eq!(*reason, rpi_ai::ErrorReason::Aborted, "{error:?}");
    server.await.unwrap();

    let mut refused = provider
        .stream_simple(&model(&base), &context(), &options("access-shutdown"))
        .await;
    let (events, terminals) = drain(&mut refused, |_| {}).await;
    assert_eq!(terminals, 1);
    let AssistantMessageEvent::Error { error, .. } = events.last().unwrap() else {
        panic!("expected a refusal: {events:?}");
    };
    assert!(
        error
            .error_message
            .as_deref()
            .unwrap()
            .contains("shutting down"),
        "{error:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_accounts_never_share_tokens_or_projects() {
    let (_session, providers) = load();
    let provider = antigravity(&providers);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let streams: Arc<Mutex<Vec<Seen>>> = Arc::new(Mutex::new(Vec::new()));
    let recorded = streams.clone();
    let server = tokio::spawn(async move {
        // Four connections in total: discovery and streaming for each of two accounts.
        let mut handlers = Vec::new();
        for _ in 0..4 {
            let (mut socket, _) = listener.accept().await.unwrap();
            let recorded = recorded.clone();
            handlers.push(tokio::spawn(async move {
                let seen = read_request(&mut socket).await;
                if seen.path.ends_with(":loadCodeAssist") {
                    let body = json!({"cloudaicompanionProject": project_for(&seen.authorization)});
                    respond_json(&mut socket, 200, &body.to_string()).await;
                    return;
                }
                let project = seen.body["project"].as_str().unwrap().to_owned();
                let text = project.clone();
                recorded.lock().unwrap().push(seen);
                start_stream(&mut socket).await;
                send_chunk(
                    &mut socket,
                    &event(json!({"candidates":[{"content":{"parts":[{"text": text}]},"finishReason":"STOP"}]})),
                )
                .await;
                finish_chunks(&mut socket).await;
            }));
        }
        for handler in handlers {
            handler.await.unwrap();
        }
    });

    let stream_for = |token: &'static str| {
        let provider = provider.clone();
        let base = base.clone();
        async move {
            let mut stream = provider
                .stream_simple(&model(&base), &context(), &options(token))
                .await;
            let (events, terminals) = drain(&mut stream, |_| {}).await;
            assert_eq!(terminals, 1);
            let AssistantMessageEvent::Done { message, .. } = events.last().unwrap() else {
                panic!("expected success: {events:?}");
            };
            (token, text_of(&message.content))
        }
    };
    let (first, second) = tokio::join!(stream_for("access-alpha"), stream_for("access-beta"));
    server.await.unwrap();

    assert_eq!(first.1, "project-access-alpha");
    assert_eq!(second.1, "project-access-beta");
    let streams = streams.lock().unwrap();
    assert_eq!(streams.len(), 2);
    for seen in streams.iter() {
        let token = seen.authorization.trim_start_matches("Bearer ");
        assert_eq!(seen.body["project"], format!("project-{token}"));
        assert!(
            seen.body["model"]
                .as_str()
                .unwrap()
                .starts_with("gemini-3.8-flash"),
            "wire model: {}",
            seen.body["model"]
        );
    }
}
