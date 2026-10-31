//! Response progress through the production connection owner and dispatcher.
use super::*;
use rpi_extcore::host::HostSession;
use rpi_plugin_sdk::stdio::InputLine;
use std::sync::atomic::{AtomicBool, AtomicUsize};
use std::sync::Mutex as StdMutex;
use std::time::Duration;
use tokio::io::{DuplexStream, Lines};
use tokio::sync::{mpsc, Notify};

fn poll(rx: &mut mpsc::UnboundedReceiver<String>) -> std::io::Result<InputLine> {
    Ok(match rx.try_recv() {
        Ok(line) => InputLine::Line(line),
        Err(mpsc::error::TryRecvError::Empty) => InputLine::Pending,
        Err(mpsc::error::TryRecvError::Disconnected) => InputLine::Eof,
    })
}

fn request(id: u64, method: &str, params: Value) -> String {
    json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}).to_string()
}

async fn frame(lines: &mut Lines<BufReader<DuplexStream>>) -> Value {
    serde_json::from_str(
        &tokio::time::timeout(Duration::from_secs(3), lines.next_line())
            .await
            .expect("dispatcher output stalled")
            .unwrap()
            .expect("dispatcher output closed"),
    )
    .unwrap()
}

async fn until_id(lines: &mut Lines<BufReader<DuplexStream>>, id: u64) -> Value {
    loop {
        let next = frame(lines).await;
        if next["id"] == id {
            return next;
        }
        assert!(next.get("id").is_none(), "unexpected response: {next}");
    }
}

/// Real core FileIOService -> ACP ClientFileIO -> stdin response routing ->
/// native file_io_response, while another session's opener cannot complete.
/// The simulated editor commits before sending its acknowledgement. No direct
/// Writer::resolve call, deadline relaxation, model, or 45-second sleep is used.
#[tokio::test]
async fn dispatcher_resolves_committed_write_before_unrelated_opener_finishes() {
    let (output, peer) = tokio::io::duplex(65536);
    let writer = Arc::new(Writer::with_output(output));
    let mut lines = BufReader::new(peer).lines();
    let (tx, mut rx) = mpsc::unbounded_channel();
    let (native_ack_tx, mut native_ack_rx) = mpsc::unbounded_channel();
    let (stop_tx, stop) = watch::channel(false);
    let sessions = Arc::new(Mutex::new(HashMap::new()));
    let workers = Arc::new(StdMutex::new(Vec::new()));
    let entered_b = Arc::new(Notify::new());
    let release_b = Arc::new(Notify::new());
    let b_finished = Arc::new(AtomicBool::new(false));
    let opens = Arc::new(AtomicUsize::new(0));
    let server = run_connection_with_opener(
        stop,
        || poll(&mut rx),
        None,
        writer.clone(),
        Err("unused".into()),
        sessions.clone(),
        {
            let workers = workers.clone();
            let entered_b = entered_b.clone();
            let release_b = release_b.clone();
            let b_finished = b_finished.clone();
            move |id, _cwd, servers| {
                assert!(servers.is_empty());
                let index = opens.fetch_add(1, Ordering::SeqCst);
                let workers = workers.clone();
                let entered_b = entered_b.clone();
                let release_b = release_b.clone();
                let b_finished = b_finished.clone();
                let native_ack_tx = native_ack_tx.clone();
                async move {
                    if index == 1 {
                        entered_b.notify_one();
                        release_b.notified().await;
                        b_finished.store(true, Ordering::SeqCst);
                        return Err("fixture B released".into());
                    }
                    assert_eq!(index, 0);
                    let (native_out, native_peer) = tokio::io::duplex(65536);
                    let (host, events) = HostSession::test_connection_with_ready(
                        native_out,
                        64,
                        json!({"type":"ready","protocol":"rpi",
                            "supportedCommands":["configure_file_io","file_io_response","prompt","abort"],
                            "supportedEvents":["file_io_request","file_io_request_closed"],
                            "fileIO":{"version":1,"maxPending":64,"maxTextBytes":262144,
                                "maxPayloadBytes":786432,"timeoutMs":60000}}),
                    );
                    let worker = tokio::spawn(async move {
                        let mut commands = BufReader::new(native_peer).lines();
                        while let Some(line) = commands.next_line().await.unwrap() {
                            let command: Value = serde_json::from_str(&line).unwrap();
                            let kind = command["type"].as_str().unwrap();
                            let data = match kind {
                                "configure_file_io" | "file_io_response" | "abort" => {
                                    json!({"accepted":true})
                                }
                                "prompt" => json!({"accepted":true,"runId":"file-run"}),
                                _ => {
                                    let _ = events
                                        .send(json!({"type":"response","id":command["id"],
                                        "success":false,"error":"unsupported fixture command"}));
                                    continue;
                                }
                            };
                            if events
                                .send(json!({"type":"response","id":command["id"],
                                "success":true,"data":data}))
                                .is_err()
                            {
                                break;
                            }
                            if kind == "prompt" {
                                events.send(json!({"type":"file_io_request","requestId":"native:47ef5c6b-35ab-492a-9b6c-ae79e9c81472",
                                    "operation":"write_text_file",
                                    "path":std::env::current_dir().unwrap().join("editor-buffer.txt"),
                                    "content":"committed editor buffer"})).unwrap();
                            }
                            if kind == "file_io_response" {
                                native_ack_tx.send(command.clone()).unwrap();
                                let _ = events.send(json!({"type":"file_io_request_closed","requestId":"native:47ef5c6b-35ab-492a-9b6c-ae79e9c81472"}));
                                let _ = events.send(json!({"type":"agent_settled","runId":"file-run","outcome":"completed"}));
                            }
                            if kind == "abort" {
                                let _ = events.send(json!({"type":"agent_settled","runId":"file-run","outcome":"aborted"}));
                            }
                        }
                    });
                    workers.lock().unwrap().push(worker);
                    Ok((
                        Session::attach_for_test(host).await?,
                        json!({"sessionId":id}),
                    ))
                }
            }
        },
    );
    let scenario = async {
        tx.send(request(
            1,
            "initialize",
            json!({"protocolVersion":1,
            "clientCapabilities":{"fs":{"writeTextFile":true}}}),
        ))
        .unwrap();
        assert!(until_id(&mut lines, 1).await.get("result").is_some());
        tx.send(request(
            2,
            "session/new",
            json!({"cwd":std::env::current_dir().unwrap()}),
        ))
        .unwrap();
        let new_a = until_id(&mut lines, 2).await;
        let id_a = new_a["result"]["sessionId"].as_str().unwrap().to_owned();
        assert!(sessions.lock().await.contains_key(&id_a));
        tx.send(request(
            3,
            "session/prompt",
            json!({"sessionId":id_a,
            "prompt":[{"type":"text","text":"write through the client"}]}),
        ))
        .unwrap();
        let write = loop {
            let next = frame(&mut lines).await;
            if next["method"] == "fs/write_text_file" {
                break next;
            }
            assert!(
                next.get("id").is_none(),
                "prompt completed too early: {next}"
            );
        };
        assert_eq!(write["params"]["sessionId"], id_a);
        assert_eq!(writer.pending_count(), 1);
        tx.send(request(
            4,
            "session/new",
            json!({"cwd":std::env::current_dir().unwrap()}),
        ))
        .unwrap();
        entered_b.notified().await;
        // Fill all 64 nonresponse slots while B owns serialized dispatch. A
        // response following that full queue must still bypass it immediately.
        for _ in 0..INPUT_QUEUE_CAPACITY {
            tx.send(json!({"jsonrpc":"2.0","method":"fixture/ignored"}).to_string())
                .unwrap();
        }
        // Commit to the editor's authoritative buffer before sending success.
        let committed = write["params"]["content"].as_str().unwrap().to_owned();
        tx.send(json!({"jsonrpc":"2.0","id":write["id"],"result":{}}).to_string())
            .unwrap();
        let ack = tokio::time::timeout(Duration::from_secs(3), native_ack_rx.recv())
            .await
            .expect("native acknowledgement blocked behind B")
            .unwrap();
        assert_eq!(ack["type"], "file_io_response");
        assert_eq!(
            ack["requestId"],
            "native:47ef5c6b-35ab-492a-9b6c-ae79e9c81472"
        );
        assert_eq!(ack["result"], json!({}));
        assert!(
            ack.get("error").is_none(),
            "committed write falsely failed: {ack}"
        );
        assert_eq!(writer.pending_count(), 0);
        assert!(
            !b_finished.load(Ordering::SeqCst),
            "B must still be pending at native ACK"
        );
        assert_eq!(committed, "committed editor buffer");
        release_b.notify_one();
        let mut prompt_done = false;
        let mut b_done = false;
        while !prompt_done || !b_done {
            let next = frame(&mut lines).await;
            if next["id"] == 3 {
                assert_eq!(next["result"]["stopReason"], "end_turn");
                prompt_done = true;
            } else if next["id"] == 4 {
                assert!(next.get("error").is_some());
                b_done = true;
            } else {
                assert!(next.get("id").is_none(), "unexpected response: {next}");
            }
        }
        assert_eq!(committed, "committed editor buffer");
        drop(tx);
    };
    let (exit, ()) = tokio::time::timeout(Duration::from_secs(10), async {
        tokio::join!(server, scenario)
    })
    .await
    .unwrap();
    assert_eq!(exit, 0);
    assert!(sessions.lock().await.is_empty());
    assert!(writer.closed.load(Ordering::Acquire));
    let handles = std::mem::take(&mut *workers.lock().unwrap());
    for handle in handles {
        handle.abort();
        let _ = handle.await;
    }
    drop(stop_tx);
}

#[tokio::test]
async fn dispatcher_drains_eof_queue_in_order_after_probe_and_blocked_lifecycle() {
    let (output, peer) = tokio::io::duplex(65536);
    let writer = Arc::new(Writer::with_output(output));
    let mut lines = BufReader::new(peer).lines();
    let (tx, mut rx) = mpsc::unbounded_channel();
    let (stop_tx, stop) = watch::channel(false);
    let entered = Notify::new();
    let release = Notify::new();
    let eof_seen = Notify::new();
    let server = run_connection_with_test_opener(
        stop,
        || {
            let next = poll(&mut rx)?;
            if matches!(&next, InputLine::Eof) {
                eof_seen.notify_one();
            }
            Ok(next)
        },
        Some(request(1, "authenticate", json!({}))),
        writer.clone(),
        Err("unused".into()),
        Arc::new(Mutex::new(HashMap::new())),
        |_, _| async {
            entered.notify_one();
            release.notified().await;
            Err("fixture lifecycle released".into())
        },
    );
    let scenario = async {
        tx.send(request(
            2,
            "session/new",
            json!({"cwd":std::env::current_dir().unwrap()}),
        ))
        .unwrap();
        entered.notified().await;
        tx.send(request(3, "authenticate", json!({}))).unwrap();
        tx.send("{".into()).unwrap();
        tx.send(request(4, "authenticate", json!({}))).unwrap();
        drop(tx);
        eof_seen.notified().await;
        // EOF must not close Writer while B owns lifecycle or requests remain.
        assert!(!writer.closed.load(Ordering::Acquire));
        release.notify_one();
        assert_eq!(frame(&mut lines).await["id"], 1);
        assert_eq!(frame(&mut lines).await["id"], 2);
        assert_eq!(frame(&mut lines).await["id"], 3);
        let parse_error = frame(&mut lines).await;
        assert_eq!(parse_error["id"], Value::Null);
        assert_eq!(parse_error["error"]["code"], -32700);
        assert_eq!(frame(&mut lines).await["id"], 4);
    };
    let (exit, ()) = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(server, scenario)
    })
    .await
    .unwrap();
    assert_eq!(exit, 0);
    drop(stop_tx);
}

#[tokio::test]
async fn dispatcher_overflow_fails_closed_instead_of_stranding_responses() {
    let (output, _peer) = tokio::io::duplex(65536);
    let writer = Arc::new(Writer::with_output(output));
    let (tx, mut rx) = mpsc::unbounded_channel();
    let (stop_tx, stop) = watch::channel(false);
    let entered = Notify::new();
    let release = Notify::new();
    let server = run_connection_with_test_opener(
        stop,
        || poll(&mut rx),
        None,
        writer.clone(),
        Err("unused".into()),
        Arc::new(Mutex::new(HashMap::new())),
        |_, _| async {
            entered.notify_one();
            release.notified().await;
            Err("fixture lifecycle released".into())
        },
    );
    let scenario = async {
        tx.send(request(
            1,
            "session/new",
            json!({"cwd":std::env::current_dir().unwrap()}),
        ))
        .unwrap();
        entered.notified().await;
        for id in 2..=66 {
            tx.send(request(id, "authenticate", json!({}))).unwrap();
        }
        writer.terminal().await;
        assert_eq!(writer.pending_count(), 0);
        release.notify_one();
        drop(tx);
    };
    let (exit, ()) = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(server, scenario)
    })
    .await
    .unwrap();
    assert_eq!(exit, 2);
    drop(stop_tx);
}

#[tokio::test]
async fn dispatcher_read_failure_preserves_exit_two() {
    let (output, _peer) = tokio::io::duplex(65536);
    let writer = Arc::new(Writer::with_output(output));
    let (stop_tx, stop) = watch::channel(false);
    let exit = run_connection(
        stop,
        || {
            Err(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "fixture read failure",
            ))
        },
        None,
        writer.clone(),
        Err("unused".into()),
    )
    .await;
    assert_eq!(exit, 2);
    assert!(writer.closed.load(Ordering::Acquire));
    assert_eq!(writer.pending_count(), 0);
    drop(stop_tx);
}

#[tokio::test]
async fn dispatcher_stop_closes_writer_while_owned_lifecycle_is_pending() {
    let (output, _peer) = tokio::io::duplex(65536);
    let writer = Arc::new(Writer::with_output(output));
    let (tx, mut rx) = mpsc::unbounded_channel();
    let (stop_tx, stop) = watch::channel(false);
    let entered = Notify::new();
    let release = Notify::new();
    // Deliberately !Send: ingress must not spawn or require a Send reader.
    let polls = std::rc::Rc::new(std::cell::Cell::new(0usize));
    let server = run_connection_with_test_opener(
        stop,
        || {
            polls.set(polls.get() + 1);
            poll(&mut rx)
        },
        None,
        writer.clone(),
        Err("unused".into()),
        Arc::new(Mutex::new(HashMap::new())),
        |_, _| async {
            entered.notify_one();
            release.notified().await;
            Err("fixture lifecycle released".into())
        },
    );
    let scenario = async {
        tx.send(request(
            1,
            "session/new",
            json!({"cwd":std::env::current_dir().unwrap()}),
        ))
        .unwrap();
        entered.notified().await;
        stop_tx.send(true).unwrap();
        writer.terminal().await;
        assert_eq!(writer.pending_count(), 0);
        // Cleanup retains ownership of lifecycle; no detached opener survives.
        release.notify_one();
    };
    let (exit, ()) = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(server, scenario)
    })
    .await
    .unwrap();
    assert_eq!(exit, 0);
    assert!(polls.get() > 0);
}
