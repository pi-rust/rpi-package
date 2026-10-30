// Included in session::tests. The native fixture owns an actual client-issued
// terminal handle, waits on it, then emits sequential cleanup only after EOF.
#[tokio::test]
async fn eof_cleanup_prompt_keeps_advancing_until_native_owner_settles() {
    let (session, commands) = test_session();
    let mut commands = BufReader::new(commands).lines();
    let (output, peer) = tokio::io::duplex(8192);
    let writer = Arc::new(Writer::with_output(output));
    writer.negotiate_subagents(&json!({"clientCapabilities":{"subagents":{}}}));
    let child = "child:1:s:worker";
    writer.register_child(child.into());
    let mut replies = BufReader::new(peer).lines();
    let prompt = tokio::spawn({
        let session = session.clone();
        let writer = writer.clone();
        async move { session.prompt(&writer, "s", &[]).await }
    });
    let command = read_json(&mut commands).await;
    let events = session.events.as_ref().unwrap();
    events.send(json!({"type":"response","id":command["id"],"command":"prompt","success":true,"data":{"accepted":true}})).unwrap();
    let event = |id: &str, method: &str, params: Value| json!({
        "type":"client_request", "requestId":id, "sessionId":"worker", "method":method, "params":params
    });
    events.send(event("create", "terminal/create", json!({"command":"fixture"}))).unwrap();
    let create = read_json(&mut replies).await;
    assert_eq!(create["method"], "terminal/create");
    assert_eq!(create["params"]["sessionId"], child);
    // The terminal ID originates at the client, not in the cleanup fixture.
    let terminal = format!("client-terminal-{}", uuid::Uuid::new_v4());
    writer.resolve(create["id"].as_u64().unwrap(), json!({"terminalId":terminal})).await;
    let created = read_json(&mut commands).await;
    assert_eq!(created["requestId"], "create");
    assert_eq!(created["result"]["terminalId"], terminal);
    events.send(event("wait", "terminal/wait_for_exit", json!({"terminalId":terminal}))).unwrap();
    let wait = read_json(&mut replies).await;
    assert_eq!(wait["method"], "terminal/wait_for_exit");
    writer.close_input();
    session.abort().await.unwrap();
    // The wait cancellation reply and native abort may be scheduled either way.
    let mut saw_abort = false;
    let mut saw_wait = false;
    while !saw_abort || !saw_wait {
        let response = read_json(&mut commands).await;
        if response["type"] == "abort" { saw_abort = true; }
        else {
            assert_eq!(response["requestId"], "wait");
            assert_eq!(response["cancelled"], true);
            assert!(response.get("result").is_none());
            saw_wait = true;
        }
    }
    // No post-disconnect create/filesystem/UI work may reach the write half.
    for (id, method, params) in [
        ("late-create", "terminal/create", json!({"command":"must-not-run"})),
        ("late-write", "fs/write_text_file", json!({"path":std::env::temp_dir().join("must-not-write"),"content":"forbidden"})),
    ] {
        events.send(event(id, method, params)).unwrap();
        let response = read_json(&mut commands).await;
        assert_eq!(response["requestId"], id);
        assert_eq!(response["cancelled"], true);
    }
    events.send(permission_event("late-ui")).unwrap();
    let response = read_json(&mut commands).await;
    assert_eq!(response["requestId"], "late-ui");
    assert_eq!(response["cancelled"], true);
    for (id, method) in [("kill", "terminal/kill"), ("release", "terminal/release")] {
        events.send(event(id, method, json!({"terminalId":terminal}))).unwrap();
        let cleanup = read_json(&mut replies).await;
        assert_eq!(cleanup["method"], method);
        assert_eq!(cleanup["params"]["sessionId"], child);
        assert_eq!(cleanup["params"]["terminalId"], terminal);
        // Input is EOF: do not reply. Native receives unavailable, not success,
        // and still advances to its next cleanup action.
        let response = read_json(&mut commands).await;
        assert_eq!(response["requestId"], id);
        assert_eq!(response["cancelled"], true);
        assert!(response.get("result").is_none());
        assert!(!prompt.is_finished(), "must await authoritative settlement");
    }
    events.send(json!({"type":"agent_settled","runId":"test-run","outcome":"aborted"})).unwrap();
    assert_eq!(tokio::time::timeout(std::time::Duration::from_secs(2), prompt).await.unwrap().unwrap(), Ok("cancelled"));
    assert_eq!(writer.pending_count(), 0);
    writer.close();
    drop(writer);
    assert!(replies.next_line().await.unwrap().is_none());
    session.kill().await;
}
