#[test]
fn compact_parser_requires_pure_text_and_exact_token() {
    let text = |s: &str| ContentBlock::Text { text: s.into() };
    for (input, expected) in [
        ("/compact", None),
        (" \t/compact\n ", None),
        (
            "/compact keep decisions\n  and next steps 🌊",
            Some("keep decisions\n  and next steps 🌊"),
        ),
        ("/compact\tkeep all", Some("keep all")),
    ] {
        assert_eq!(
            compact_instructions(&[text(input)]),
            Some(expected.map(str::to_owned))
        );
    }
    assert_eq!(
        compact_instructions(&[text("/compact keep"), text("second line")]),
        Some(Some("keep\nsecond line".into()))
    );
    for input in [
        "",
        "/compactness",
        "/compact: keep",
        "please /compact",
        "`/compact`",
        "/COMPACT",
    ] {
        assert_eq!(compact_instructions(&[text(input)]), None);
    }
    for resource in [
        ContentBlock::ResourceLink {
            name: Some("/compact".into()),
            uri: "file:///compact".into(),
        },
        ContentBlock::Resource {
            resource: crate::protocol::EmbeddedResource {
                uri: "file:///compact".into(),
                text: Some("/compact".into()),
            },
        },
        ContentBlock::Other,
    ] {
        assert_eq!(compact_instructions(std::slice::from_ref(&resource)), None);
        assert_eq!(compact_instructions(&[text("/compact"), resource]), None);
    }
}

#[tokio::test]
async fn unsupported_compact_errors_but_compactness_remains_a_prompt() {
    let (session, commands) = test_session();
    let writer = Arc::new(Writer::with_output(tokio::io::sink()));
    assert!(!session.compact_available());
    assert!(session
        .prompt(
            &writer,
            "s",
            &[ContentBlock::Text {
                text: "/compact".into()
            }]
        )
        .await
        .unwrap_err()
        .contains("does not support compact"));
    let task = tokio::spawn({
        let session = session.clone();
        async move {
            session
                .prompt(
                    &writer,
                    "s",
                    &[ContentBlock::Text {
                        text: "/compactness".into(),
                    }],
                )
                .await
        }
    });
    let mut commands = BufReader::new(commands).lines();
    let command = read_json(&mut commands).await;
    assert_eq!(command["type"], "prompt");
    assert_eq!(command["message"], "/compactness");
    settle_prompt(&session, &command, json!({"outcome":"completed"}));
    assert_eq!(task.await.unwrap(), Ok("end_turn"));
}

#[tokio::test]
async fn compact_shares_prompt_stream_and_holds_lease_through_cancel() {
    let (stdin, commands) = tokio::io::duplex(8192);
    let (rpc, events) = HostSession::test_connection_with_ready(
        stdin,
        64,
        json!({"supportedCommands":["compact"]}),
    );
    let session = Arc::new(Session {
        rpc,
        durable: None,
        configuration: tokio::sync::OnceCell::new(),
        events: Some(events),
    });
    assert!(session.compact_available());
    let (output, peer) = tokio::io::duplex(8192);
    let writer = Arc::new(Writer::with_output(output));
    let mut replies = BufReader::new(peer).lines();
    let mut commands = BufReader::new(commands).lines();
    let task = tokio::spawn({
        let session = session.clone();
        let writer = writer.clone();
        async move {
            session
                .prompt(
                    &writer,
                    "s",
                    &[ContentBlock::Text {
                        text: "/compact keep\n  everything".into(),
                    }],
                )
                .await
        }
    });
    let command = read_json(&mut commands).await;
    assert_eq!(command["type"], "compact");
    assert_eq!(command["customInstructions"], "keep\n  everything");
    let events = session.events.as_ref().unwrap();
    events
        .send(json!({"type":"response","id":command["id"],"success":true,"data":{"accepted":true}}))
        .unwrap();
    events.send(json!({"type":"agent_end"})).unwrap();
    session.abort().await.unwrap();
    assert_eq!(read_json(&mut commands).await["type"], "abort");
    tokio::task::yield_now().await;
    assert!(!task.is_finished());
    assert!(session
        .prompt(&writer, "s", &[])
        .await
        .unwrap_err()
        .contains("already running"));
    events
        .send(json!({"type":"agent_settled","outcome":"aborted","summary":"Compaction aborted."}))
        .unwrap();
    assert_eq!(
        read_json(&mut replies).await["params"]["update"]["content"]["text"],
        "Compaction aborted."
    );
    assert_eq!(task.await.unwrap(), Ok("cancelled"));
    assert!(session.rpc.try_run_lease().is_ok());
}
