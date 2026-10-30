use super::*;
use tokio::io::{AsyncBufReadExt, BufReader};

fn reply() -> RequestReply {
    RequestReply {
        result: json!({"sessionId":"opened"}),
        commands: Some(json!({"sessionId":"opened","update":{
            "sessionUpdate":"available_commands_update","availableCommands":[{"name":"compact"}]}})),
    }
}

async fn join_request(tasks: &RequestTasks, id: &Value) {
    let task = tasks.lock().await.remove(&request_key(id)).unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(2), task.handle.unwrap())
        .await
        .expect("request task did not settle")
        .unwrap();
}

#[tokio::test]
async fn opening_response_precedes_exactly_one_inventory_notification() {
    let (output, input) = tokio::io::duplex(8192);
    let writer = Arc::new(Writer::with_output(output));
    let tasks = RequestTasks::default();
    let id = json!("opening");
    assert!(
        spawn_request_reply(
            &tasks,
            &PromptTasks::default(),
            id.clone(),
            writer.clone(),
            |_| async { Ok(reply()) }
        )
        .await
    );
    join_request(&tasks, &id).await;
    drop(writer);
    let mut lines = BufReader::new(input).lines();
    let first: Value = serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
    let second: Value = serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
    assert_eq!(
        first,
        json!({"jsonrpc":"2.0","id":id,"result":{"sessionId":"opened"}})
    );
    assert_eq!(second["method"], "session/update");
    assert_eq!(second["params"], reply().commands.unwrap());
    assert!(
        lines.next_line().await.unwrap().is_none(),
        "extra response/notification"
    );
}

#[tokio::test]
async fn failed_or_cancelled_opening_never_publishes_prepared_inventory() {
    for cancel in [false, true] {
        let (output, input) = tokio::io::duplex(8192);
        let writer = Arc::new(Writer::with_output(output));
        let tasks = RequestTasks::default();
        let id = json!("failed-opening");
        let (ready, prepared) = tokio::sync::oneshot::channel();
        assert!(
            spawn_request_reply(
                &tasks,
                &PromptTasks::default(),
                id.clone(),
                writer.clone(),
                move |mut cancelled| async move {
                    let _prepared_inventory = reply();
                    ready.send(()).unwrap();
                    if cancel {
                        wait_cancelled(&mut cancelled).await;
                        Err(RequestError::Cancelled)
                    } else {
                        Err(RequestError::Internal(
                            "opening failed after discovery".into(),
                        ))
                    }
                }
            )
            .await
        );
        prepared.await.unwrap();
        if cancel {
            assert!(cancel_request_task(&tasks, &request_key(&id)).await);
        }
        join_request(&tasks, &id).await;
        drop(writer);
        let mut lines = BufReader::new(input).lines();
        let frame: Value =
            serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
        assert_eq!(frame["id"], id);
        assert_eq!(frame["error"]["code"], if cancel { -32800 } else { -32603 });
        assert!(
            lines.next_line().await.unwrap().is_none(),
            "inventory leaked after failure"
        );
    }
}

#[tokio::test]
async fn failed_response_transport_closes_without_inventory_write() {
    struct FailedOutput(Arc<AtomicU64>);
    impl AsyncWrite for FailedOutput {
        fn poll_write(
            self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
            _: &[u8],
        ) -> std::task::Poll<std::io::Result<usize>> {
            self.0.fetch_add(1, Ordering::SeqCst);
            std::task::Poll::Ready(Err(std::io::ErrorKind::BrokenPipe.into()))
        }
        fn poll_flush(
            self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            std::task::Poll::Ready(Ok(()))
        }
        fn poll_shutdown(
            self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            std::task::Poll::Ready(Ok(()))
        }
    }
    let writes = Arc::new(AtomicU64::new(0));
    let writer = Arc::new(Writer::with_output(FailedOutput(writes.clone())));
    let tasks = RequestTasks::default();
    let id = json!(1);
    assert!(
        spawn_request_reply(
            &tasks,
            &PromptTasks::default(),
            id.clone(),
            writer.clone(),
            |_| async { Ok(reply()) }
        )
        .await
    );
    join_request(&tasks, &id).await;
    assert!(writer.closed.load(Ordering::Acquire));
    assert_eq!(writes.load(Ordering::SeqCst), 1);
}
