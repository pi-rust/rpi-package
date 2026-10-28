//! Load the real bundle and verify transport controls without a model service.
use std::{path::PathBuf, sync::Arc, time::Duration};

use rpi_ai::{
    AssistantMessage, AssistantMessageEvent, Content, Context, Message, Model, Provider,
    SimpleStreamOptions, StopReason, StreamingProtocolCompat, ThinkingLevel,
};
use serde_json::{json, Value};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::oneshot,
};

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

fn providers() -> (rpi_extensions::ExtensionSession, Vec<Arc<dyn Provider>>) {
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
    assert_eq!(providers.len(), 5);
    (session, providers)
}

/// The OpenAI-chat fixtures below cannot serve Antigravity, which speaks Cloud Code's
/// Gemini SSE protocol. Its lifecycle, streaming and cancellation coverage lives in
/// tests/antigravity.rs; every other provider keeps the shared assertions unchanged.
fn chat_providers() -> (rpi_extensions::ExtensionSession, Vec<Arc<dyn Provider>>) {
    let (session, providers) = providers();
    let chat = providers
        .into_iter()
        .filter(|provider| provider.id() != rpi_providers::antigravity::PROVIDER_ID)
        .collect();
    (session, chat)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires the matching host in RPI_TEST_CLI; loopback HTTP only"]
async fn host_retries_extension_stalls_and_checks_compat_after_manifest_defaults() {
    let host = std::env::var_os("RPI_TEST_CLI").expect("set RPI_TEST_CLI");
    let root = tempfile::tempdir().unwrap();
    let agent = root.path().join("agent");
    std::fs::create_dir(&agent).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    std::fs::write(agent.join("models.json"), json!({"providers":{"proxy":{
        "apiKey":"fixture-only", "baseUrl":format!("http://{address}/v1"),
        "models":[{"id":"custom", "reasoning":true, "contextWindow":131072, "maxTokens":128,
            "compat":{"thinkingFormat":"deepseek", "requiresReasoningContentOnAssistantMessages":true,
                "misspelledFlag":true}}]
    }}}).to_string()).unwrap();
    std::fs::write(
        agent.join("settings.json"),
        json!({
            "compaction":{"enabled":false}, "retry":{"enabled":true,"maxRetries":1,"baseDelayMs":0}
        })
        .to_string(),
    )
    .unwrap();
    let server = tokio::spawn(async move {
        let mut stalled = None;
        for attempt in 0..2 {
            let (mut socket, _) = listener.accept().await.unwrap();
            let request = read_request(&mut socket).await;
            assert_eq!(request["thinking"]["type"], "enabled");
            if attempt == 0 {
                socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\n\r\n").await.unwrap();
                let data = frame("partial", false);
                socket
                    .write_all(format!("{:x}\r\n{data}\r\n", data.len()).as_bytes())
                    .await
                    .unwrap();
                stalled = Some(socket);
            } else {
                let data = format!(
                    "{}data: [DONE]\n\n",
                    frame("Recovered through extension.", true)
                );
                socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{data}", data.len()).as_bytes()).await.unwrap();
            }
        }
        drop(stalled);
    });
    let mut command = tokio::process::Command::new(host);
    command
        .current_dir(root.path())
        .env("RPI_CODING_AGENT_DIR", agent)
        .env_remove("HTTP_PROXY")
        .env_remove("HTTPS_PROXY")
        .env_remove("ALL_PROXY")
        .arg("--extension")
        .arg(library_path())
        .args([
            "--offline",
            "--package-only",
            "--no-session",
            "--no-builtin-tools",
            "--no-skills",
            "--no-context-files",
            "--no-prompt-templates",
            "--provider",
            "proxy",
            "--model",
            "custom",
            "--thinking",
            "high",
            "--print",
            "--timeout=10",
            "--stream-idle-timeout=1",
            "hello",
        ])
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true);
    let output = tokio::time::timeout(Duration::from_secs(8), command.output())
        .await
        .unwrap()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stderr}");
    assert!(String::from_utf8_lossy(&output.stdout).contains("Recovered through extension."));
    assert!(
        stderr.contains("unknown field \"misspelledFlag\""),
        "{stderr}"
    );
    tokio::time::timeout(Duration::from_secs(2), server)
        .await
        .unwrap()
        .unwrap();
}

async fn read_request(socket: &mut TcpStream) -> Value {
    read_request_with_headers(socket).await.0
}

async fn read_request_with_headers(socket: &mut TcpStream) -> (Value, String) {
    let mut bytes = Vec::new();
    loop {
        let mut buffer = [0; 4096];
        let count = socket.read(&mut buffer).await.unwrap();
        assert_ne!(count, 0);
        bytes.extend_from_slice(&buffer[..count]);
        assert!(bytes.len() < 1_000_000);
        if let Some(end) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
            let headers = String::from_utf8_lossy(&bytes[..end]).to_ascii_lowercase();
            let length: usize = headers
                .lines()
                .find_map(|line| line.strip_prefix("content-length:"))
                .unwrap()
                .trim()
                .parse()
                .unwrap();
            if bytes.len() >= end + 4 + length {
                return (
                    serde_json::from_slice(&bytes[end + 4..end + 4 + length]).unwrap(),
                    String::from_utf8(bytes[..end].to_vec()).unwrap(),
                );
            }
        }
    }
}

#[tokio::test]
async fn proxy_header_overrides_do_not_send_duplicate_credentials_or_routing_headers() {
    use rpi_providers::proxy::{ProxyConfig, ProxyProvider};
    for request_override in [false, true] {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let (_, headers) = read_request_with_headers(&mut socket).await;
            let body = format!("{}data: [DONE]\n\n", frame("done", true));
            socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
            headers
        });
        let base = format!("http://{address}/v1");
        let provider = ProxyProvider::new(
            ProxyConfig::new(&base)
                .with_header("X-Tenant", "config-tenant")
                .with_header("authorization", "Bearer ConfigToken"),
        );
        let mut model = Model::new(
            "fixture",
            "Fixture",
            rpi_ai::Api::OpenaiCompletions,
            "proxy",
            base,
        );
        model.headers = Some(std::collections::BTreeMap::from([
            ("x-tenant".into(), "model-tenant".into()),
            ("Authorization".into(), "Bearer ModelToken".into()),
        ]));
        let mut options = options(Duration::from_secs(1));
        options.api_key = None;
        if request_override {
            options.headers = Some(std::collections::BTreeMap::from([
                ("x-TENANT".into(), "request-tenant".into()),
                ("aUtHoRiZaTiOn".into(), "Bearer RequestToken".into()),
            ]));
        }
        let stream = provider
            .stream_simple(&model, &Context::new(vec![]), &options)
            .await;
        let message = tokio::time::timeout(Duration::from_secs(2), stream.result())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(message.stop_reason, StopReason::Stop, "{message:?}");
        let headers = server.await.unwrap();
        let expected = if request_override {
            ["request-tenant", "Bearer RequestToken"]
        } else {
            ["config-tenant", "Bearer ConfigToken"]
        };
        for (name, value) in ["x-tenant", "authorization"].into_iter().zip(expected) {
            let values: Vec<_> = headers
                .lines()
                .filter_map(|line| {
                    let (key, value) = line.split_once(':')?;
                    key.eq_ignore_ascii_case(name).then(|| value.trim())
                })
                .collect();
            assert_eq!(
                values,
                [value],
                "{name}; request override={request_override}"
            );
        }
    }
}

fn frame(text: &str, terminal: bool) -> String {
    format!(
        "data: {}\n\n",
        json!({"choices":[{"index":0,"delta":{"content":text},
        "finish_reason": if terminal { json!("stop") } else { Value::Null }}]})
    )
}

struct Fixture {
    url: String,
    request: oneshot::Receiver<Value>,
    stop: Option<oneshot::Sender<()>>,
    server: tokio::task::JoinHandle<()>,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.server.abort();
    }
}

impl Fixture {
    async fn new(chunks: Vec<(Duration, String)>, finish: bool) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/v1", listener.local_addr().unwrap());
        let (request_tx, request) = oneshot::channel();
        let (stop, stop_rx) = oneshot::channel();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            request_tx.send(read_request(&mut socket).await).unwrap();
            socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\n\r\n").await.unwrap();
            for (delay, chunk) in chunks {
                tokio::time::sleep(delay).await;
                let wire = format!("{:x}\r\n{chunk}\r\n", chunk.len());
                if socket.write_all(wire.as_bytes()).await.is_err() {
                    return;
                }
            }
            if finish {
                // The consumer may close as soon as it reads [DONE].
                let _ = socket
                    .write_all(b"e\r\ndata: [DONE]\n\n\r\n0\r\n\r\n")
                    .await;
            } else {
                let _ = stop_rx.await;
            }
        });
        Self {
            url,
            request,
            stop: Some(stop),
            server,
        }
    }

    async fn finish(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        tokio::time::timeout(Duration::from_secs(3), &mut self.server)
            .await
            .unwrap()
            .unwrap();
    }
}

fn model(provider: &dyn Provider, fixture: &Fixture) -> Model {
    let mut model = provider.models()[0].clone();
    model.base_url = fixture.url.clone();
    model.max_tokens = 128;
    model
}

fn options(idle: Duration) -> SimpleStreamOptions {
    SimpleStreamOptions {
        api_key: Some("fixture-only".into()),
        timeout: Some(Duration::from_secs(3)),
        stream_idle_timeout: Some(idle),
        max_tokens: Some(128),
        ..Default::default()
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn every_loaded_provider_reuses_connections_without_reusing_credentials() {
    let (_session, providers) = chat_providers();
    for provider in providers {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            // Both accounts must arrive on this one connection. A fresh client
            // would connect again and leave this read waiting until timeout.
            for account in ["first-account", "second-account"] {
                let (_, headers) = tokio::time::timeout(
                    Duration::from_secs(3),
                    read_request_with_headers(&mut socket),
                )
                .await
                .expect("the second request must reuse the existing connection");
                let credentials: Vec<_> = headers
                    .lines()
                    .filter_map(|line| {
                        let (name, value) = line.split_once(':')?;
                        name.eq_ignore_ascii_case("authorization")
                            .then(|| value.trim())
                    })
                    .collect();
                assert_eq!(credentials, [format!("Bearer {account}")]);
                let body = format!("{}data: [DONE]\n\n", frame(account, true));
                socket.write_all(format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\n\r\n{body}", body.len(),
                ).as_bytes()).await.unwrap();
            }
        });
        let mut model = provider.models()[0].clone();
        model.base_url = format!("http://{address}/v1");
        for account in ["first-account", "second-account"] {
            let mut options = options(Duration::from_secs(2));
            options.api_key = Some(account.into());
            let stream = provider
                .stream_simple(&model, &Context::new(vec![]), &options)
                .await;
            let message = tokio::time::timeout(Duration::from_secs(3), stream.result())
                .await
                .unwrap()
                .unwrap();
            assert_eq!(
                message.stop_reason,
                StopReason::Stop,
                "{}: {message:?}",
                provider.id()
            );
            assert_eq!(Content::text_only(&message.content, ""), account);
        }
        server.await.unwrap();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn lifecycle_shutdown_cancels_all_accounts_and_rejects_new_requests() {
    let (session, providers) = chat_providers();
    let mut active = Vec::new();
    for provider in &providers {
        let mut fixture =
            Fixture::new(vec![(Duration::ZERO, frame("partial", false))], false).await;
        let stream = provider
            .stream_simple(
                &model(provider.as_ref(), &fixture),
                &Context::new(vec![]),
                &options(Duration::ZERO),
            )
            .await;
        tokio::time::timeout(Duration::from_secs(2), &mut fixture.request)
            .await
            .unwrap()
            .unwrap();
        active.push((fixture, stream));
    }
    session.begin_shutdown();
    session.begin_shutdown();
    for (mut fixture, stream) in active {
        let message = tokio::time::timeout(Duration::from_secs(2), stream.result())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(message.stop_reason, StopReason::Aborted, "{message:?}");
        fixture.finish().await;
    }
    for provider in providers {
        let stream = provider
            .stream_simple(
                &provider.models()[0],
                &Context::new(vec![]),
                &options(Duration::ZERO),
            )
            .await;
        let message = tokio::time::timeout(Duration::from_secs(2), stream.result())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(message.stop_reason, StopReason::Error);
        assert!(message.error_message.unwrap().contains("shutting down"));
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn every_loaded_provider_reports_invalid_transport_configuration() {
    let (_session, providers) = chat_providers();
    for provider in providers {
        let mut model = provider.models()[0].clone();
        model.base_url = ":invalid".into();
        let stream = provider
            .stream_simple(&model, &Context::new(vec![]), &options(Duration::ZERO))
            .await;
        let message = tokio::time::timeout(Duration::from_secs(2), stream.result())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(message.stop_reason, StopReason::Error);
        assert!(message.error_message.is_some());
    }
}

#[tokio::test]
async fn native_provider_constructors_report_transport_errors_without_panicking() {
    let native: Vec<Arc<dyn Provider>> = vec![
        Arc::new(rpi_providers::deepseek::DeepSeekProvider::new(None)),
        Arc::new(rpi_providers::openrouter::OpenRouterProvider::new(None)),
        Arc::new(rpi_providers::proxy::ProxyProvider::new(
            rpi_providers::proxy::ProxyConfig::new("http://127.0.0.1:9/v1"),
        )),
        Arc::new(rpi_providers::llama_cpp::LlamaCppProvider::with_defaults()),
    ];
    for provider in native {
        let model = Model::new(
            "fixture",
            "Fixture",
            rpi_ai::Api::OpenaiCompletions,
            provider.id(),
            ":invalid",
        );
        let stream = provider
            .stream_simple(&model, &Context::new(vec![]), &options(Duration::ZERO))
            .await;
        let message = stream.result().await.unwrap();
        assert_eq!(message.stop_reason, StopReason::Error);
        assert!(message.error_message.is_some());
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn every_loaded_provider_interrupts_stalled_streams_before_the_total_deadline() {
    let (_session, providers) = chat_providers();
    for provider in providers {
        let mut fixture =
            Fixture::new(vec![(Duration::ZERO, frame("partial", false))], false).await;
        let options = options(Duration::from_millis(100));
        let stream = provider
            .stream_simple(
                &model(provider.as_ref(), &fixture),
                &Context::new(vec![]),
                &options,
            )
            .await;
        let message = tokio::time::timeout(Duration::from_secs(2), stream.result())
            .await
            .expect("the plugin must honor idle timeout before the 3-second total deadline")
            .unwrap();
        assert_eq!(message.stop_reason, StopReason::Error, "{}", provider.id());
        assert!(
            message
                .error_message
                .as_deref()
                .unwrap()
                .contains("stream idle timeout"),
            "{message:?}"
        );
        fixture.finish().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn every_loaded_provider_keeps_active_fragmented_streams_alive() {
    let (_session, providers) = chat_providers();
    for provider in providers {
        let data = frame("healthy", true);
        let split = data.len() / 2;
        let chunks = vec![
            (Duration::ZERO, "event: ping\n\ndata: \n\n".into()),
            (Duration::from_millis(200), ": ping\n\n".into()),
            (Duration::from_millis(200), data[..split].into()),
            (Duration::from_millis(200), data[split..].into()),
        ];
        let mut fixture = Fixture::new(chunks, true).await;
        let stream = provider
            .stream_simple(
                &model(provider.as_ref(), &fixture),
                &Context::new(vec![]),
                &options(Duration::from_millis(500)),
            )
            .await;
        let message = tokio::time::timeout(Duration::from_secs(2), stream.result())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(message.stop_reason, StopReason::Stop, "{message:?}");
        assert_eq!(Content::text_only(&message.content, ""), "healthy");
        fixture.finish().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn every_loaded_provider_preserves_the_first_frame_after_a_leading_bom() {
    let (_session, providers) = chat_providers();
    for provider in providers {
        let mut fixture = Fixture::new(
            vec![
                (Duration::ZERO, format!("\u{feff}{}", frame("first", false))),
                (Duration::ZERO, frame("\u{feff}second", true)),
            ],
            true,
        )
        .await;
        let stream = provider
            .stream_simple(
                &model(provider.as_ref(), &fixture),
                &Context::new(vec![]),
                &options(Duration::from_secs(1)),
            )
            .await;
        let message = tokio::time::timeout(Duration::from_secs(2), stream.result())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(message.stop_reason, StopReason::Stop, "{message:?}");
        assert_eq!(
            Content::text_only(&message.content, ""),
            "first\u{feff}second",
            "{}",
            provider.id()
        );
        fixture.finish().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn every_loaded_provider_dispatches_cr_delimited_frames_while_response_stays_open() {
    let (_session, providers) = chat_providers();
    for provider in providers {
        // Exceed delta batching so this checks frame delivery independently of
        // the provider's text coalescing policy.
        let text = "early".repeat(1024);
        let mut fixture = Fixture::new(
            vec![(Duration::ZERO, frame(&text, false).replace('\n', "\r"))],
            false,
        )
        .await;
        let options = options(Duration::from_secs(1));
        let mut stream = provider
            .stream_simple(
                &model(provider.as_ref(), &fixture),
                &Context::new(vec![]),
                &options,
            )
            .await;
        let delta = tokio::time::timeout(Duration::from_secs(2), async {
            while let Some(event) = stream.next().await {
                match event {
                    AssistantMessageEvent::TextDelta { delta, .. } => return delta,
                    AssistantMessageEvent::Done { .. } | AssistantMessageEvent::Error { .. } => {
                        panic!("{}: terminal before CR-delimited text", provider.id());
                    }
                    _ => {}
                }
            }
            panic!("{}: no text before EOF", provider.id());
        })
        .await
        .unwrap();
        assert_eq!(delta, text);
        assert!(!fixture.server.is_finished());
        options.signal.cancel();
        let message = tokio::time::timeout(Duration::from_secs(2), stream.result())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            message.stop_reason,
            StopReason::Aborted,
            "{}: {message:?}",
            provider.id()
        );
        fixture.finish().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn loaded_provider_allows_disabling_and_cancelling_the_idle_watchdog() {
    let (_session, providers) = chat_providers();
    for provider in providers {
        let mut fixture = Fixture::new(
            vec![(Duration::from_millis(150), frame("late", true))],
            true,
        )
        .await;
        let stream = provider
            .stream_simple(
                &model(provider.as_ref(), &fixture),
                &Context::new(vec![]),
                &options(Duration::ZERO),
            )
            .await;
        let message = tokio::time::timeout(Duration::from_secs(2), stream.result())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(message.stop_reason, StopReason::Stop);
        fixture.finish().await;

        let mut fixture =
            Fixture::new(vec![(Duration::ZERO, frame("partial", false))], false).await;
        let options = options(Duration::from_secs(1));
        let stream = provider
            .stream_simple(
                &model(provider.as_ref(), &fixture),
                &Context::new(vec![]),
                &options,
            )
            .await;
        tokio::time::timeout(Duration::from_secs(2), &mut fixture.request)
            .await
            .unwrap()
            .unwrap();
        options.signal.cancel();
        let message = tokio::time::timeout(Duration::from_secs(2), stream.result())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(message.stop_reason, StopReason::Aborted, "{message:?}");
        fixture.finish().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn every_loaded_provider_preserves_compat_and_replayed_reasoning() {
    let (_session, providers) = chat_providers();
    for provider in providers {
        let mut fixture = Fixture::new(vec![(Duration::ZERO, frame("done", true))], true).await;
        let mut model = model(provider.as_ref(), &fixture);
        model.reasoning = true;
        model.compat = Some(StreamingProtocolCompat::OpenaiCompletions(
            serde_json::from_value(json!({
                "thinkingFormat":"deepseek", "requiresReasoningContentOnAssistantMessages":true
                , "supportsReasoningEffort":false
            }))
            .unwrap(),
        ));
        let mut thinking =
            AssistantMessage::empty(model.api.clone(), &model.provider, &model.id, 1);
        thinking.content = vec![
            Content::thinking("retained reasoning"),
            Content::text("answer"),
        ];
        let mut plain = thinking.clone();
        plain.content = vec![Content::text("plain answer")];
        let context = Context::new(vec![
            Message::Assistant(Box::new(thinking)),
            Message::Assistant(Box::new(plain)),
        ]);
        let mut options = options(Duration::from_secs(1));
        options.reasoning = Some(ThinkingLevel::High);
        if provider.id() == "deepseek" {
            options.max_tokens = None;
        }
        let stream = provider.stream_simple(&model, &context, &options).await;
        let message = tokio::time::timeout(Duration::from_secs(2), stream.result())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(message.stop_reason, StopReason::Stop, "{message:?}");
        let request = (&mut fixture.request).await.unwrap();
        assert_eq!(request["thinking"]["type"], "enabled", "{}", provider.id());
        assert!(
            request.get("reasoning_effort").is_none(),
            "{}",
            provider.id()
        );
        if provider.id() == "deepseek" {
            assert!(request.get("store").is_none());
            assert!(
                request["max_tokens"].as_u64().unwrap() > model.max_tokens,
                "partial compat must preserve DeepSeek's separate thinking allowance"
            );
        }
        assert_eq!(
            request["messages"][0]["reasoning_content"],
            "retained reasoning"
        );
        assert_eq!(request["messages"][1]["reasoning_content"], "");
        fixture.finish().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn deepseek_preserves_explicit_thinking_format_and_effort_mapping() {
    let (_session, providers) = chat_providers();
    let provider = providers
        .iter()
        .find(|provider| provider.id() == "deepseek")
        .unwrap();
    let mut fixture = Fixture::new(vec![(Duration::ZERO, frame("done", true))], true).await;
    let mut model = model(provider.as_ref(), &fixture);
    model.thinking_level_map = Some(std::collections::BTreeMap::from([(
        ThinkingLevel::High,
        Some("low".into()),
    )]));
    model.compat = Some(StreamingProtocolCompat::OpenaiCompletions(
        serde_json::from_value(json!({"thinkingFormat":"openai"})).unwrap(),
    ));
    let mut options = options(Duration::from_secs(1));
    options.reasoning = Some(ThinkingLevel::High);
    let stream = provider
        .stream_simple(&model, &Context::new(vec![]), &options)
        .await;
    let message = tokio::time::timeout(Duration::from_secs(2), stream.result())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(message.stop_reason, StopReason::Stop, "{message:?}");
    let request = (&mut fixture.request).await.unwrap();
    assert!(request.get("thinking").is_none());
    assert_eq!(request["reasoning_effort"], "low");
    fixture.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn deepseek_disables_thinking_for_custom_non_reasoning_models() {
    let (_session, providers) = chat_providers();
    let provider = providers
        .iter()
        .find(|provider| provider.id() == "deepseek")
        .unwrap();
    for requested in [None, Some(ThinkingLevel::High)] {
        let mut fixture = Fixture::new(vec![(Duration::ZERO, frame("done", true))], true).await;
        let mut model = model(provider.as_ref(), &fixture);
        // models.json entries without `reasoning` default to false. Preserve
        // explicit thinking=off even if the remote endpoint defaults it on.
        model.reasoning = false;
        let mut options = options(Duration::from_secs(1));
        options.reasoning = requested;
        let stream = provider
            .stream_simple(&model, &Context::new(vec![]), &options)
            .await;
        let message = tokio::time::timeout(Duration::from_secs(2), stream.result())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(message.stop_reason, StopReason::Stop, "{message:?}");
        let request = (&mut fixture.request).await.unwrap();
        assert_eq!(request["thinking"]["type"], "disabled");
        assert!(request.get("reasoning_effort").is_none());
        fixture.finish().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn proxy_config_timeout_is_a_fallback_and_request_options_take_precedence() {
    use rpi_providers::proxy::{ProxyConfig, ProxyProvider};
    let mut fixture = Fixture::new(vec![(Duration::ZERO, frame("partial", false))], false).await;
    let provider =
        ProxyProvider::new(ProxyConfig::new(&fixture.url).with_timeout(Duration::from_millis(50)));
    let model = Model::new(
        "test",
        "Test",
        rpi_ai::Api::OpenaiCompletions,
        "proxy",
        &fixture.url,
    );
    let mut options = options(Duration::ZERO);
    options.timeout = None;
    let stream = provider
        .stream_simple(&model, &Context::new(vec![]), &options)
        .await;
    let message = tokio::time::timeout(Duration::from_secs(2), stream.result())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(message.stop_reason, StopReason::Error);
    assert!(message.error_message.unwrap().contains("timed out"));
    fixture.finish().await;

    let mut fixture = Fixture::new(
        vec![(Duration::from_millis(150), frame("override", true))],
        true,
    )
    .await;
    let provider =
        ProxyProvider::new(ProxyConfig::new(&fixture.url).with_timeout(Duration::from_millis(20)));
    let mut model = model;
    model.base_url = fixture.url.clone();
    options.timeout = Some(Duration::from_secs(1));
    let stream = provider
        .stream_simple(&model, &Context::new(vec![]), &options)
        .await;
    assert_eq!(stream.result().await.unwrap().stop_reason, StopReason::Stop);
    fixture.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn every_loaded_provider_keeps_the_total_deadline_independent_of_idle_timeout() {
    let (_session, providers) = chat_providers();
    for provider in providers {
        let mut fixture =
            Fixture::new(vec![(Duration::ZERO, frame("partial", false))], false).await;
        let mut options = options(Duration::from_secs(1));
        options.timeout = Some(Duration::from_millis(50));
        let stream = provider
            .stream_simple(
                &model(provider.as_ref(), &fixture),
                &Context::new(vec![]),
                &options,
            )
            .await;
        let message = tokio::time::timeout(Duration::from_secs(2), stream.result())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(message.stop_reason, StopReason::Error);
        let error = message.error_message.unwrap();
        assert!(error.contains("timed out"), "{error}");
        assert!(!error.contains("stream idle timeout"), "{error}");
        fixture.finish().await;
    }
}
