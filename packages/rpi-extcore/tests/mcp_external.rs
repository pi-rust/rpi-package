#![cfg(feature = "test-support")]
use rpi_extcore::{
    host::{HostSession, TestEvents},
    mcp::{Exposure, McpConfig, McpServerConfig, StdioServer},
    mcp_external::{
        self as external, Backend, BackendFuture, CancellationToken, ExternalConfig, Operation,
        Outcome, ProtocolError, ToolResult,
    },
};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    sync::{mpsc, Notify},
};

#[derive(Default)]
struct Fake {
    requests: Mutex<Vec<Operation>>,
    cancelled: AtomicUsize,
    stopped: AtomicUsize,
    started: Notify,
}
impl Backend for Fake {
    fn request(
        &self,
        op: Operation,
        token: CancellationToken,
    ) -> BackendFuture<'_, Result<Outcome, String>> {
        Box::pin(async move {
            self.requests.lock().unwrap().push(op.clone());
            if op.method == "tools/list" {
                return Ok(Outcome::Result(
                    json!({"tools":[{"name":"echo","description":"from caller context","inputSchema":{"type":"object"}}]}),
                ));
            }
            assert_eq!(op.method, "tools/call");
            let args = &op.params["arguments"];
            if args["hang"] == true {
                self.started.notify_one();
                token.cancelled().await;
                self.cancelled.fetch_add(1, Ordering::SeqCst);
                return Err("cancelled after cleanup".into());
            }
            if args["transport"] == true {
                return Err("secret transport credential".into());
            }
            if args["protocol"] == true {
                return Ok(Outcome::Error(ProtocolError {
                    code: -32000,
                    message: "secret error".into(),
                    data: Some(Value::Null),
                    extensions: BTreeMap::new(),
                }));
            }
            if args["mrtr"] == true {
                return Ok(Outcome::Result(
                    json!({"resultType":"input_required","inputRequests":{"login":{"method":"elicitation/create","params":{"mode":"form"}}},"requestState":"opaque"}),
                ));
            }
            Ok(Outcome::Result(
                json!({"resultType":"complete","content":[{"type":"text","text":"echo"},{"type":"image","data":"AA==","mimeType":"image/png","annotations":{"audience":["user"]}}],"structuredContent":{"scope":op.scope},"isError":args["fail"] == true,"_meta":{"opaque":"kept"}}),
            ))
        })
    }
    fn shutdown(&self) -> BackendFuture<'_, Result<(), String>> {
        Box::pin(async move {
            self.stopped.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })
    }
}
fn mock() -> (Arc<HostSession>, TestEvents, mpsc::UnboundedReceiver<Value>) {
    let (input, output) = tokio::io::duplex(1024 * 1024);
    let (host, events) = HostSession::test_connection_with_ready(
        input,
        256,
        json!({"type":"ready","protocol":"rpi","supportedCommands":["configure_external_tools","external_tool_response"]}),
    );
    let (tx, rx) = mpsc::unbounded_channel();
    let replies = events.clone();
    tokio::spawn(async move {
        let mut lines = BufReader::new(output).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            let request: Value = serde_json::from_str(&line).unwrap();
            let _ = tx.send(request.clone());
            let _ = replies.send(json!({"type":"response","id":request["id"],"success":true,"data":{"accepted":true}}));
        }
    });
    (host, events, rx)
}
fn config(scope: &str) -> McpConfig {
    McpConfig {
        scope: scope.into(),
        cwd: std::env::current_dir().unwrap(),
        servers: BTreeMap::new(),
        discovery_timeout: Duration::from_secs(3),
    }
}
fn external(fake: Arc<Fake>) -> ExternalConfig {
    ExternalConfig {
        servers: BTreeMap::from([("client".into(), "registration-1".into())]),
        backend: fake,
        metadata: BTreeMap::from([
            (
                "io.modelcontextprotocol/protocolVersion".into(),
                json!(external::PROTOCOL_VERSION),
            ),
            (
                "io.modelcontextprotocol/clientCapabilities".into(),
                json!({}),
            ),
            (
                "io.modelcontextprotocol/clientInfo".into(),
                json!({"name":"test","version":"1"}),
            ),
            ("caller-context".into(), json!({"principal":"authorized"})),
        ]),
    }
}
async fn next(rx: &mut mpsc::UnboundedReceiver<Value>) -> Value {
    tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .unwrap()
        .unwrap()
}
fn call(events: &TestEvents, name: &str, id: &str, arguments: Value) {
    events.send(json!({"type":"external_tool_request","toolName":name,"requestId":id,"arguments":arguments})).unwrap();
}
#[tokio::test]
async fn discovery_calls_errors_fidelity_dedup_and_mrtr() {
    let fake = Arc::new(Fake::default());
    let (host, events, mut rx) = mock();
    let mut binding = external::attach(host.clone(), config("one"), external(fake.clone()))
        .await
        .unwrap();
    let configured = next(&mut rx).await;
    let name = configured["tools"][0]["name"].as_str().unwrap();
    assert_eq!(configured["tools"].as_array().unwrap().len(), 1);
    assert_eq!(binding.descriptors()[0].remote_name, "echo");
    call(&events, name, "call-1", json!({"fail":true}));
    let response = next(&mut rx).await;
    assert_eq!(response["requestId"], "call-1");
    assert_eq!(response["result"]["isError"], true);
    assert_eq!(
        response["result"]["structuredContent"],
        json!({"scope":"one"})
    );
    assert_eq!(response["result"]["_meta"], json!({"opaque":"kept"}));
    assert_eq!(
        response["result"]["content"][1]["annotations"],
        json!({"audience":["user"]})
    );
    call(&events, name, "call-1", json!({}));
    assert!(tokio::time::timeout(Duration::from_millis(60), rx.recv())
        .await
        .is_err());
    for (id, args, expected) in [
        (
            "protocol",
            json!({"protocol":true}),
            "protocol error -32000",
        ),
        ("transport", json!({"transport":true}), "backend failed"),
        ("mrtr", json!({"mrtr":true}), "input_required"),
    ] {
        call(&events, name, id, args);
        let response = next(&mut rx).await;
        assert!(response["error"]["message"]
            .as_str()
            .unwrap()
            .contains(expected));
        assert!(!response.to_string().contains("secret"));
    }
    {
        let requests = fake.requests.lock().unwrap();
        assert_eq!(requests.len(), 5); // one discovery, four calls, no MRTR replay
        assert_eq!(requests[0].method, "tools/list");
        for op in requests.iter() {
            assert_eq!(op.scope, "one");
            assert!(op.cwd.is_absolute());
            assert_eq!(op.server_id, "registration-1");
            assert_eq!(
                op.params["_meta"]["io.modelcontextprotocol/protocolVersion"],
                "2026-07-28"
            );
            assert_eq!(
                op.params["_meta"]["caller-context"]["principal"],
                "authorized"
            );
        }
        let ids: std::collections::HashSet<_> = requests.iter().map(|op| &op.request_id).collect();
        assert_eq!(ids.len(), requests.len());
    }
    binding.shutdown().await.unwrap();
    assert_eq!(next(&mut rx).await["tools"], json!([]));
    assert_eq!(fake.stopped.load(Ordering::SeqCst), 1);
    host.terminate().await;
}

#[tokio::test]
async fn cancellation_is_scoped_and_shutdown_joins() {
    let fake = Arc::new(Fake::default());
    let other = Arc::new(Fake::default());
    let (host, events, mut rx) = mock();
    let (other_host, other_events, mut other_rx) = mock();
    let mut binding = external::attach(host.clone(), config("one"), external(fake.clone()))
        .await
        .unwrap();
    let mut sibling = external::attach(other_host.clone(), config("two"), external(other.clone()))
        .await
        .unwrap();
    let name = next(&mut rx).await["tools"][0]["name"]
        .as_str()
        .unwrap()
        .to_string();
    next(&mut other_rx).await;
    call(&events, &name, "same", json!({"hang":true}));
    fake.started.notified().await;
    events
        .send(json!({"type":"external_tool_request_closed","requestId":"same"}))
        .unwrap();
    call(&other_events, &name, "same", json!({}));
    assert_eq!(
        next(&mut other_rx).await["result"]["structuredContent"]["scope"],
        "two"
    );
    tokio::time::timeout(Duration::from_secs(2), async {
        while fake.cancelled.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(rx.try_recv().is_err()); // closed requests get no late response
    call(&events, &name, "shutdown", json!({"hang":true}));
    fake.started.notified().await;
    tokio::time::timeout(Duration::from_secs(2), binding.shutdown())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(fake.cancelled.load(Ordering::SeqCst), 2);
    assert_eq!(fake.stopped.load(Ordering::SeqCst), 1);
    assert_ne!(
        fake.requests.lock().unwrap()[0].request_id,
        other.requests.lock().unwrap()[0].request_id
    );
    sibling.shutdown().await.unwrap();
    host.terminate().await;
    other_host.terminate().await;
}

#[tokio::test]
async fn mixed_native_fixture_and_external_use_one_configuration() {
    let fake = Arc::new(Fake::default());
    let (host, events, mut rx) = mock();
    let mut cfg = config("mixed");
    cfg.servers.insert(
        "native".into(),
        McpServerConfig::Stdio(StdioServer {
            command: env!("CARGO_BIN_EXE_rpi-extcore-mcp-fixture").into(),
            args: vec![],
            env: BTreeMap::new(),
            cwd: None,
            enabled: true,
            timeout: 10,
            exposure: Exposure::Direct,
        }),
    );
    let mut binding = external::attach(host.clone(), cfg, external(fake.clone()))
        .await
        .unwrap();
    assert_eq!(next(&mut rx).await["tools"].as_array().unwrap().len(), 2);
    for (index, tool) in binding.descriptors().iter().enumerate() {
        call(
            &events,
            &tool.native_name,
            &format!("call-{index}"),
            json!({}),
        );
        assert!(next(&mut rx).await["result"]["content"].is_array());
    }
    assert_eq!(fake.requests.lock().unwrap().len(), 2);
    binding.shutdown().await.unwrap();
    host.terminate().await;
}

#[tokio::test]
async fn invalid_version_and_namespace_rejected_without_requests() {
    for collision in [false, true] {
        let fake = Arc::new(Fake::default());
        let (host, _, mut rx) = mock();
        let mut cfg = config("validation");
        let mut ext = external(fake.clone());
        if collision {
            ext.servers.insert("".into(), "bad".into());
        } else {
            ext.metadata.insert(
                "io.modelcontextprotocol/protocolVersion".into(),
                json!("2025-03-26"),
            );
        }
        cfg.discovery_timeout = Duration::from_secs(1);
        assert!(external::attach(host.clone(), cfg, ext).await.is_err());
        assert!(fake.requests.lock().unwrap().is_empty());
        assert_eq!(fake.stopped.load(Ordering::SeqCst), 1);
        assert!(rx.try_recv().is_err());
        host.terminate().await;
    }
}

#[test]
fn typed_mrtr_and_protocol_error_preserve_opaque_fields() {
    let result = json!({"resultType":"input_required","requestState":"opaque:[1,2]","inputRequests":{"key":{"method":"roots/list","unknown":null}},"extension":null});
    let typed: ToolResult = serde_json::from_value(result.clone()).unwrap();
    assert!(matches!(typed, ToolResult::InputRequired { .. }));
    assert_eq!(serde_json::to_value(typed).unwrap(), result);
    for data in [None, Some(Value::Null)] {
        let error = ProtocolError {
            code: -32000,
            message: "inner".into(),
            data,
            extensions: BTreeMap::from([("unknown".into(), json!([1]))]),
        };
        assert_eq!(
            serde_json::from_value::<ProtocolError>(serde_json::to_value(&error).unwrap()).unwrap(),
            error
        );
    }
}

#[test]
fn validated_input_boundary_types_methods_limits_and_extensions() {
    use external::InputRequired;
    let valid = json!({"resultType":"input_required","inputRequests":{"same":{"method":"elicitation/create","params":{"mode":"form","extension":null},"unknown":[1]}},"requestState":"not:decoded","extension":null});
    for method in ["tools/call", "resources/read", "prompts/get"] {
        let input = InputRequired::from_value(method, valid.clone()).unwrap();
        assert_eq!(serde_json::to_value(&input).unwrap(), valid);
        assert_eq!(
            input.input_requests().unwrap()["same"].extensions["unknown"],
            json!([1])
        );
    }
    for value in [
        json!({"resultType":"input_required"}),
        json!({"resultType":"input_required","inputRequests":[]}),
        json!({"resultType":"input_required","inputRequests":null}),
        json!({"resultType":"input_required","requestState":{}}),
        json!({"resultType":"input_required","requestState":null}),
        json!({"resultType":"input_required","inputRequests":{"x":{"method":"unknown"}}}),
        json!({"resultType":"input_required","inputRequests":{"x":{"method":"roots/list","params":null}}}),
        json!({"resultType":"input_required","requestState":"x".repeat(external::MAX_INPUT_BYTES)}),
    ] {
        assert!(Outcome::from_result("tools/call", value).is_err());
    }
    for method in ["tools/list", "prompts/list", "resources/list", "unknown"] {
        assert!(Outcome::from_result(method, valid.clone()).is_err());
    }
    assert_eq!(
        Outcome::from_result("tools/call", Value::Null).unwrap(),
        Outcome::Result(Value::Null)
    );
    assert!(InputRequired::from_value(
        "tools/call",
        json!({"resultType":"input_required","inputRequests":{}})
    )
    .is_ok());
}

#[derive(Default)]
struct Rounds {
    requests: Mutex<Vec<Operation>>,
    active: Notify,
    cleaned: AtomicUsize,
}
impl Backend for Rounds {
    fn request(
        &self,
        op: Operation,
        token: CancellationToken,
    ) -> BackendFuture<'_, Result<Outcome, String>> {
        Box::pin(async move {
            let round = {
                let mut ops = self.requests.lock().unwrap();
                let n = ops
                    .iter()
                    .filter(|p| {
                        p.native_request_id == op.native_request_id && p.method == op.method
                    })
                    .count();
                ops.push(op.clone());
                n
            };
            if op.method == "tools/list" {
                return Ok(Outcome::Result(
                    json!({"tools":[{"name":"echo","inputSchema":{"type":"object"}}]}),
                ));
            }
            if op.params["arguments"]["retryWait"] == true && round == 1 {
                self.active.notify_one();
                token.cancelled().await;
                self.cleaned.fetch_add(1, Ordering::SeqCst);
                return Err("cancelled and joined".into());
            }
            let result = if op.params["arguments"]["oversize"] == true {
                json!({"resultType":"input_required","requestState":"x".repeat(external::MAX_INPUT_BYTES)})
            } else if op.params["arguments"]["endless"] == true {
                json!({"resultType":"input_required","requestState":"opaque"})
            } else {
                match round {
                    0 => {
                        json!({"resultType":"input_required","inputRequests":{"same":{"method":"elicitation/create","params":{"mode":"form"}}},"requestState":"first","unknown":null})
                    }
                    1 => json!({"resultType":"input_required","requestState":"second"}),
                    2 => {
                        json!({"resultType":"input_required","inputRequests":{"same":{"method":"elicitation/create","params":{"mode":"form"}}}})
                    }
                    _ => json!({"resultType":"complete","content":[],"unknown":null}),
                }
            };
            // Exercise Result(input_required) compatibility and typed branch equally.
            if round % 2 == 0 {
                Ok(Outcome::Result(result))
            } else {
                Outcome::from_result(&op.method, result)
            }
        })
    }
    fn shutdown(&self) -> BackendFuture<'_, Result<(), String>> {
        Box::pin(async { Ok(()) })
    }
}
#[derive(Default)]
struct Policy {
    contexts: Mutex<Vec<external::RetryContext>>,
    wait: bool,
    started: Notify,
    cleaned: AtomicUsize,
}
impl external::InputPolicy for Policy {
    fn capabilities(&self) -> external::InputCapabilities {
        external::InputCapabilities {
            elicitation_form: true,
            ..Default::default()
        }
    }
    fn resolve(
        &self,
        ctx: external::RetryContext,
        input: external::InputRequired,
        token: CancellationToken,
    ) -> BackendFuture<'_, Result<external::InputDecision, String>> {
        Box::pin(async move {
            let id = ctx.native_request_id.clone();
            self.contexts.lock().unwrap().push(ctx);
            if self.wait {
                self.started.notify_one();
                token.cancelled().await;
                self.cleaned.fetch_add(1, Ordering::SeqCst);
                // Even a Retry returned during cleanup must not send anything.
            }
            Ok(external::InputDecision::Retry {
                input_responses: input.input_requests().map(|requests| {
                    requests
                        .keys()
                        .map(|k| {
                            (
                                k.clone(),
                                json!({"action":"accept","content":{"native":id}}),
                            )
                        })
                        .collect()
                }),
            })
        })
    }
}
fn rounds_external(backend: Arc<Rounds>) -> ExternalConfig {
    let mut cfg = external(Arc::new(Fake::default()));
    cfg.backend = backend;
    cfg
}

#[tokio::test]
async fn multiround_latest_state_original_args_fresh_ids_and_concurrent_keys() {
    let backend = Arc::new(Rounds::default());
    let policy = Arc::new(Policy::default());
    let (host, events, mut rx) = mock();
    let mut binding = external::attach_with_options(
        host.clone(),
        config("rounds"),
        rounds_external(backend.clone()),
        external::ExternalOptions {
            input_policy: Some(policy.clone()),
        },
    )
    .await
    .unwrap();
    let name = next(&mut rx).await["tools"][0]["name"]
        .as_str()
        .unwrap()
        .to_string();
    for id in ["a", "b"] {
        call(&events, &name, id, json!({"original":id}));
    }
    for _ in 0..2 {
        assert_eq!(next(&mut rx).await["result"]["unknown"], Value::Null);
    }
    {
        let ops = backend.requests.lock().unwrap();
        let ids: std::collections::HashSet<_> = ops.iter().map(|o| &o.request_id).collect();
        assert_eq!(ids.len(), ops.len());
        for id in ["a", "b"] {
            let calls: Vec<_> = ops
                .iter()
                .filter(|o| o.native_request_id.as_deref() == Some(id))
                .collect();
            assert_eq!(calls.len(), 4);
            for op in &calls {
                assert_eq!(op.params["arguments"], json!({"original":id}));
                assert_eq!(
                    op.params["_meta"]["io.modelcontextprotocol/clientCapabilities"],
                    json!({"elicitation":{"form":{}}})
                );
            }
            assert!(calls[0].params.get("requestState").is_none());
            assert_eq!(calls[1].params["requestState"], "first");
            assert_eq!(calls[2].params["requestState"], "second");
            assert!(calls[2].params.get("inputResponses").is_none());
            assert!(calls[3].params.get("requestState").is_none());
            for index in [1, 3] {
                assert_eq!(
                    calls[index].params["inputResponses"]["same"],
                    json!({"action":"accept","content":{"native":id}})
                );
            }
        }
        let ctx = policy.contexts.lock().unwrap();
        assert_eq!(ctx.len(), 6);
        assert_ne!(
            ctx.iter()
                .find(|c| c.native_request_id == "a")
                .unwrap()
                .continuation_id,
            ctx.iter()
                .find(|c| c.native_request_id == "b")
                .unwrap()
                .continuation_id
        );
    }
    for (id, args, message) in [
        ("large", json!({"oversize":true}), "byte limit"),
        ("loop", json!({"endless":true}), "round limit"),
    ] {
        call(&events, &name, id, args);
        assert!(next(&mut rx).await["error"]["message"]
            .as_str()
            .unwrap()
            .contains(message));
    }
    binding.shutdown().await.unwrap();
    host.terminate().await;
}

#[tokio::test]
async fn policy_wait_and_retry_cancel_join_without_future_send_sibling_survives() {
    for waiting in [true, false] {
        let backend = Arc::new(Rounds::default());
        let policy = Arc::new(Policy {
            wait: waiting,
            ..Default::default()
        });
        let (host, events, mut rx) = mock();
        let mut binding = external::attach_with_options(
            host.clone(),
            config("cancel"),
            rounds_external(backend.clone()),
            external::ExternalOptions {
                input_policy: Some(policy.clone()),
            },
        )
        .await
        .unwrap();
        let name = next(&mut rx).await["tools"][0]["name"]
            .as_str()
            .unwrap()
            .to_string();
        let (sibling_host, sibling_events, mut sibling_rx) = mock();
        let mut sibling = external::attach(
            sibling_host.clone(),
            config("sibling"),
            external(Arc::new(Fake::default())),
        )
        .await
        .unwrap();
        let sibling_name = next(&mut sibling_rx).await["tools"][0]["name"]
            .as_str()
            .unwrap()
            .to_string();
        for close_scope in [false, true] {
            let id = if close_scope { "shutdown" } else { "closed" };
            call(&events, &name, id, json!({"retryWait":true}));
            if waiting {
                policy.started.notified().await;
            } else {
                backend.active.notified().await;
            }
            if close_scope {
                binding.shutdown().await.unwrap();
            } else {
                events
                    .send(json!({"type":"external_tool_request_closed","requestId":id}))
                    .unwrap();
                tokio::time::timeout(Duration::from_secs(2), async {
                    while (if waiting {
                        policy.cleaned.load(Ordering::SeqCst)
                    } else {
                        backend.cleaned.load(Ordering::SeqCst)
                    }) < 1
                    {
                        tokio::task::yield_now().await;
                    }
                })
                .await
                .unwrap();
            }
        }
        assert_eq!(
            if waiting {
                policy.cleaned.load(Ordering::SeqCst)
            } else {
                backend.cleaned.load(Ordering::SeqCst)
            },
            2
        );
        {
            let ops = backend.requests.lock().unwrap();
            for id in ["closed", "shutdown"] {
                assert_eq!(
                    ops.iter()
                        .filter(|o| o.native_request_id.as_deref() == Some(id))
                        .count(),
                    if waiting { 1 } else { 2 }
                );
            }
        }
        call(&sibling_events, &sibling_name, "still-alive", json!({}));
        assert!(next(&mut sibling_rx).await["result"]["content"].is_array());
        sibling.shutdown().await.unwrap();
        host.terminate().await;
        sibling_host.terminate().await;
    }
}

struct RejectPolicy(u8);
impl external::InputPolicy for RejectPolicy {
    fn capabilities(&self) -> external::InputCapabilities {
        external::InputCapabilities {
            elicitation_form: self.0 != 0,
            ..Default::default()
        }
    }
    fn resolve(
        &self,
        _: external::RetryContext,
        _: external::InputRequired,
        _: CancellationToken,
    ) -> BackendFuture<'_, Result<external::InputDecision, String>> {
        Box::pin(async move {
            Ok(match self.0 {
                0 => panic!("unadvertised input must not reach policy"),
                1 => external::InputDecision::Abandon,
                2 => external::InputDecision::Retry {
                    input_responses: Some(BTreeMap::from([(
                        "wrong".into(),
                        json!({"action":"accept"}),
                    )])),
                },
                _ => external::InputDecision::Retry {
                    input_responses: Some(BTreeMap::from([(
                        "same".into(),
                        json!({"action":"accept","content":"x".repeat(external::MAX_INPUT_BYTES)}),
                    )])),
                },
            })
        })
    }
}
#[tokio::test]
async fn unadvertised_abandon_bad_keys_and_large_responses_never_replay() {
    for (mode, message) in [
        (0, "unsupported"),
        (1, "abandoned"),
        (2, "keys"),
        (3, "byte limit"),
    ] {
        let backend = Arc::new(Rounds::default());
        let (host, events, mut rx) = mock();
        let mut ext = rounds_external(backend.clone());
        // Explicit metadata cannot smuggle capabilities past the real policy.
        ext.metadata.insert(
            "io.modelcontextprotocol/clientCapabilities".into(),
            json!({"sampling":{},"roots":{},"elicitation":{"url":{}}}),
        );
        let mut binding = external::attach_with_options(
            host.clone(),
            config("reject"),
            ext,
            external::ExternalOptions {
                input_policy: Some(Arc::new(RejectPolicy(mode))),
            },
        )
        .await
        .unwrap();
        let name = next(&mut rx).await["tools"][0]["name"]
            .as_str()
            .unwrap()
            .to_string();
        call(&events, &name, "reject", json!({}));
        assert!(next(&mut rx).await["error"]["message"]
            .as_str()
            .unwrap()
            .contains(message));
        {
            let ops = backend.requests.lock().unwrap();
            assert_eq!(ops.len(), 2);
            assert_eq!(
                ops[0].params["_meta"]["io.modelcontextprotocol/clientCapabilities"],
                if mode == 0 {
                    json!({})
                } else {
                    json!({"elicitation":{"form":{}}})
                }
            );
        }
        binding.shutdown().await.unwrap();
        host.terminate().await;
    }
}
