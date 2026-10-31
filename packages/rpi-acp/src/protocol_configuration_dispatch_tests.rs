//! End-to-end configuration regressions through the production ACP dispatcher.
//! Module wiring and the connection-scoped new-session opener live in protocol.rs.
use super::*;
use rpi_extcore::host::HostSession;
use rpi_plugin_sdk::stdio::InputLine;
use std::sync::Mutex as StdMutex;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, BufReader, DuplexStream, Lines};
use tokio::sync::mpsc;

const SESSION_ID: &str = "c4fd32dc-f868-4e1b-88c4-7e9b99abbb14";

struct NativeState {
    model: String,
    thinking: String,
    reject_history: bool,
    commands: Vec<String>,
    provider: Option<String>,
    models: Value,
    model_requests: Vec<Value>,
}

/// The peer implements real native command correlation, including configuration
/// readback and bounded history pages. No adapter setter/forwarder is mocked.
struct NativeFixture {
    session: Arc<Session>,
    state: Arc<StdMutex<NativeState>>,
    worker: tokio::task::JoinHandle<()>,
}
impl NativeFixture {
    async fn new(id: String) -> Self {
        Self::with_models(
            id,
            "a",
            Some("p"),
            json!([
                {"id":"a","provider":"p","name":"A","contextWindow":123},
                {"id":"org/b","provider":"p","name":"B","contextWindow":456}
            ]),
        )
        .await
    }

    async fn with_models(id: String, model: &str, provider: Option<&str>, models: Value) -> Self {
        let (writer, reader) = tokio::io::duplex(65536);
        let (host, events) = HostSession::test_connection_with_ready(
            writer,
            64,
            json!({"type":"ready","protocol":"rpi","supportedCommands":[
                "get_state", "get_available_models", "get_available_thinking_levels",
                "set_model", "set_thinking_level", "get_messages"
            ]}),
        );
        let state = Arc::new(StdMutex::new(NativeState {
            model: model.into(),
            thinking: "off".into(),
            reject_history: false,
            commands: Vec::new(),
            provider: provider.map(str::to_owned),
            models,
            model_requests: Vec::new(),
        }));
        let shared = state.clone();
        let worker = tokio::spawn(async move {
            let mut lines = BufReader::new(reader).lines();
            while let Some(line) = lines.next_line().await.unwrap() {
                let request: Value = serde_json::from_str(&line).unwrap();
                let response = {
                    let mut state = shared.lock().unwrap();
                    let command = request["type"].as_str().unwrap();
                    state.commands.push(command.into());
                    if command == "get_messages" && state.reject_history {
                        json!({"type":"response","id":request["id"],"success":false,
                            "error":"fixture history rejected"})
                    } else {
                        let data = match command {
                            "get_state" => {
                                let mut data = json!({"sessionId":id,"modelId":state.model,
                                    "thinkingLevel":state.thinking,"leafId":"entry"});
                                if let Some(provider) = &state.provider {
                                    data["provider"] = json!(provider);
                                }
                                data
                            }
                            "get_available_models" => json!({"models":state.models}),
                            "get_available_thinking_levels" => json!({"levels":["off","high"]}),
                            "set_model" => {
                                assert_eq!(request["model"], request["modelId"]);
                                assert!(
                                    state.models.as_array().unwrap().iter().any(|model| {
                                        model["id"] == request["modelId"]
                                            && model["provider"] == request["provider"]
                                    }),
                                    "setter must preserve an exact catalog identity: {request}"
                                );
                                state.model_requests.push(request.clone());
                                state.provider = request["provider"].as_str().map(str::to_owned);
                                state.model = request["modelId"].as_str().unwrap().into();
                                json!({})
                            }
                            "set_thinking_level" => {
                                state.thinking = request["level"].as_str().unwrap().into();
                                json!({})
                            }
                            "get_messages" => json!({
                                "messages":[{"entryId":"entry","role":"user","content":"replayed history"}],
                                "total":1,"leafId":"entry","firstId":"entry","lastId":"entry",
                                "beforeId":request["beforeId"],"hasMore":false
                            }),
                            other => panic!("unexpected native command: {other}"),
                        };
                        json!({"type":"response","id":request["id"],"success":true,"data":data})
                    }
                };
                if events.send(response).is_err() {
                    break;
                }
            }
        });
        let session = Session::attach_for_test(host).await.unwrap();
        Self {
            session,
            state,
            worker,
        }
    }

    async fn change(&self, model: &str, thinking: &str) {
        {
            let mut state = self.state.lock().unwrap();
            state.model = model.into();
            state.thinking = thinking.into();
        }
        self.session
            .configuration()
            .await
            .unwrap()
            .unwrap()
            .refresh()
            .await
            .unwrap();
    }
}
impl Drop for NativeFixture {
    fn drop(&mut self) {
        self.worker.abort();
    }
}

struct Client {
    input: mpsc::UnboundedSender<String>,
    output: Lines<BufReader<DuplexStream>>,
}
impl Client {
    fn request(&self, id: u64, method: &str, params: Value) {
        self.input
            .send(json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}).to_string())
            .unwrap();
    }

    async fn frame(&mut self) -> Value {
        let line = tokio::time::timeout(Duration::from_secs(2), self.output.next_line())
            .await
            .expect("dispatcher output timed out")
            .unwrap()
            .expect("dispatcher output closed");
        serde_json::from_str(&line).unwrap()
    }

    async fn quiet(&mut self) {
        assert!(
            tokio::time::timeout(Duration::from_millis(40), self.output.next_line())
                .await
                .is_err(),
            "unexpected duplicate or late dispatcher frame"
        );
    }
}

fn transport() -> (Client, Arc<Writer>, mpsc::UnboundedReceiver<String>) {
    let (output, peer) = tokio::io::duplex(65536);
    let (input, requests) = mpsc::unbounded_channel();
    (
        Client {
            input,
            output: BufReader::new(peer).lines(),
        },
        Arc::new(Writer::with_output(output)),
        requests,
    )
}

fn poll(requests: &mut mpsc::UnboundedReceiver<String>) -> std::io::Result<InputLine> {
    Ok(match requests.try_recv() {
        Ok(line) => InputLine::Line(line),
        Err(mpsc::error::TryRecvError::Empty) => InputLine::Pending,
        Err(mpsc::error::TryRecvError::Disconnected) => InputLine::Eof,
    })
}

fn lifecycle_params(id: &str) -> Value {
    json!({"sessionId":id,"cwd":std::env::current_dir().unwrap()})
}

fn assert_options(options: &Value, model: &str, thinking: &str) {
    let options = options
        .as_array()
        .expect("complete configuration option list");
    assert_eq!(options.len(), 2);
    assert_eq!(options[0]["id"], "model");
    assert_eq!(options[0]["currentValue"], format!("p/{model}"));
    assert_eq!(options[0]["options"].as_array().unwrap().len(), 2);
    assert_eq!(options[1]["id"], "thinking");
    assert_eq!(options[1]["currentValue"], thinking);
    assert_eq!(options[1]["options"].as_array().unwrap().len(), 2);
}

fn assert_response(frame: &Value, id: u64, model: &str, thinking: &str) {
    assert_eq!(
        frame["id"], id,
        "expected lifecycle/set response before any config notification: {frame}"
    );
    assert!(frame.get("error").is_none(), "{frame}");
    assert_options(&frame["result"]["configOptions"], model, thinking);
}

fn assert_update(frame: &Value, id: &str, model: &str, thinking: &str) {
    assert!(frame.get("id").is_none(), "{frame}");
    assert_eq!(frame["method"], "session/update");
    assert_eq!(frame["params"]["sessionId"], id);
    assert_eq!(
        frame["params"]["update"]["sessionUpdate"],
        "config_option_update"
    );
    assert_options(&frame["params"]["update"]["configOptions"], model, thinking);
}

#[tokio::test]
async fn dispatcher_load_resume_rebase_and_keep_live_updates_without_prompt() {
    let fixture = NativeFixture::new(SESSION_ID.into()).await;
    let sessions = Arc::new(Mutex::new(HashMap::from([(
        SESSION_ID.into(),
        fixture.session.clone(),
    )])));
    let (mut client, writer, mut requests) = transport();
    let (stop_tx, stop) = watch::channel(false);
    let server = run_connection_with_sessions(
        stop,
        || poll(&mut requests),
        None,
        writer,
        Err("unused session list root".into()),
        sessions,
    );
    let scenario = async {
        for (id, method) in [
            (1, "session/load"),
            (2, "session/resume"),
            (3, "session/load"),
        ] {
            client.request(id, method, lifecycle_params(SESSION_ID));
            if method == "session/load" {
                let replay = client.frame().await;
                assert_eq!(
                    replay["params"]["update"]["sessionUpdate"],
                    "user_message_chunk"
                );
                assert_eq!(
                    replay["params"]["update"]["content"]["text"],
                    "replayed history"
                );
            }
            let model = if id == 2 { "org/b" } else { "a" };
            let thinking = if id == 2 { "high" } else { "off" };
            assert_response(&client.frame().await, id, model, thinking);
            client.quiet().await;
            let (next_model, next_thinking) = if id == 2 {
                ("a", "off")
            } else {
                ("org/b", "high")
            };
            fixture.change(next_model, next_thinking).await;
            assert_update(&client.frame().await, SESSION_ID, next_model, next_thinking);
            client.quiet().await;
        }
        stop_tx.send_replace(true);
    };
    let (exit, ()) = tokio::time::timeout(Duration::from_secs(8), async {
        tokio::join!(server, scenario)
    })
    .await
    .unwrap();
    assert_eq!(exit, 0);
}

#[tokio::test]
async fn dispatcher_real_setters_read_back_full_state_and_do_not_echo_stale_updates() {
    let fixture = NativeFixture::new(SESSION_ID.into()).await;
    let sessions = Arc::new(Mutex::new(HashMap::from([(
        SESSION_ID.into(),
        fixture.session.clone(),
    )])));
    let (mut client, writer, mut requests) = transport();
    let (stop_tx, stop) = watch::channel(false);
    let server = run_connection_with_sessions(
        stop,
        || poll(&mut requests),
        None,
        writer,
        Err("unused session list root".into()),
        sessions,
    );
    let scenario = async {
        client.request(1, "session/resume", lifecycle_params(SESSION_ID));
        assert_response(&client.frame().await, 1, "a", "off");
        for (id, config, value, model, thinking) in [
            (2, "model", "p/org/b", "org/b", "off"),
            (3, "thinking", "high", "org/b", "high"),
        ] {
            client.request(
                id,
                "session/set_config_option",
                json!({"sessionId":SESSION_ID,"configId":config,"value":value}),
            );
            assert_response(&client.frame().await, id, model, thinking);
            client.quiet().await;
        }
        let commands = fixture.state.lock().unwrap().commands.clone();
        assert_eq!(
            commands
                .iter()
                .filter(|c| c.as_str() == "set_model")
                .count(),
            1
        );
        assert_eq!(
            commands
                .iter()
                .filter(|c| c.as_str() == "set_thinking_level")
                .count(),
            1
        );
        fixture.change("a", "off").await;
        assert_update(&client.frame().await, SESSION_ID, "a", "off");
        client.quiet().await;
        stop_tx.send_replace(true);
    };
    let (exit, ()) = tokio::time::timeout(Duration::from_secs(8), async {
        tokio::join!(server, scenario)
    })
    .await
    .unwrap();
    assert_eq!(exit, 0);
}

#[tokio::test]
async fn dispatcher_providerless_slash_setter_preserves_catalog_identity() {
    check_catalog_setter(false).await;
}

#[tokio::test]
async fn dispatcher_ambiguous_model_value_is_hidden_and_rejected_before_command() {
    check_catalog_setter(true).await;
}

async fn check_catalog_setter(collision: bool) {
    let mut models = json!([
        {"id":"org/a","name":"A"},
        {"id":"org/b","name":"B"}
    ]);
    if collision {
        models
            .as_array_mut()
            .unwrap()
            .push(json!({"id":"b","provider":"org","name":"Qualified B"}));
    }
    let fixture = NativeFixture::with_models(SESSION_ID.into(), "org/a", None, models).await;
    let sessions = Arc::new(Mutex::new(HashMap::from([(
        SESSION_ID.into(),
        fixture.session.clone(),
    )])));
    let (mut client, writer, mut requests) = transport();
    let (stop_tx, stop) = watch::channel(false);
    let server = run_connection_with_sessions(
        stop,
        || poll(&mut requests),
        None,
        writer,
        Err("unused session list root".into()),
        sessions,
    );
    let scenario = async {
        client.request(1, "session/resume", lifecycle_params(SESSION_ID));
        let response = client.frame().await;
        assert_eq!(response["id"], 1);
        assert!(response.get("error").is_none(), "{response}");
        let options = response["result"]["configOptions"].as_array().unwrap();
        if collision {
            assert!(options.iter().all(|option| option["id"] != "model"));
            assert_eq!(options.len(), 1, "thinking must remain available");
        } else {
            assert_eq!(options[0]["currentValue"], "org/a");
            assert_eq!(options[0]["options"][1]["value"], "org/b");
        }
        // Unknown selectors must never fall through to lexical splitting either.
        client.request(
            2,
            "session/set_config_option",
            json!({
                "sessionId":SESSION_ID,"configId":"model","value":"unknown/model"
            }),
        );
        let unknown = client.frame().await;
        assert_eq!(unknown["id"], 2);
        assert!(unknown.get("error").is_some(), "{unknown}");
        assert!(fixture.state.lock().unwrap().model_requests.is_empty());

        client.request(
            3,
            "session/set_config_option",
            json!({
                "sessionId":SESSION_ID,"configId":"model","value":"org/b"
            }),
        );
        let response = client.frame().await;
        assert_eq!(response["id"], 3);
        if collision {
            assert!(response.get("error").is_some(), "{response}");
            assert!(fixture.state.lock().unwrap().model_requests.is_empty());
            assert_eq!(fixture.state.lock().unwrap().model, "org/a");
        } else {
            assert!(response.get("error").is_none(), "{response}");
            assert_eq!(
                response["result"]["configOptions"][0]["currentValue"],
                "org/b"
            );
            let requests = fixture.state.lock().unwrap().model_requests.clone();
            assert_eq!(requests.len(), 1);
            assert_eq!(requests[0]["modelId"], "org/b");
            assert_eq!(requests[0]["model"], "org/b");
            assert!(requests[0].get("provider").is_none(), "{requests:?}");
        }
        client.quiet().await;
        stop_tx.send_replace(true);
    };
    let (exit, ()) = tokio::time::timeout(Duration::from_secs(8), async {
        tokio::join!(server, scenario)
    })
    .await
    .unwrap();
    assert_eq!(exit, 0);
}

#[tokio::test]
async fn dispatcher_failed_load_preserves_existing_configuration_observer() {
    const SESSION_ID: &str = "22977269-be8f-4e5b-bac4-82e0331d0699";
    let fixture = NativeFixture::new(SESSION_ID.into()).await;
    let sessions = Arc::new(Mutex::new(HashMap::from([(
        SESSION_ID.into(),
        fixture.session.clone(),
    )])));
    let (mut client, writer, mut requests) = transport();
    let (stop_tx, stop) = watch::channel(false);
    let server = run_connection_with_sessions(
        stop,
        || poll(&mut requests),
        None,
        writer,
        Err("unused session list root".into()),
        sessions,
    );
    let scenario = async {
        client.request(1, "session/resume", lifecycle_params(SESSION_ID));
        assert_response(&client.frame().await, 1, "a", "off");
        // Fail only after replay preparation succeeds. This is not the earlier
        // get_messages/preflight failure: the existing observer must survive emit.
        *crate::history::FAIL_REPLAY_EMIT_FOR.lock().unwrap() = Some(SESSION_ID.into());
        client.request(2, "session/load", lifecycle_params(SESSION_ID));
        let error = client.frame().await;
        assert_eq!(error["id"], 2);
        assert!(error.get("error").is_some(), "{error}");
        assert!(
            crate::history::FAIL_REPLAY_EMIT_FOR
                .lock()
                .unwrap()
                .is_none(),
            "fault must be reached in emit, not bypassed by failed preparation"
        );
        fixture.change("org/b", "high").await;
        assert_update(&client.frame().await, SESSION_ID, "org/b", "high");
        client.quiet().await;
        stop_tx.send_replace(true);
    };
    let (exit, ()) = tokio::time::timeout(Duration::from_secs(8), async {
        tokio::join!(server, scenario)
    })
    .await
    .unwrap();
    assert_eq!(exit, 0);
}

#[tokio::test]
async fn dispatcher_new_response_precedes_live_configuration_and_close_stops_observer() {
    let sessions = Arc::new(Mutex::new(HashMap::new()));
    let (mut client, writer, mut requests) = transport();
    let (stop_tx, stop) = watch::channel(false);
    let (opened_tx, mut opened_rx) = mpsc::unbounded_channel();
    // Required test-only hook: same production dispatcher, replacing only the
    // new-session opener. The generated UUID is passed into the real host fixture.
    let server = run_connection_with_test_opener(
        stop,
        || poll(&mut requests),
        None,
        writer,
        Err("unused session list root".into()),
        sessions.clone(),
        move |id: String, _cwd: PathBuf| {
            let opened_tx = opened_tx.clone();
            let future: std::pin::Pin<
                Box<dyn std::future::Future<Output = Result<(Arc<Session>, Value), String>> + Send>,
            > = Box::pin(async move {
                let fixture = NativeFixture::new(id).await;
                let state = fixture.session.state().await?;
                let session = fixture.session.clone();
                opened_tx
                    .send(fixture)
                    .unwrap_or_else(|_| panic!("test client closed"));
                Ok((session, state))
            });
            future
        },
    );
    let scenario = async {
        client.request(
            1,
            "session/new",
            json!({"cwd":std::env::current_dir().unwrap(),"mcpServers":[]}),
        );
        let fixture = opened_rx.recv().await.unwrap();
        let response = client.frame().await;
        assert_response(&response, 1, "a", "off");
        let id = response["result"]["sessionId"].as_str().unwrap().to_owned();
        assert!(uuid::Uuid::parse_str(&id).is_ok());
        assert!(Arc::ptr_eq(
            sessions.lock().await.get(&id).unwrap(),
            &fixture.session
        ));
        fixture.change("org/b", "high").await;
        assert_update(&client.frame().await, &id, "org/b", "high");
        client.quiet().await;
        client.request(2, "session/close", json!({"sessionId":id}));
        let closed = client.frame().await;
        assert_eq!(closed["id"], 2);
        assert!(closed.get("error").is_none(), "{closed}");
        assert!(!sessions.lock().await.contains_key(&id));
        client.quiet().await;
        stop_tx.send_replace(true);
    };
    let (exit, ()) = tokio::time::timeout(Duration::from_secs(8), async {
        tokio::join!(server, scenario)
    })
    .await
    .unwrap();
    assert_eq!(exit, 0);
}
