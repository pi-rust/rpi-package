//! Build the real cdylib first, then test the loaded extension rather than a
//! statically constructed provider. See ../README.md for offline and live runs.
use rpi_ai::{
    types::{Content, Message, StopReason, UserMessage},
    AssistantMessageEvent, Context, Model, Provider, SimpleStreamOptions,
};
use rpi_providers::deepseek::{MODEL_ID, PROVIDER_ID};
use std::{path::PathBuf, sync::Arc, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::oneshot,
};

const FIRST_TEXT: &str =
    "A DeepSeek extension chunk arrives before the response ends. 流式输出已到达。";
const LAST_TEXT: &str = " Second chunk.";
const REASONING: &str =
    "Checking the streaming extension path before delivering a synthetic reply. ";

fn load_extension() -> (rpi_extensions::ExtensionSession, Arc<dyn Provider>, Model) {
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
    let library = [
        profile.join(&filename),
        profile.join("deps").join(&filename),
    ]
    .into_iter()
    .find(|path| path.is_file())
    .expect("build the extension first: cargo build -p rpi-providers");
    let session = rpi_extensions::load_session_mixed(
        &[],
        &[PathBuf::from(library)],
        Arc::new(rpi_extensions::NullDiagnostics),
        None,
    );
    let snapshot = session.snapshot().expect("extension loaded");
    let manifest = snapshot
        .provider_manifest(PROVIDER_ID)
        .expect("DeepSeek declaration loaded from the cdylib");
    assert_eq!(manifest.api_key_env.as_deref(), Some("DEEPSEEK_API_KEY"));
    assert!(manifest.requires_auth);
    assert!(
        snapshot
            .providers()
            .iter()
            .any(|provider| provider.provider_id == PROVIDER_ID && provider.stream_fn.is_some()),
        "the native streaming callback must be registered"
    );
    let provider = rpi_extensions::PluggableProvider::from_session(
        &session,
        tokio::runtime::Handle::current(),
    )
    .into_iter()
    .find(|provider| provider.id() == PROVIDER_ID)
    .unwrap();
    let model = provider
        .models()
        .iter()
        .find(|model| model.id == MODEL_ID)
        .cloned()
        .unwrap();
    (session, provider, model)
}

fn context() -> Context {
    Context::new(vec![Message::User(UserMessage::new(
        "Reply briefly for a streaming smoke test.",
        0,
    ))])
}

async fn read_request(socket: &mut TcpStream) -> serde_json::Value {
    let mut request = Vec::new();
    let end = loop {
        let mut buffer = [0; 4096];
        let count = socket.read(&mut buffer).await.unwrap();
        assert_ne!(count, 0, "client closed before request body");
        request.extend_from_slice(&buffer[..count]);
        if let Some(end) = request.windows(4).position(|window| window == b"\r\n\r\n") {
            let headers = String::from_utf8_lossy(&request[..end]).to_ascii_lowercase();
            let length: usize = headers
                .lines()
                .find_map(|line| line.strip_prefix("content-length:"))
                .unwrap()
                .trim()
                .parse()
                .unwrap();
            if request.len() >= end + 4 + length {
                break end;
            }
        }
    };
    let headers = String::from_utf8_lossy(&request[..end]).to_ascii_lowercase();
    assert!(headers.starts_with("post /v1/chat/completions "));
    assert!(headers.contains("authorization: bearer fixture-deepseek-key"));
    let body: serde_json::Value = serde_json::from_slice(&request[end + 4..]).unwrap();
    assert_eq!(body["model"], MODEL_ID);
    assert_eq!(body["stream"], true);
    assert_eq!(body["max_tokens"], 128);
    assert_eq!(body["messages"][0]["role"], "user");
    body
}

async fn http_chunk(socket: &mut TcpStream, bytes: &[u8]) {
    let mut frame = format!("{:x}\r\n", bytes.len()).into_bytes();
    frame.extend_from_slice(bytes);
    frame.extend_from_slice(b"\r\n");
    socket.write_all(&frame).await.unwrap();
}

async fn finish_response(socket: &mut TcpStream) {
    // [DONE] completes SSE. Clients may close before the HTTP trailer arrives.
    // Earlier writes of content and the terminal SSE frame must still succeed.
    if let Err(error) = socket.write_all(b"0\r\n\r\n").await {
        assert!(
            matches!(
                error.kind(),
                std::io::ErrorKind::BrokenPipe
                    | std::io::ErrorKind::ConnectionReset
                    | std::io::ErrorKind::ConnectionAborted
                    | std::io::ErrorKind::NotConnected
            ),
            "unexpected HTTP trailer error after [DONE]: {error}"
        );
    }
}

fn sse(delta: serde_json::Value, finish: serde_json::Value) -> String {
    format!(
        "data: {}\n\n",
        serde_json::json!({"id":"deepseek-fixture","object":"chat.completion.chunk","model":MODEL_ID,
        "choices":[{"index":0,"delta":delta,"finish_reason":finish}]})
    )
}

async fn stub(reject: bool) -> (String, oneshot::Sender<()>, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}/v1", listener.local_addr().unwrap());
    let (release, gate) = oneshot::channel();
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        read_request(&mut socket).await;
        if reject {
            let body =
                r#"{"error":{"message":"fixture rejected key","type":"authentication_error"}}"#;
            socket.write_all(format!("HTTP/1.1 401 Unauthorized\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
            return;
        }
        socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n").await.unwrap();
        http_chunk(
            &mut socket,
            sse(
                serde_json::json!({"role":"assistant","reasoning_content":REASONING}),
                serde_json::Value::Null,
            )
            .as_bytes(),
        )
        .await;
        let text = sse(
            serde_json::json!({"content":FIRST_TEXT}),
            serde_json::Value::Null,
        );
        // Split an SSE event inside a multibyte UTF-8 character as HTTP chunks.
        let split = text.find('流').unwrap() + 1;
        http_chunk(&mut socket, &text.as_bytes()[..split]).await;
        http_chunk(&mut socket, &text.as_bytes()[split..]).await;
        // No finish frame or EOF until the host has observed a text delta.
        // A fake stream that waits for the whole response will time out here.
        if gate.await.is_err() {
            return;
        }
        let mut last = sse(
            serde_json::json!({"content":LAST_TEXT}),
            serde_json::json!("stop"),
        );
        last.push_str(&format!("data: {}\n\n", serde_json::json!({"choices":[],"usage":{"prompt_tokens":5,"completion_tokens":7,"total_tokens":12}})));
        last.push_str("data: [DONE]\n\n");
        http_chunk(&mut socket, last.as_bytes()).await;
        finish_response(&mut socket).await;
    });
    (base, release, server)
}

async fn next(stream: &mut rpi_ai::AssistantMessageEventStream) -> Option<AssistantMessageEvent> {
    tokio::time::timeout(Duration::from_secs(5), stream.next())
        .await
        .expect("extension must deliver an event without waiting for server EOF")
}

fn options() -> SimpleStreamOptions {
    SimpleStreamOptions {
        api_key: Some("fixture-deepseek-key".into()),
        max_tokens: Some(128),
        max_retries: Some(0),
        ..Default::default()
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn real_extension_delivers_reasoning_and_text_before_server_finishes() {
    let (_session, provider, mut model) = load_extension();
    let (base, release, server) = stub(false).await;
    model.base_url = base;
    let mut stream = provider.stream_simple(&model, &context(), &options()).await;
    let mut tags = vec![];
    let mut text = String::new();
    let mut reasoning = String::new();
    loop {
        let event = next(&mut stream).await.expect("text before terminal");
        tags.push(event.type_tag());
        match event {
            AssistantMessageEvent::ThinkingDelta { delta, .. } => reasoning.push_str(&delta),
            AssistantMessageEvent::TextDelta { delta, .. } => {
                text.push_str(&delta);
                break;
            }
            AssistantMessageEvent::Done { .. } | AssistantMessageEvent::Error { .. } => {
                panic!("terminal before incremental output")
            }
            _ => {}
        }
    }
    assert_eq!(reasoning, REASONING);
    assert_eq!(text, FIRST_TEXT);
    assert!(
        !server.is_finished(),
        "delta must arrive while response is still open"
    );
    release.send(()).unwrap();
    while let Some(event) = next(&mut stream).await {
        tags.push(event.type_tag());
        match event {
            AssistantMessageEvent::TextDelta { delta, .. } => text.push_str(&delta),
            AssistantMessageEvent::Error { error, .. } => {
                panic!("unexpected error: {:?}", error.stop_reason)
            }
            _ => {}
        }
    }
    let message = stream.result().await.unwrap();
    assert_eq!(message.provider, PROVIDER_ID);
    assert_eq!(message.stop_reason, StopReason::Stop);
    assert_eq!(text, format!("{FIRST_TEXT}{LAST_TEXT}"));
    assert_eq!(Content::text_only(&message.content, ""), text);
    assert_eq!(message.usage.total_tokens, 12);
    assert_eq!(tags.first(), Some(&"start"));
    assert_eq!(tags.last(), Some(&"done"));
    assert_eq!(tags.iter().filter(|tag| **tag == "done").count(), 1);
    assert_eq!(tags.iter().filter(|tag| **tag == "text_delta").count(), 2);
    tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn real_extension_cancels_an_open_response_and_retains_partial_text() {
    let (_session, provider, mut model) = load_extension();
    let (base, release, server) = stub(false).await;
    model.base_url = base;
    let options = options();
    let mut stream = provider.stream_simple(&model, &context(), &options).await;
    loop {
        match next(&mut stream)
            .await
            .expect("partial text before cancellation")
        {
            AssistantMessageEvent::TextDelta { .. } => break,
            AssistantMessageEvent::Done { .. } | AssistantMessageEvent::Error { .. } => {
                panic!("unexpected terminal before cancel")
            }
            _ => {}
        }
    }
    options.signal.cancel();
    let mut terminal = vec![];
    while let Some(event) = next(&mut stream).await {
        if event.is_terminal() {
            terminal.push(event.type_tag());
        }
    }
    let message = stream.result().await.unwrap();
    assert_eq!(message.stop_reason, StopReason::Aborted);
    assert_eq!(Content::text_only(&message.content, ""), FIRST_TEXT);
    assert_eq!(terminal, ["error"]);
    drop(release); // The server never sends a successful finish frame.
    tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn real_extension_reports_auth_errors_without_a_success_terminal() {
    let (_session, provider, mut model) = load_extension();
    let (base, _release, server) = stub(true).await;
    model.base_url = base;
    let mut stream = provider.stream_simple(&model, &context(), &options()).await;
    let mut terminal = vec![];
    while let Some(event) = next(&mut stream).await {
        if event.is_terminal() {
            terminal.push(event.type_tag());
        }
    }
    let message = stream.result().await.unwrap();
    assert_eq!(message.stop_reason, StopReason::Error);
    assert!(message.error_message.as_deref().unwrap().contains("401"));
    assert_eq!(terminal, ["error"]);
    tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .unwrap()
        .unwrap();
}

async fn reply(socket: &mut TcpStream, chunks: &[String]) {
    socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\n\r\n").await.unwrap();
    for chunk in chunks {
        http_chunk(socket, chunk.as_bytes()).await;
    }
    http_chunk(socket, b"data: [DONE]\n\n").await;
    finish_response(socket).await;
}

async fn collect(
    provider: &Arc<dyn Provider>,
    model: &Model,
    context: &Context,
    options: &SimpleStreamOptions,
) -> (rpi_ai::AssistantMessage, Vec<&'static str>) {
    let mut stream = provider.stream_simple(model, context, options).await;
    let mut tags = vec![];
    while let Some(event) = next(&mut stream).await {
        tags.push(event.type_tag());
        assert!(
            !matches!(event, AssistantMessageEvent::Error { .. }),
            "unexpected error terminal"
        );
    }
    assert_eq!(tags.iter().filter(|tag| **tag == "done").count(), 1);
    (stream.result().await.unwrap(), tags)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn real_extension_replays_reasoning_through_tools_and_the_next_user_turn() {
    use rpi_ai::types::{Schema, Tool, ToolResultMessage, ToolResultRole};
    let (_session, provider, mut model) = load_extension();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    model.base_url = format!("http://{}/v1", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        for turn in 0..3 {
            let (mut socket, _) = listener.accept().await.unwrap();
            let request = read_request(&mut socket).await;
            assert_eq!(request["thinking"]["type"], "enabled");
            assert_eq!(request["reasoning_effort"], "high");
            assert_eq!(request["tools"][0]["function"]["name"], "weather");
            if turn > 0 {
                let assistants: Vec<_> = request["messages"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .filter(|message| message["role"] == "assistant")
                    .collect();
                assert_eq!(
                    assistants[0]["reasoning_content"],
                    "Need weather from a tool."
                );
                assert!(assistants[0]["content"].is_null());
                assert_eq!(assistants[0]["tool_calls"][0]["id"], "call_weather");
                assert_eq!(
                    assistants[0]["tool_calls"][0]["function"]["arguments"],
                    "{\"city\":\"Hangzhou\"}"
                );
                assert!(request["messages"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|message| message["role"] == "tool"
                        && message["tool_call_id"] == "call_weather"
                        && message["content"] == "sunny"));
                if turn == 2 {
                    assert_eq!(assistants.len(), 2);
                    assert_eq!(
                        assistants[1]["reasoning_content"],
                        "Now explain the result."
                    );
                    assert_eq!(assistants[1]["content"], "It is sunny.");
                    assert_eq!(
                        request["messages"].as_array().unwrap().last().unwrap()["role"],
                        "user"
                    );
                }
            }
            if turn == 0 {
                reply(&mut socket, &[
                    sse(serde_json::json!({"reasoning_content":"Need weather from a tool."}), serde_json::Value::Null),
                    sse(serde_json::json!({"tool_calls":[{"index":0,"id":"call_weather","type":"function","function":{"name":"weather","arguments":"{\"city\":"}}]}), serde_json::Value::Null),
                    sse(serde_json::json!({"tool_calls":[{"index":0,"function":{"arguments":"\"Hangzhou\"}"}}]}), serde_json::Value::Null),
                    sse(serde_json::json!({}), serde_json::json!("tool_calls")),
                ]).await;
            } else {
                reply(
                    &mut socket,
                    &[
                        sse(
                            serde_json::json!({"reasoning_content":"Now explain the result."}),
                            serde_json::Value::Null,
                        ),
                        sse(
                            serde_json::json!({"content":"It is sunny."}),
                            serde_json::Value::Null,
                        ),
                        sse(serde_json::json!({}), serde_json::json!("stop")),
                    ],
                )
                .await;
            }
        }
    });
    let mut context = context();
    context.tools.push(Tool {
        name: "weather".into(), description: "Get weather".into(),
        parameters: Schema::new(serde_json::json!({"type":"object","properties":{"city":{"type":"string"}},"required":["city"]})),
        constrained_sampling: None,
    });
    let mut opts = options();
    opts.reasoning = Some(rpi_ai::ThinkingLevel::Medium);
    let (first, tags) = collect(&provider, &model, &context, &opts).await;
    assert_eq!(first.stop_reason, StopReason::ToolUse);
    assert!(
        tags.contains(&"toolcall_start")
            && tags.contains(&"toolcall_delta")
            && tags.contains(&"toolcall_end")
    );
    let call = first
        .content
        .iter()
        .find_map(|block| match block {
            Content::ToolCall(call) => Some(call),
            _ => None,
        })
        .unwrap();
    assert_eq!(call.id, "call_weather");
    assert_eq!(call.name, "weather");
    assert_eq!(call.arguments, serde_json::json!({"city":"Hangzhou"}));
    context.messages.push(Message::Assistant(Box::new(first)));
    context
        .messages
        .push(Message::ToolResult(Box::new(ToolResultMessage {
            role: ToolResultRole,
            tool_call_id: "call_weather".into(),
            tool_name: "weather".into(),
            content: vec![Content::text("sunny")],
            details: None,
            usage: None,
            added_tool_names: vec![],
            is_error: false,
            timestamp: 1,
        })));
    let (second, _) = collect(&provider, &model, &context, &opts).await;
    assert_eq!(second.stop_reason, StopReason::Stop);
    assert_eq!(Content::text_only(&second.content, ""), "It is sunny.");
    context.messages.push(Message::Assistant(Box::new(second)));
    context
        .messages
        .push(Message::User(UserMessage::new("And tomorrow?", 2)));
    let (third, _) = collect(&provider, &model, &context, &opts).await;
    assert_eq!(third.stop_reason, StopReason::Stop);
    tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn real_extension_explicitly_disables_thinking() {
    let (_session, provider, mut model) = load_extension();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    model.base_url = format!("http://{}/v1", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let request = read_request(&mut socket).await;
        assert_eq!(request["thinking"]["type"], "disabled");
        assert!(request.get("reasoning_effort").is_none());
        reply(
            &mut socket,
            &[
                sse(
                    serde_json::json!({"content":"without thinking"}),
                    serde_json::Value::Null,
                ),
                sse(serde_json::json!({}), serde_json::json!("stop")),
            ],
        )
        .await;
    });
    let mut opts = options();
    opts.reasoning = Some(rpi_ai::ThinkingLevel::Off);
    let (message, _) = collect(&provider, &model, &context(), &opts).await;
    assert_eq!(Content::text_only(&message.content, ""), "without thinking");
    tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "live API smoke test; requires DEEPSEEK_API_KEY and consumes API quota"]
async fn live_deepseek_extension_stream() {
    let key = std::env::var("DEEPSEEK_API_KEY")
        .ok()
        .filter(|key| !key.trim().is_empty())
        .expect("set DEEPSEEK_API_KEY before opting into the live test");
    let (_session, provider, model) = load_extension();
    let options = SimpleStreamOptions {
        api_key: Some(key),
        max_tokens: Some(1_024),
        max_retries: Some(0),
        timeout: Some(Duration::from_secs(45)),
        ..Default::default()
    };
    let context = Context::new(vec![Message::User(UserMessage::new(
        "Write ten numbered short English sentences about the weather, one sentence per line. No introduction. This is a streaming transport test.",
        0,
    ))]);
    let mut stream = provider.stream_simple(&model, &context, &options).await;
    let mut deltas = 0;
    while let Some(event) = tokio::time::timeout(Duration::from_secs(60), stream.next())
        .await
        .expect("live stream timed out")
    {
        match event {
            AssistantMessageEvent::TextDelta { delta, .. } => {
                deltas += 1;
                eprint!("{delta}");
            }
            AssistantMessageEvent::Error { error, .. } => {
                panic!("DeepSeek request failed: {:?}", error.stop_reason)
            }
            _ => {}
        }
    }
    let message = stream.result().await.unwrap();
    assert!(matches!(
        message.stop_reason,
        StopReason::Stop | StopReason::Length
    ));
    assert!(
        deltas > 1 && !Content::text_only(&message.content, "").is_empty(),
        "live smoke test needs multiple text deltas to validate incremental delivery"
    );
    eprintln!("\nprovider={} text_delta_events={deltas}", message.provider);
}
