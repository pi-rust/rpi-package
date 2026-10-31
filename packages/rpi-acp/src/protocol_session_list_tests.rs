#[test]
fn list_capability_and_envelopes_allow_absent_null_and_optional_params() {
    assert_eq!(
        initialize_result()["agentCapabilities"]["sessionCapabilities"]["list"],
        json!({})
    );
    for request in [
        json!({"jsonrpc":"2.0","id":7,"method":"session/list"}),
        json!({"jsonrpc":"2.0","id":"7","method":"session/list","params":null}),
        json!({"jsonrpc":"2.0","id":-1,"method":"session/list","params":{}}),
    ] {
        let RpcMessage::Request { params, .. } = parse_rpc_message(request).unwrap() else {
            panic!()
        };
        assert_eq!(crate::session_list::parse(&params).unwrap().limit, 50);
    }
    assert_ne!(request_key(&json!(7)), request_key(&json!("7")));
}

// Runs the production dispatcher with only its input/output replaced; there is
// no native peer, model, MCP configuration, credential source or filesystem setup.
#[tokio::test]
async fn list_dispatcher_works_without_initialization_or_native_and_never_creates_root() {
    use rpi_plugin_sdk::stdio::InputLine;
    let root = std::env::temp_dir().join(format!("acp-list-dispatch-{}", uuid::Uuid::new_v4()));
    let (output, peer) = tokio::io::duplex(16_384);
    let writer = Arc::new(Writer::with_output(output));
    let mut lines = BufReader::new(peer).lines();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Value>();
    let (_stop_tx, stop) = watch::channel(false);
    let server = run_connection(
        stop,
        || match rx.try_recv() {
            Ok(message) => Ok(InputLine::Line(message.to_string())),
            Err(tokio::sync::mpsc::error::TryRecvError::Empty) => Ok(InputLine::Pending),
            Err(_) => Ok(InputLine::Eof),
        },
        None,
        writer,
        Ok(root.clone()),
    );
    let client = async {
        for (i, params) in [
            Value::Null,
            json!({}),
            json!({"cwd":null,"cursor":null,"limit":1000}),
        ]
        .into_iter()
        .enumerate()
        {
            tx.send(json!({"jsonrpc":"2.0","id":i,"method":"session/list","params":params}))
                .unwrap();
            let response: Value =
                serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
            assert_eq!(
                response,
                json!({"jsonrpc":"2.0","id":i,"result":{"sessions":[]}})
            );
        }
        for (i, params) in [
            json!({"cursor":"not-issued"}),
            json!({"cwd":"relative"}),
            json!([]),
            json!({"cwd":root.join("nonexistent-filter")}),
        ]
        .into_iter()
        .enumerate()
        {
            tx.send(json!({"jsonrpc":"2.0","id":i+10,"method":"session/list","params":params}))
                .unwrap();
            let response: Value =
                serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
            assert_eq!(response["error"]["code"], -32602);
            assert!(response.get("result").is_none());
        }
        // Invalid list requests did not poison authentication or initialization.
        tx.send(json!({"jsonrpc":"2.0","id":"init","method":"initialize","params":{"protocolVersion":1}})).unwrap();
        let response: Value =
            serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
        assert_eq!(
            response["result"]["agentCapabilities"]["sessionCapabilities"]["list"],
            json!({})
        );
        drop(tx);
    };
    let (code, ()) = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        tokio::join!(server, client)
    })
    .await
    .unwrap();
    assert_eq!(code, 0);
    assert!(!root.exists());
}

#[tokio::test]
async fn list_dispatcher_cross_method_duplicates_typed_ids_cancel_and_eof() {
    let root = std::env::temp_dir().join(format!("acp-list-eof-{}", uuid::Uuid::new_v4()));
    let (output, peer) = tokio::io::duplex(16_384);
    let writer = Arc::new(Writer::with_output(output));
    let mut lines = BufReader::new(peer).lines();
    // Preload the serialized dispatch FIFO. Independent ingress now yields to
    // other tasks, so a raw stdin burst no longer guarantees that list jobs
    // remain pending. This fixture isolates duplicate admission and EOF cleanup;
    // protocol_response_progress_tests covers ingress/dispatcher integration.
    let messages = [
        json!({"jsonrpc":"2.0","id":1,"method":"session/list"}),
        json!({"jsonrpc":"2.0","id":1,"method":"session/prompt","params":{}}),
        json!({"jsonrpc":"2.0","id":1,"method":"session/list"}),
        json!({"jsonrpc":"2.0","id":"1","method":"session/list"}),
        json!({"jsonrpc":"2.0","id":3,"method":"session/list"}),
        json!({"jsonrpc":"2.0","method":"$/cancel_request","params":{"requestId":1}}),
        // Only numeric 1 is explicitly cancelled. String "1" must be
        // cancelled and joined by EOF cleanup, independently of that request.
        json!({"jsonrpc":"2.0","id":"cancel","method":"$/cancel_request","params":{"requestId":1}}),
    ];
    let (tx, rx) = tokio::sync::mpsc::channel(INPUT_QUEUE_CAPACITY);
    for message in messages {
        tx.try_send(parse_rpc_message(message)).unwrap();
    }
    drop(tx);
    let (outcome_tx, outcome_rx) = oneshot::channel();
    outcome_tx.send(Ok(())).unwrap();
    let (_stop_tx, stop) = watch::channel(false);
    assert_eq!(
        tokio::time::timeout(
            std::time::Duration::from_secs(10),
            dispatch_connection(
                stop,
                rx,
                outcome_rx,
                writer,
                Ok(root.clone()),
                Arc::new(Mutex::new(HashMap::new())),
                |_, _, _| async { Err("no session opening in this fixture".to_owned()) },
            )
        )
        .await
        .unwrap(),
        0
    );
    let mut responses = Vec::new();
    while let Some(line) = lines.next_line().await.unwrap() {
        responses.push(serde_json::from_str::<Value>(&line).unwrap());
    }
    assert_eq!(responses.len(), 6);
    assert_eq!(responses[0]["error"]["code"], -32600);
    assert_eq!(responses[1]["error"]["code"], -32600);
    assert_eq!(responses[2]["id"], 3);
    assert_eq!(responses[2]["error"]["code"], -32001);
    assert_eq!(
        responses[3],
        json!({"jsonrpc":"2.0","id":"cancel","result":{}})
    );
    let completions = &responses[4..];
    assert!(completions.iter().any(|r| r["id"] == json!(1)));
    assert!(completions.iter().any(|r| r["id"] == json!("1")));
    for completion in completions {
        assert_eq!(completion["error"]["code"], -32800);
        assert!(completion.get("result").is_none());
    }
    assert!(!root.exists());
}

#[tokio::test]
async fn invalid_captured_root_affects_list_only() {
    use rpi_plugin_sdk::stdio::InputLine;
    let (output, peer) = tokio::io::duplex(8192);
    let mut lines = BufReader::new(peer).lines();
    let writer = Arc::new(Writer::with_output(output));
    let mut messages = std::collections::VecDeque::from([
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":1}}),
        json!({"jsonrpc":"2.0","id":2,"method":"session/list"}),
        json!({"jsonrpc":"2.0","id":3,"method":"authenticate"}),
    ]);
    let (_tx, stop) = watch::channel(false);
    assert_eq!(
        run_connection(
            stop,
            || Ok(messages
                .pop_front()
                .map(|m| InputLine::Line(m.to_string()))
                .unwrap_or(InputLine::Eof)),
            None,
            writer,
            Err("sensitive-root-value".into())
        )
        .await,
        0
    );
    for id in 1..=3 {
        let response: Value =
            serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
        assert_eq!(response["id"], id);
        if id == 2 {
            assert_eq!(response["error"]["code"], -32001);
        } else {
            assert!(response.get("result").is_some());
        }
        assert!(!response.to_string().contains("sensitive-root-value"));
    }
}

#[tokio::test]
async fn list_dispatcher_cancel_is_typed_and_does_not_cancel_other_list() {
    use rpi_plugin_sdk::stdio::InputLine;
    let root = std::env::temp_dir().join(format!("acp-list-cancel-{}", uuid::Uuid::new_v4()));
    let (output, peer) = tokio::io::duplex(16_384);
    let writer = Arc::new(Writer::with_output(output));
    let mut lines = BufReader::new(peer).lines();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Value>();
    // Queue all three before yielding so cancellation precedes task polling.
    tx.send(json!({"jsonrpc":"2.0","id":1,"method":"session/list"}))
        .unwrap();
    tx.send(json!({"jsonrpc":"2.0","id":"1","method":"session/list"}))
        .unwrap();
    tx.send(json!({"jsonrpc":"2.0","method":"$/cancel_request","params":{"requestId":1}}))
        .unwrap();
    let (_stop_tx, stop) = watch::channel(false);
    let server = run_connection(
        stop,
        || match rx.try_recv() {
            Ok(message) => Ok(InputLine::Line(message.to_string())),
            Err(tokio::sync::mpsc::error::TryRecvError::Empty) => Ok(InputLine::Pending),
            Err(_) => Ok(InputLine::Eof),
        },
        None,
        writer,
        Ok(root.clone()),
    );
    let client = async {
        let mut responses = Vec::new();
        for _ in 0..2 {
            responses.push(
                serde_json::from_str::<Value>(&lines.next_line().await.unwrap().unwrap()).unwrap(),
            );
        }
        let cancelled = responses.iter().find(|r| r["id"] == json!(1)).unwrap();
        assert_eq!(cancelled["error"]["code"], -32800);
        assert!(cancelled.get("result").is_none());
        let other = responses.iter().find(|r| r["id"] == json!("1")).unwrap();
        assert_eq!(other["result"], json!({"sessions":[]}));
        assert!(other.get("error").is_none());
        // EOF happens only after both completions, so it cannot explain the
        // cancelled response or suppress a duplicate completion unnoticed.
        drop(tx);
        assert!(lines.next_line().await.unwrap().is_none());
    };
    let (code, ()) = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        tokio::join!(server, client)
    })
    .await
    .unwrap();
    assert_eq!(code, 0);
    assert!(!root.exists());
}

// Observe a partial write followed by Pending without reading any response byte.
// The retained duplex peer holds one byte, proving in-progress backpressure.
struct ListBackpressuredOutput {
    output: tokio::io::DuplexStream,
    written: usize,
    stalled: Option<tokio::sync::oneshot::Sender<usize>>,
}

impl tokio::io::AsyncWrite for ListBackpressuredOutput {
    fn poll_write(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        bytes: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        let this = self.get_mut();
        let result = std::pin::Pin::new(&mut this.output).poll_write(cx, bytes);
        match &result {
            std::task::Poll::Ready(Ok(count)) => this.written += count,
            std::task::Poll::Pending if this.written > 0 => {
                if let Some(stalled) = this.stalled.take() {
                    let _ = stalled.send(this.written);
                }
            }
            _ => {}
        }
        result
    }

    fn poll_flush(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.get_mut().output).poll_flush(cx)
    }

    fn poll_shutdown(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.get_mut().output).poll_shutdown(cx)
    }
}

async fn list_shutdown_with_retained_unread_peer(host_stop: bool, wait_for_write: bool) {
    use rpi_plugin_sdk::stdio::InputLine;
    let root = std::env::temp_dir().join(format!("acp-list-stalled-{}", uuid::Uuid::new_v4()));
    let (output, unread_peer) = tokio::io::duplex(1);
    let (stalled_tx, stalled_rx) = tokio::sync::oneshot::channel();
    let writer = Arc::new(Writer::with_output(ListBackpressuredOutput {
        output,
        written: 0,
        stalled: Some(stalled_tx),
    }));
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Value>();
    tx.send(json!({"jsonrpc":"2.0","id":1,"method":"session/list"}))
        .unwrap();
    let (stop_tx, stop) = watch::channel(false);
    let server = run_connection(
        stop,
        || match rx.try_recv() {
            Ok(message) => Ok(InputLine::Line(message.to_string())),
            Err(tokio::sync::mpsc::error::TryRecvError::Empty) => Ok(InputLine::Pending),
            Err(_) => Ok(InputLine::Eof),
        },
        None,
        writer,
        Ok(root.clone()),
    );
    if !wait_for_write {
        // Minimal B1 recipe: accepted task followed immediately by EOF.
        drop(tx);
        assert_eq!(
            tokio::time::timeout(std::time::Duration::from_secs(5), server)
                .await
                .unwrap(),
            0
        );
    } else {
        let client = async {
            // Positive proof: one byte accepted and next write polled Pending
            // while still connected. No response byte is consumed by this test.
            assert_eq!(stalled_rx.await.unwrap(), 1);
            if host_stop {
                stop_tx.send_replace(true);
                // Keep input alive so only host-stop can explain cleanup.
                std::future::pending::<()>().await;
            } else {
                drop(tx);
                std::future::pending::<()>().await;
            }
        };
        tokio::pin!(client);
        assert_eq!(
            tokio::time::timeout(std::time::Duration::from_secs(5), async {
                tokio::select! {
                    code = server => code,
                    _ = &mut client => unreachable!(),
                }
            })
            .await
            .unwrap(),
            0
        );
    }
    // Keep the unread peer through cleanup: never a BrokenPipe wakeup.
    drop(unread_peer);
    assert!(!root.exists());
}

#[tokio::test]
async fn list_dispatcher_eof_never_waits_for_retained_unread_peer() {
    list_shutdown_with_retained_unread_peer(false, false).await;
}

#[tokio::test]
async fn list_dispatcher_eof_interrupts_already_backpressured_completion() {
    list_shutdown_with_retained_unread_peer(false, true).await;
}

#[tokio::test]
async fn list_dispatcher_host_stop_interrupts_already_backpressured_completion() {
    list_shutdown_with_retained_unread_peer(true, true).await;
}
