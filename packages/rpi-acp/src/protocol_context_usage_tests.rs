//! Production-dispatch tests with a correlated in-memory native peer.
use super::*;
use rpi_extcore::host::HostSession;
use rpi_plugin_sdk::stdio::InputLine;
use std::sync::Mutex as StdMutex;
use std::time::Duration;
use tokio::sync::mpsc;

struct Native {
    session: Arc<Session>,
    data: Arc<StdMutex<Value>>,
    commands: Arc<StdMutex<Vec<Value>>>,
    worker: tokio::task::JoinHandle<()>,
}
impl Native {
    async fn new(id: String, supports_state: bool) -> Self {
        let (out, peer) = tokio::io::duplex(65536);
        let commands = Arc::new(StdMutex::new(Vec::new()));
        let data = Arc::new(StdMutex::new(json!({"context":{"tokens":10,"window":100},
            "sessionId":id,"leafId":"leaf"})));
        let supported = if supports_state {
            json!(["get_state", "get_messages", "compact"])
        } else {
            json!(["compact"])
        };
        let (host, events) = HostSession::test_connection_with_ready(
            out,
            64,
            json!({"type":"ready","protocol":"rpi","supportedCommands":supported}),
        );
        let capture = commands.clone();
        let state = data.clone();
        let worker = tokio::spawn(async move {
            let mut lines = BufReader::new(peer).lines();
            while let Some(line) = lines.next_line().await.unwrap() {
                let request: Value = serde_json::from_str(&line).unwrap();
                capture.lock().unwrap().push(request.clone());
                let command = request["type"].as_str().unwrap();
                let snapshot = state.lock().unwrap().clone();
                if command == "get_state" && snapshot["noResponse"] == true {
                    continue;
                }
                if matches!(command, "prompt" | "compact") && snapshot["rejectRun"] == true {
                    let _ = events.send(json!({"type":"response","id":request["id"],
                        "success":false,"error":"fixture rejection"}));
                    continue;
                }
                let response = match command {
                    "get_state" => snapshot.clone(),
                    "get_messages" => {
                        json!({"messages":[],"total":0,"leafId":"leaf", "hasMore":false,
                        "firstId":null,"lastId":null,"beforeId":request["beforeId"]})
                    }
                    "prompt" | "compact" => snapshot
                        .get("ack")
                        .cloned()
                        .unwrap_or_else(|| json!({"accepted":true,"runId":"run"})),
                    "abort" => json!({}),
                    // Legacy config discovery remains unchanged and fails fast.
                    _ => {
                        let _ = events.send(json!({"type":"response","id":request["id"],"success":false,"error":"unsupported"}));
                        continue;
                    }
                };
                if events.send(json!({"type":"response","id":request["id"],"success":true,"data":response})).is_err() { break; }
                if matches!(command, "prompt" | "compact") && snapshot.get("ack").is_some() {
                    continue;
                }
                if matches!(command, "prompt" | "compact") && snapshot["holdRun"] != true {
                    let outcome = snapshot["outcome"]
                        .as_str()
                        .unwrap_or(if command == "compact" {
                            "compacted"
                        } else {
                            "completed"
                        });
                    let _ = events
                        .send(json!({"type":"agent_settled","runId":"run","outcome":outcome}));
                }
                if command == "abort" {
                    let _ = events
                        .send(json!({"type":"agent_settled","runId":"run","outcome":"aborted"}));
                }
            }
        });
        Self {
            session: Session::attach_for_test(host).await.unwrap(),
            data,
            commands,
            worker,
        }
    }
    fn types(&self) -> Vec<String> {
        self.commands
            .lock()
            .unwrap()
            .iter()
            .map(|v| v["type"].as_str().unwrap().to_owned())
            .collect()
    }
}
impl Drop for Native {
    fn drop(&mut self) {
        self.worker.abort();
    }
}

fn poll_usage(rx: &mut mpsc::UnboundedReceiver<String>) -> std::io::Result<InputLine> {
    Ok(match rx.try_recv() {
        Ok(line) => InputLine::Line(line),
        Err(mpsc::error::TryRecvError::Empty) => InputLine::Pending,
        Err(_) => InputLine::Eof,
    })
}
fn send_usage(tx: &mpsc::UnboundedSender<String>, id: u64, method: &str, params: Value) {
    tx.send(json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}).to_string())
        .unwrap();
}
async fn usage_frame(lines: &mut tokio::io::Lines<BufReader<tokio::io::DuplexStream>>) -> Value {
    serde_json::from_str(
        &tokio::time::timeout(Duration::from_secs(8), lines.next_line())
            .await
            .unwrap()
            .unwrap()
            .unwrap(),
    )
    .unwrap()
}
fn assert_usage(frame: &Value, id: &str, used: u64) {
    assert_eq!(
        *frame,
        json!({"jsonrpc":"2.0","method":"session/update","params":{
        "sessionId":id,"update":{"sessionUpdate":"usage_update","used":used,"size":100}}})
    );
}

#[tokio::test]
async fn dispatcher_usage_new_load_resume_prompt_compact_exact_commands_and_order() {
    let (out, peer) = tokio::io::duplex(65536);
    let writer = Arc::new(Writer::with_output(out));
    let mut lines = BufReader::new(peer).lines();
    let (tx, mut rx) = mpsc::unbounded_channel();
    let fixtures = Arc::new(StdMutex::new(Vec::<Native>::new()));
    let shared = fixtures.clone();
    let sessions = Arc::new(Mutex::new(HashMap::new()));
    let (stop_tx, stop) = watch::channel(false);
    let server = run_connection_with_test_opener(
        stop,
        || poll_usage(&mut rx),
        None,
        writer,
        Err("unused".into()),
        sessions,
        move |id, _cwd| {
            let shared = shared.clone();
            async move {
                let fixture = Native::new(id, true).await;
                let session = fixture.session.clone();
                let state = session.state().await?;
                shared.lock().unwrap().push(fixture);
                Ok((session, state))
            }
        },
    );
    let scenario = async {
        let cwd = std::env::current_dir().unwrap();
        send_usage(&tx, 1, "session/new", json!({"cwd":cwd}));
        let response = usage_frame(&mut lines).await;
        assert_eq!(response["id"], 1);
        let id = response["result"]["sessionId"].as_str().unwrap().to_owned();
        assert_eq!(
            usage_frame(&mut lines).await["params"]["update"]["sessionUpdate"],
            "available_commands_update"
        );
        assert_usage(&usage_frame(&mut lines).await, &id, 10);
        for (request, method) in [
            (2, "session/load"),
            (3, "session/resume"),
            (4, "session/resume"),
        ] {
            send_usage(&tx, request, method, json!({"sessionId":id,"cwd":cwd}));
            assert_eq!(usage_frame(&mut lines).await["id"], request);
            assert_eq!(
                usage_frame(&mut lines).await["params"]["update"]["sessionUpdate"],
                "available_commands_update"
            );
            assert_usage(&usage_frame(&mut lines).await, &id, 10);
        }
        for (request, prompt, used) in [(5, "hello", 20), (6, "/compact", 5)] {
            fixtures.lock().unwrap()[0].data.lock().unwrap()["context"]["tokens"] = json!(used);
            send_usage(
                &tx,
                request,
                "session/prompt",
                json!({"sessionId":id,"prompt":[{"type":"text","text":prompt}]}),
            );
            assert_usage(&usage_frame(&mut lines).await, &id, used);
            let response = usage_frame(&mut lines).await;
            assert_eq!(response["id"], request);
            assert_eq!(response["result"]["stopReason"], "end_turn");
        }
        let commands = fixtures.lock().unwrap()[0].types();
        assert_eq!(
            commands,
            vec![
                "get_state",
                "get_available_models",
                "get_state",
                "get_messages",
                "get_messages",
                "get_available_models",
                "get_state",
                "get_available_models",
                "get_state",
                "get_available_models",
                "prompt",
                "get_state",
                "compact",
                "get_state"
            ]
        );
        // EOF cleans up the live native and all dispatcher-owned tasks.
        drop(tx);
    };
    let (exit, ()) = tokio::time::timeout(Duration::from_secs(15), async {
        tokio::join!(server, scenario)
    })
    .await
    .unwrap();
    assert_eq!(exit, 0);
    drop(stop_tx);
}

#[tokio::test]
async fn dispatcher_usage_unknown_malformed_timeout_and_failed_settlement_preserve_result() {
    for context in [
        Value::Null,
        json!({"tokens":"bad","window":100}),
        json!({"tokens":0,"window":0}),
        json!({"tokens":22,"window":100}),
    ] {
        let id = "d304b7c4-ae49-41bc-97e8-f9aec9c0ed57";
        let fixture = Native::new(id.into(), true).await;
        let (out, peer) = tokio::io::duplex(65536);
        let writer = Arc::new(Writer::with_output(out));
        let mut lines = BufReader::new(peer).lines();
        let (tx, mut rx) = mpsc::unbounded_channel();
        let sessions = Arc::new(Mutex::new(HashMap::from([(
            id.into(),
            fixture.session.clone(),
        )])));
        let (_stop_tx, stop) = watch::channel(false);
        let server = run_connection_with_sessions(
            stop,
            || poll_usage(&mut rx),
            None,
            writer,
            Err("unused".into()),
            sessions,
        );
        let scenario = async {
            fixture.data.lock().unwrap()["context"] = context.clone();
            // The final iteration with otherwise-valid data times out instead of emitting zero.
            fixture.data.lock().unwrap()["noResponse"] = json!(context["tokens"] == 22);
            send_usage(
                &tx,
                1,
                "session/prompt",
                json!({"sessionId":id,"prompt":[{"type":"text","text":"hello"}]}),
            );
            let response = usage_frame(&mut lines).await;
            assert_eq!(response["id"], 1);
            assert_eq!(response["result"]["stopReason"], "end_turn");
            fixture.data.lock().unwrap()["noResponse"] = json!(false);
            fixture.data.lock().unwrap()["context"] = json!({"tokens":30,"window":100});
            fixture.data.lock().unwrap()["outcome"] = json!("failed");
            send_usage(
                &tx,
                2,
                "session/prompt",
                json!({"sessionId":id,"prompt":[{"type":"text","text":"fail this run"}]}),
            );
            assert_usage(&usage_frame(&mut lines).await, id, 30);
            let response = usage_frame(&mut lines).await;
            assert_eq!(response["id"], 2);
            assert!(response.get("error").is_some());
            assert_eq!(
                fixture.types(),
                vec!["prompt", "get_state", "prompt", "get_state"]
            );
            drop(tx);
        };
        let (exit, ()) = tokio::time::timeout(Duration::from_secs(15), async {
            tokio::join!(server, scenario)
        })
        .await
        .unwrap();
        assert_eq!(exit, 0);
    }
}

#[tokio::test]
async fn dispatcher_usage_cancel_reads_after_settlement_and_unsupported_host_never_queries() {
    for supported in [true, false] {
        let id = "d304b7c4-ae49-41bc-97e8-f9aec9c0ed57";
        let fixture = Native::new(id.into(), supported).await;
        fixture.data.lock().unwrap()["holdRun"] = json!(true);
        let (out, peer) = tokio::io::duplex(65536);
        let mut lines = BufReader::new(peer).lines();
        let (tx, mut rx) = mpsc::unbounded_channel();
        let (_stop_tx, stop) = watch::channel(false);
        let server = run_connection_with_sessions(
            stop,
            || poll_usage(&mut rx),
            None,
            Arc::new(Writer::with_output(out)),
            Err("unused".into()),
            Arc::new(Mutex::new(HashMap::from([(
                id.into(),
                fixture.session.clone(),
            )]))),
        );
        let scenario = async {
            send_usage(
                &tx,
                1,
                "session/prompt",
                json!({"sessionId":id,"prompt":[{"type":"text","text":"cancel this run"}]}),
            );
            tokio::time::timeout(Duration::from_secs(2), async {
                while !fixture.types().iter().any(|v| v == "prompt") {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
            tx.send(
                json!({"jsonrpc":"2.0","method":"$/cancel_request","params":{"requestId":1}})
                    .to_string(),
            )
            .unwrap();
            if supported {
                assert_usage(&usage_frame(&mut lines).await, id, 10);
            }
            let response = usage_frame(&mut lines).await;
            assert_eq!(response["id"], 1);
            assert_eq!(response["result"]["stopReason"], "cancelled");
            let expected = if supported {
                vec!["prompt", "abort", "get_state"]
            } else {
                vec!["prompt", "abort"]
            };
            assert_eq!(fixture.types(), expected);
            drop(tx);
        };
        let (exit, ()) = tokio::time::timeout(Duration::from_secs(10), async {
            tokio::join!(server, scenario)
        })
        .await
        .unwrap();
        assert_eq!(exit, 0);
    }
}

#[tokio::test]
async fn dispatcher_usage_eof_cancels_an_inflight_unanswered_read() {
    let id = "d304b7c4-ae49-41bc-97e8-f9aec9c0ed57";
    let fixture = Native::new(id.into(), true).await;
    fixture.data.lock().unwrap()["noResponse"] = json!(true);
    let (out, _peer) = tokio::io::duplex(65536);
    let writer = Arc::new(Writer::with_output(out));
    let (tx, mut rx) = mpsc::unbounded_channel();
    let (_stop_tx, stop) = watch::channel(false);
    let server = run_connection_with_sessions(
        stop,
        || poll_usage(&mut rx),
        None,
        writer.clone(),
        Err("unused".into()),
        Arc::new(Mutex::new(HashMap::from([(
            id.into(),
            fixture.session.clone(),
        )]))),
    );
    let scenario = async {
        send_usage(
            &tx,
            1,
            "session/prompt",
            json!({"sessionId":id,"prompt":[{"type":"text","text":"settle before eof"}]}),
        );
        tokio::time::timeout(Duration::from_secs(2), async {
            while !fixture.types().iter().any(|v| v == "get_state") {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        // Positively captured request proves this is cleanup during a read,
        // not just EOF before the prompt task had a chance to run.
        assert_eq!(fixture.types(), vec!["prompt", "get_state"]);
        drop(tx);
    };
    let (exit, ()) = tokio::time::timeout(Duration::from_secs(3), async {
        tokio::join!(server, scenario)
    })
    .await
    .unwrap();
    assert_eq!(exit, 0);
    assert_eq!(writer.pending_count(), 0);
}

#[tokio::test]
async fn dispatcher_usage_rejected_or_malformed_acceptance_never_reads_context() {
    for (control, value) in [
        ("rejectRun", json!(true)),
        ("ack", json!({"accepted":false})),
    ] {
        for text in ["reject this run", "/compact"] {
            let id = "d304b7c4-ae49-41bc-97e8-f9aec9c0ed57";
            let fixture = Native::new(id.into(), true).await;
            fixture.data.lock().unwrap()[control] = value.clone();
            let (out, peer) = tokio::io::duplex(65536);
            let mut lines = BufReader::new(peer).lines();
            let (tx, mut rx) = mpsc::unbounded_channel();
            let (_stop_tx, stop) = watch::channel(false);
            let server = run_connection_with_sessions(
                stop,
                || poll_usage(&mut rx),
                None,
                Arc::new(Writer::with_output(out)),
                Err("unused".into()),
                Arc::new(Mutex::new(HashMap::from([(
                    id.into(),
                    fixture.session.clone(),
                )]))),
            );
            let scenario = async {
                send_usage(
                    &tx,
                    1,
                    "session/prompt",
                    json!({"sessionId":id,
                    "prompt":[{"type":"text","text":text}]}),
                );
                let frame = usage_frame(&mut lines).await;
                assert_eq!(frame["id"], 1);
                assert!(frame.get("error").is_some());
                assert_eq!(
                    fixture.types(),
                    vec![if text == "/compact" {
                        "compact"
                    } else {
                        "prompt"
                    }]
                );
                drop(tx);
            };
            let (exit, ()) = tokio::time::timeout(Duration::from_secs(3), async {
                tokio::join!(server, scenario)
            })
            .await
            .unwrap();
            assert_eq!(exit, 0);
        }
    }
}
