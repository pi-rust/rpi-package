// Included in protocol::tests so transport state remains private to the adapter.
#[tokio::test]
async fn eof_cleanup_rejects_interactive_work_and_never_invents_acknowledgements() {
    let (output, peer) = tokio::io::duplex(8192);
    let writer = Arc::new(Writer::with_output(output));
    let mut lines = BufReader::new(peer).lines();
    let pending = tokio::spawn({
        let writer = writer.clone();
        async move { writer.request("elicitation/create", json!({})).await }
    });
    lines.next_line().await.unwrap().unwrap();
    writer.close_input();
    assert_eq!(pending.await.unwrap(), Value::Null);
    assert!(!writer.is_closed());
    let (_cancel, mut receiver) = watch::channel(false);
    for method in ["fs/read_text_file", "fs/write_text_file", "terminal/create", "terminal/wait_for_exit", "elicitation/create"] {
        assert_eq!(writer.request(method, json!({})).await, Value::Null);
        assert_eq!(writer.request_cancellable(method, json!({}), &mut receiver).await, Value::Null);
        assert_eq!(writer.request_terminal_cleanup(method, json!({}), &mut receiver).await, Value::Null);
    }
    let token = rpi_extcore::mcp_external::CancellationToken::default();
    assert!(writer.request_checked("tools/call", json!({}), token).await.is_err());
    for method in ["terminal/kill", "terminal/release"] {
        assert_eq!(writer.request_terminal_cleanup(method, json!({"sessionId":"child:1:s:worker", "terminalId":"owned-terminal"}), &mut receiver).await, Value::Null);
        let frame: Value = serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
        assert_eq!(frame["method"], method);
        assert_eq!(frame["params"]["sessionId"], "child:1:s:worker");
        assert_eq!(frame["params"]["terminalId"], "owned-terminal");
    }
    assert_eq!(writer.pending_count(), 0);
    writer.close();
    drop(writer);
    assert!(lines.next_line().await.unwrap().is_none());
}

#[tokio::test]
async fn eof_cleanup_output_backpressure_is_bounded_and_closes_output() {
    // Fill an undrained surviving write half. No global timeout env mutation:
    // the production writer deadline must itself close the broken stream.
    let (output, _undrained) = tokio::io::duplex(1);
    let writer = Writer::with_output(output);
    writer.close_input();
    let (_cancel, mut receiver) = watch::channel(false);
    let result = tokio::time::timeout(
        writer_timeout() + std::time::Duration::from_secs(2),
        writer.request_terminal_cleanup("terminal/kill", json!({"sessionId":"s","terminalId":"t"}), &mut receiver),
    ).await.expect("cleanup must respect output backpressure deadline");
    assert_eq!(result, Value::Null);
    assert!(writer.is_closed());
    assert_eq!(writer.pending_count(), 0);
    assert_eq!(writer.request_terminal_cleanup("terminal/release", json!({"sessionId":"s","terminalId":"t"}), &mut receiver).await, Value::Null);
}
