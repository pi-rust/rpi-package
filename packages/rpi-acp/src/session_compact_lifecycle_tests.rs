#[tokio::test]
async fn compact_reuses_dialog_and_tool_updates_then_settles() {
    let (stdin, commands) = tokio::io::duplex(8192);
    let (rpc, events) = HostSession::test_connection_with_ready(
        stdin,
        64,
        json!({"supportedCommands":["compact"]}),
    );
    let session = Arc::new(Session {
        rpc,
        context_presentation: tokio::sync::Mutex::default(),
        durable: None,
        configuration: tokio::sync::OnceCell::new(),
        events: Some(events),
    });
    let (output, peer) = tokio::io::duplex(8192);
    let writer = Arc::new(Writer::with_output(output));
    let mut replies = BufReader::new(peer).lines();
    let mut commands = BufReader::new(commands).lines();
    let task = tokio::spawn({
        let session = session.clone();
        async move {
            session
                .prompt(
                    &writer,
                    "s",
                    &[ContentBlock::Text {
                        text: "/compact".into(),
                    }],
                )
                .await
        }
    });
    let command = read_json(&mut commands).await;
    let events = session.events.as_ref().unwrap();
    events
        .send(json!({"type":"response","id":command["id"],"success":true,"data":{"accepted":true}}))
        .unwrap();
    // No question capability: the normal dialog task must cancel the question.
    events.send(json!({"type":"ui_dialog_request","requestId":"q","ui":{"kind":"input","title":"Question"}})).unwrap();
    let answer = read_json(&mut commands).await;
    assert_eq!(answer["type"], "ui_dialog_response");
    assert_eq!(answer["requestId"], "q");
    assert_eq!(answer["cancelled"], true);
    events.send(json!({"type":"tool_execution_start","toolCallId":"t","toolName":"read","args":{"path":"file"}})).unwrap();
    assert_eq!(
        read_json(&mut replies).await["params"]["update"]["sessionUpdate"],
        "tool_call"
    );
    assert!(!task.is_finished());
    events
        .send(json!({"type":"agent_settled","outcome":"declined"}))
        .unwrap();
    assert_eq!(
        read_json(&mut replies).await["params"]["update"]["content"]["text"],
        "Nothing to compact yet."
    );
    assert_eq!(task.await.unwrap(), Ok("end_turn"));
}

#[tokio::test]
async fn compact_stream_lag_and_host_closure_are_errors() {
    for lag in [false, true] {
        let (stdin, commands) = tokio::io::duplex(8192);
        let (rpc, events) = HostSession::test_connection_with_ready(
            stdin,
            4,
            json!({"supportedCommands":["compact"]}),
        );
        let session = Arc::new(Session {
            rpc,
            context_presentation: tokio::sync::Mutex::default(),
            durable: None,
            configuration: tokio::sync::OnceCell::new(),
            events: Some(events),
        });
        let task = tokio::spawn({
            let session = session.clone();
            async move {
                session
                    .prompt(
                        &Arc::new(Writer::with_output(tokio::io::sink())),
                        "s",
                        &[ContentBlock::Text {
                            text: "/compact".into(),
                        }],
                    )
                    .await
            }
        });
        let mut commands = BufReader::new(commands).lines();
        let command = read_json(&mut commands).await;
        let events = session.events.as_ref().unwrap();
        events.send(json!({"type":"response","id":command["id"],"success":true,"data":{"accepted":true}})).unwrap();
        if lag {
            for _ in 0..20 {
                events.send(json!({"type":"agent_end"})).unwrap();
            }
        } else {
            events.send(json!({"type":"host_closed"})).unwrap();
        }
        let error = tokio::time::timeout(std::time::Duration::from_secs(2), task)
            .await
            .unwrap()
            .unwrap()
            .unwrap_err();
        assert!(error.contains(if lag {
            "event stream lost"
        } else {
            "session ended"
        }));
        if lag {
            assert!(session
                .state()
                .await
                .unwrap_err()
                .contains("backend stopped"));
        }
    }
}
