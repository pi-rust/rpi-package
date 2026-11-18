//! OpenAI Codex through the real cdylib: loading, OAuth begin, Responses SSE streaming,
//! cancellation, HTTP errors, lifecycle shutdown and credential isolation.
//! Every loopback server here answers the Codex responses endpoint itself; nothing reaches OpenAI.
use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use rpi_ai::{
    AssistantMessageEvent, Content, Context, Message, Model, Provider, SimpleStreamOptions,
    StopReason, UserMessage,
};
use rpi_providers::codex::{models, PROVIDER_ID};
use serde_json::{json, Value};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::oneshot,
};
use url::Url;

const STREAM_PATH: &str = "/codex/responses";

fn library_path() -> PathBuf {
    let filename = format!(
        "{}rpi_providers{}",
        std::env::consts::DLL_PREFIX,
        std::env::consts::DLL_SUFFIX
    );
    let executable = std::env::current_exe().unwrap();
    executable
        .ancestors()
        .flat_map(|directory| {
            [
                directory.join(&filename),
                directory.join("deps").join(&filename),
            ]
        })
        .find(|path| path.is_file())
        .expect("first cargo build -p rpi-providers")
}

fn load() -> (rpi_extensions::ExtensionSession, Vec<Arc<dyn Provider>>) {
    let session = rpi_extensions::load_session_mixed(
        &[],
        &[library_path()],
        Arc::new(rpi_extensions::NullDiagnostics),
        None,
    );
    let providers = rpi_extensions::PluggableProvider::from_session(
        &session,
        tokio::runtime::Handle::current(),
    );
    (session, providers)
}

fn codex(providers: &[Arc<dyn Provider>]) -> Arc<dyn Provider> {
    providers
        .iter()
        .find(|provider| provider.id() == PROVIDER_ID)
        .cloned()
        .expect("openai-codex loaded")
}

/// The access token the tests stream with; the account header derives from its JWT payload.
fn access_jwt(account: &str) -> String {
    let part = |value: Value| URL_SAFE_NO_PAD.encode(serde_json::to_vec(&value).unwrap());
    format!(
        "{}.{}.signature",
        part(json!({"alg": "none"})),
        part(json!({"https://api.openai.com/auth": {"chatgpt_account_id": account}})),
    )
}

/// A catalog model pointed at a loopback base URL.
fn model(base: &str) -> Model {
    let mut model = models().remove(0);
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
    account: String,
    openai_beta: String,
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
    let header = |name: &str| {
        head.lines()
            .find_map(|line| {
                let (key, value) = line.split_once(':')?;
                key.eq_ignore_ascii_case(name)
                    .then(|| value.trim().to_owned())
            })
            .unwrap_or_default()
    };
    Seen {
        path: head
            .lines()
            .next()
            .unwrap()
            .split_whitespace()
            .nth(1)
            .unwrap()
            .to_owned(),
        authorization: header("authorization"),
        account: header("chatgpt-account-id"),
        openai_beta: header("openai-beta"),
        body,
    }
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
    assert_eq!(providers.len(), 6);
    let snapshot = session.snapshot().expect("bundle loaded");
    assert_eq!(snapshot.providers().len(), 6);

    let manifest = snapshot
        .provider_manifest(PROVIDER_ID)
        .expect("openai-codex manifest");
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
    assert_eq!(ids, ["gpt-6.1-sol", "gpt-6.1-terra", "gpt-5.6-luna"]);
    assert!(manifest
        .models
        .iter()
        .all(|model| model.provider == PROVIDER_ID && model.headers.is_none()));

    assert!(snapshot
        .providers()
        .iter()
        .any(|provider| provider.provider_id == PROVIDER_ID && provider.stream_fn.is_some()));
    let oauth = rpi_extensions::list_oauth_providers(snapshot);
    assert!(oauth.iter().any(|provider| provider.id == PROVIDER_ID));
    assert_eq!(codex(&providers).models().len(), 3);
}

#[tokio::test]
async fn oauth_begin_issues_an_s256_pkce_url_with_the_public_client() {
    let (session, _providers) = load();
    let snapshot = session.snapshot_arc().expect("bundle loaded");
    // The public Codex CLI client needs no configuration: begin works unconfigured.
    let response = rpi_extensions::request_oauth(&snapshot, PROVIDER_ID, "begin", json!({}))
        .expect("begin succeeds without configuration");
    assert_eq!(response["kind"], "interaction");

    let login = Url::parse(response["payload"]["display"]["url"].as_str().unwrap()).unwrap();
    assert_eq!(login.host_str(), Some("auth.openai.com"));
    assert_eq!(login.path(), "/oauth/authorize");
    let query: std::collections::HashMap<_, _> = login.query_pairs().into_owned().collect();
    assert_eq!(query["client_id"], "app_EMoamEEZ73f0CkXaXp7hrann");
    assert_eq!(query["response_type"], "code");
    assert_eq!(query["code_challenge_method"], "S256");
    assert_eq!(query["redirect_uri"], "http://localhost:1455/auth/callback");
    assert_eq!(query["scope"], "openid profile email offline_access");
    assert_eq!(query["originator"], "pi");
    // The state is a 32-byte nonce; the verifier never leaves the process.
    assert_eq!(URL_SAFE_NO_PAD.decode(&query["state"]).unwrap().len(), 32);
    assert_eq!(query["code_challenge"].len(), 43, "S256 digest, base64url");
}

#[tokio::test]
async fn oauth_revoke_succeeds_for_local_cleanup_without_network() {
    let (session, _providers) = load();
    let snapshot = session.snapshot_arc().unwrap();
    let response = rpi_extensions::request_oauth(
        &snapshot,
        PROVIDER_ID,
        "revoke",
        json!({"credential":{"access":"local-only","refresh":"local-only"}}),
    )
    .unwrap();
    assert_eq!(response, json!({"version":1,"kind":"success","payload":{}}));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn streams_text_thinking_and_tool_calls_over_responses_sse() {
    let (_session, providers) = load();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let (release_tx, release_rx) = oneshot::channel::<()>();
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let seen = read_request(&mut socket).await;
        assert_eq!(seen.path, STREAM_PATH);
        // The account header comes from the JWT, not from stored metadata.
        assert_eq!(
            seen.authorization,
            format!("Bearer {}", access_jwt("acct-stream"))
        );
        assert_eq!(seen.account, "acct-stream");
        assert_eq!(seen.openai_beta, "responses=experimental");
        // Pi's wire shape: Responses streaming with store false.
        assert_eq!(seen.body["model"], "gpt-6.1-sol");
        assert_eq!(seen.body["store"], false);
        assert_eq!(seen.body["stream"], true);
        assert_eq!(seen.body["instructions"], "You are a helpful assistant.");
        assert_eq!(seen.body["input"][0]["role"], "user");
        start_stream(&mut socket).await;
        for data in [
            json!({"type": "response.created", "response": {"id": "resp_1"}}),
            json!({"type": "response.output_item.added", "output_index": 0, "item": {"type": "reasoning", "id": "rs_1"}}),
            json!({"type": "response.reasoning_summary_text.delta", "output_index": 0, "delta": "ponder"}),
            json!({"type": "response.output_item.done", "output_index": 0, "item": {"type": "reasoning", "id": "rs_1", "summary": [{"text": "ponder"}]}}),
            json!({"type": "response.output_item.added", "output_index": 1, "item": {"type": "message", "id": "msg_1"}}),
            json!({"type": "response.output_text.delta", "output_index": 1, "delta": "Hel"}),
        ] {
            send_chunk(&mut socket, &event(data)).await;
        }
        // The rest arrives only after the client has seen the first text delta.
        release_rx.await.unwrap();
        for data in [
            json!({"type": "response.output_text.delta", "output_index": 1, "delta": "lo"}),
            json!({"type": "response.output_item.done", "output_index": 1, "item": {"type": "message", "id": "msg_1", "content": [{"type": "output_text", "text": "Hello"}]}}),
            json!({"type": "response.output_item.added", "output_index": 2, "item": {"type": "function_call", "id": "fc_1", "call_id": "call_9", "name": "lookup", "arguments": ""}}),
            json!({"type": "response.function_call_arguments.delta", "output_index": 2, "delta": "{\"q\": \"x\"}"}),
            json!({"type": "response.output_item.done", "output_index": 2, "item": {"type": "function_call", "id": "fc_1", "call_id": "call_9", "name": "lookup", "arguments": "{\"q\": \"x\"}"}}),
            json!({"type": "response.completed", "response": {"id": "resp_1", "status": "completed", "usage": {"input_tokens": 12, "output_tokens": 5, "total_tokens": 17, "input_tokens_details": {"cached_tokens": 2, "cache_write_tokens": 1}}}}),
        ] {
            send_chunk(&mut socket, &event(data)).await;
        }
        finish_chunks(&mut socket).await;
    });

    let mut stream = codex(&providers)
        .stream_simple(
            &model(&base),
            &context(),
            &options(&access_jwt("acct-stream")),
        )
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
            "toolcall_delta",
            "toolcall_end",
            "done",
        ]
    );
    assert_eq!(terminals, 1, "exactly one terminal event");
    let AssistantMessageEvent::ToolCallDelta { delta, .. } = &events[9] else {
        panic!("tool call delta: {:?}", events[9]);
    };
    assert_eq!(delta, "{\"q\": \"x\"}");
    let AssistantMessageEvent::ToolCallEnd { tool_call, .. } = &events[10] else {
        panic!("tool call end: {:?}", events[10]);
    };
    assert_eq!(tool_call.name, "lookup");
    assert_eq!(tool_call.id, "call_9|fc_1");
    assert_eq!(tool_call.arguments, json!({"q": "x"}));
    let AssistantMessageEvent::Done { reason, message } = events.last().unwrap() else {
        panic!("done");
    };
    assert_eq!(*reason, rpi_ai::DoneReason::ToolUse);
    assert_eq!(message.stop_reason, StopReason::ToolUse);
    assert_eq!(text_of(&message.content), "Hello");
    assert_eq!(message.usage.input, 9);
    assert_eq!(message.usage.output, 5);
    assert_eq!(message.usage.cache_read, 2);
    assert_eq!(message.response_id.as_deref(), Some("resp_1"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reasoning_effort_comes_from_the_model_map() {
    let (_session, providers) = load();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let seen = read_request(&mut socket).await;
        assert_eq!(
            seen.body["reasoning"],
            json!({"effort": "medium", "summary": "auto"})
        );
        assert_eq!(seen.body["include"], json!(["reasoning.encrypted_content"]));
        start_stream(&mut socket).await;
        send_chunk(
            &mut socket,
            &event(json!({"type": "response.output_item.added", "output_index": 0, "item": {"type": "message", "id": "m"}})),
        )
        .await;
        send_chunk(
            &mut socket,
            &event(json!({"type": "response.output_text.delta", "output_index": 0, "delta": "ok"})),
        )
        .await;
        send_chunk(
            &mut socket,
            &event(json!({"type": "response.completed", "response": {"id": "r", "status": "completed", "usage": {"input_tokens": 1, "output_tokens": 1, "total_tokens": 2}}})),
        )
        .await;
        finish_chunks(&mut socket).await;
    });

    let mut options = options(&access_jwt("acct-reasoning"));
    options.reasoning = Some(rpi_ai::types::ThinkingLevel::Medium);
    let mut stream = codex(&providers)
        .stream_simple(&model(&base), &context(), &options)
        .await;
    let (events, terminals) = drain(&mut stream, |_| {}).await;
    server.await.unwrap();
    assert_eq!(terminals, 1);
    assert!(
        matches!(events.last(), Some(AssistantMessageEvent::Done { .. })),
        "{events:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancellation_mid_stream_keeps_partial_text_and_aborts() {
    let (_session, providers) = load();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let seen = read_request(&mut socket).await;
        assert_eq!(seen.path, STREAM_PATH);
        start_stream(&mut socket).await;
        send_chunk(
            &mut socket,
            &event(json!({"type": "response.output_item.added", "output_index": 0, "item": {"type": "message", "id": "m"}})),
        )
        .await;
        send_chunk(
            &mut socket,
            &event(json!({"type": "response.output_text.delta", "output_index": 0, "delta": "partial"})),
        )
        .await;
        // Hold the response open until the client drops the connection.
        let mut buffer = [0; 64];
        let _ = socket.read(&mut buffer).await;
    });

    let options = options(&access_jwt("acct-cancel"));
    let signal = options.signal.clone();
    let mut stream = codex(&providers)
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
async fn http_errors_become_one_terminal_without_leaking_credentials_or_accounts() {
    let (_session, providers) = load();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let account = "acct-denied-0123456789abcdef";
    let server = tokio::spawn(async move {
        for (status, body) in [
            (
                401,
                r#"{"error":{"code":"invalid_api_key","message":"Bearer token and account acct-denied-0123456789abcdef are not authorized"}}"#,
            ),
            (500, "upstream exploded for the token"),
        ] {
            let (mut socket, _) = listener.accept().await.unwrap();
            // Drain the request before closing: unread bytes can cause a TCP reset
            // that truncates the HTTP error response under parallel execution.
            read_request(&mut socket).await;
            respond_json(&mut socket, status, body).await;
        }
    });

    for expected_status in ["HTTP 401", "HTTP 500"] {
        let mut stream = codex(&providers)
            .stream_simple(&model(&base), &context(), &options(&access_jwt(account)))
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
        // Neither the JWT nor the account id may leak into the error text.
        assert!(
            !message.contains("acct-denied"),
            "account leaked: {message}"
        );
        assert!(!message.contains("signature"), "token leaked: {message}");
    }
    server.await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shutdown_cancels_active_streams_and_rejects_new_ones() {
    let (session, providers) = load();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        read_request(&mut socket).await;
        start_stream(&mut socket).await;
        send_chunk(
            &mut socket,
            &event(json!({"type": "response.output_item.added", "output_index": 0, "item": {"type": "message", "id": "m"}})),
        )
        .await;
        send_chunk(
            &mut socket,
            &event(json!({"type": "response.output_text.delta", "output_index": 0, "delta": "partial"})),
        )
        .await;
        let mut buffer = [0; 64];
        let _ = socket.read(&mut buffer).await;
    });

    let provider = codex(&providers);
    let mut active = provider
        .stream_simple(
            &model(&base),
            &context(),
            &options(&access_jwt("acct-shutdown")),
        )
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
        .stream_simple(
            &model(&base),
            &context(),
            &options(&access_jwt("acct-shutdown")),
        )
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
async fn concurrent_accounts_never_share_tokens_or_accounts() {
    let (_session, providers) = load();
    let provider = codex(&providers);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let streams: Arc<Mutex<Vec<Seen>>> = Arc::new(Mutex::new(Vec::new()));
    let recorded = streams.clone();
    let server = tokio::spawn(async move {
        let mut handlers = Vec::new();
        for _ in 0..2 {
            let (mut socket, _) = listener.accept().await.unwrap();
            let recorded = recorded.clone();
            handlers.push(tokio::spawn(async move {
                let seen = read_request(&mut socket).await;
                assert_eq!(seen.path, STREAM_PATH);
                let account = seen.account.clone();
                recorded.lock().unwrap().push(seen);
                start_stream(&mut socket).await;
                send_chunk(
                    &mut socket,
                    &event(json!({"type": "response.output_item.added", "output_index": 0, "item": {"type": "message", "id": "m"}})),
                )
                .await;
                send_chunk(
                    &mut socket,
                    &event(json!({"type": "response.output_text.delta", "output_index": 0, "delta": &account})),
                )
                .await;
                send_chunk(
                    &mut socket,
                    &event(json!({"type": "response.completed", "response": {"id": "r", "status": "completed", "usage": {"input_tokens": 1, "output_tokens": 1, "total_tokens": 2}}})),
                )
                .await;
                finish_chunks(&mut socket).await;
            }));
        }
        for handler in handlers {
            handler.await.unwrap();
        }
    });

    let stream_for = |account: &'static str| {
        let provider = provider.clone();
        let base = base.clone();
        async move {
            let mut stream = provider
                .stream_simple(&model(&base), &context(), &options(&access_jwt(account)))
                .await;
            let (events, terminals) = drain(&mut stream, |_| {}).await;
            assert_eq!(terminals, 1);
            let AssistantMessageEvent::Done { message, .. } = events.last().unwrap() else {
                panic!("expected success: {events:?}");
            };
            (account, text_of(&message.content))
        }
    };
    let (first, second) = tokio::join!(stream_for("acct-alpha"), stream_for("acct-beta"));
    server.await.unwrap();

    assert_eq!(first.1, "acct-alpha");
    assert_eq!(second.1, "acct-beta");
    let streams = streams.lock().unwrap();
    assert_eq!(streams.len(), 2);
    for seen in streams.iter() {
        let account = seen.authorization.trim_start_matches("Bearer ");
        let jwt_account = access_jwt(&seen.account);
        assert_eq!(
            account, jwt_account,
            "each request carries only its own token"
        );
    }
}
