#[test]
fn native_tool_names_are_exact_optional_metadata_on_all_phases() {
    let names = [
        None,
        Some(Value::Null),
        Some(json!(42)),
        Some(json!(false)),
        Some(json!([])),
        Some(json!({})),
        Some(json!("")),
        Some(json!("read")),
        Some(json!("todo")),
        Some(json!(" /工具/Name!?\n ")),
        Some(json!("mcp_0123456789abcdef")),
    ];
    for name in names {
        for phase in [
            "tool_execution_start",
            "tool_execution_update",
            "tool_execution_end",
        ] {
            let mut event = json!({"type":phase,"toolCallId":"call","args":{},
                "partialResult":{"content":[{"type":"text","text":"progress"}]},
                "result":{"content":[{"type":"text","text":"done"}]}});
            if let Some(name) = &name {
                event["toolName"] = name.clone();
            }
            let update = translate_event(&event).unwrap();
            assert_eq!(
                update.get("name").and_then(Value::as_str),
                name.as_ref().and_then(Value::as_str)
            );
            if name.as_ref().and_then(Value::as_str).is_none() {
                assert!(update.get("name").is_none());
                if phase == "tool_execution_start" {
                    assert_eq!(update["title"], "tool");
                    assert_eq!(update["kind"], "other");
                }
            }
            assert_eq!(update["toolCallId"], "call");
            assert_eq!(
                update["status"],
                if phase == "tool_execution_end" {
                    "completed"
                } else {
                    "in_progress"
                }
            );
            if phase != "tool_execution_start" {
                assert!(update.get("kind").is_none());
            }
        }
    }
}

#[test]
fn names_do_not_create_empty_progress_or_change_tool_presentation() {
    for name in [json!(""), json!("edit"), json!("todo"), Value::Null] {
        for partial in [
            Value::Null,
            json!({}),
            json!({"content":[]}),
            json!({"content":[{"type":"text","text":""}]}),
        ] {
            assert_eq!(
                translate_event(
                    &json!({"type":"tool_execution_update","toolName":name,"partialResult":partial})
                ),
                None
            );
        }
    }
    for (name, kind) in [
        ("read", "read"),
        ("bash", "other"),
        ("write", "edit"),
        ("edit", "edit"),
        ("todo", "other"),
        ("mcp_opaque", "other"),
    ] {
        let args = json!({"path":"file","content":"new","edits":[{"oldText":"old","newText":"new"}],"action":"list"});
        let start =
            translate_event(&json!({"type":"tool_execution_start","toolName":name,"args":args}))
                .unwrap();
        assert_eq!(start["kind"], kind);
        assert_eq!(
            start.get("rawInput"),
            if name == "todo" { None } else { Some(&args) }
        );
        assert_eq!(
            start.get("content").is_some(),
            matches!(name, "write" | "edit")
        );
        for failed in [false, true] {
            let result =
                json!({"content":[{"type":"text","text":"result"}],"details":{"todos":[]}});
            let end = translate_event(&json!({"type":"tool_execution_end","toolName":name,"isError":failed,"result":result})).unwrap();
            assert_eq!(end["name"], name);
            assert_eq!(end["status"], if failed { "failed" } else { "completed" });
            assert_eq!(
                end.get("rawOutput"),
                if name == "todo" { None } else { Some(&result) }
            );
            assert_eq!(
                end["content"],
                json!([{"type":"content","content":{"type":"text","text":"result"}}])
            );
            assert!(end.get("kind").is_none());
        }
    }
}

#[tokio::test]
async fn prompt_forwards_native_tool_names_without_leaking_between_calls() {
    let (session, commands) = test_session();
    // This helper uses the real HostSession command writer/run tracker with a
    // ready frame that does not advertise get_state (no settlement usage RPC).
    let mut commands = BufReader::new(commands).lines();
    let (output, peer) = tokio::io::duplex(8192);
    let writer = Arc::new(Writer::with_output(output));
    let mut replies = BufReader::new(peer).lines();
    let task = tokio::spawn({
        let session = session.clone();
        async move { session.prompt(&writer, "s", &[]).await }
    });
    let command = read_json(&mut commands).await;
    assert_eq!(command["type"], "prompt");
    let events = session.events.as_ref().unwrap();
    events.send(json!({"type":"response","id":command["id"],"command":"prompt","success":true,"data":{"accepted":true}})).unwrap();
    events
        .send(json!({"type":"agent_start","runId":"name-run"}))
        .unwrap();
    for (id, name) in [("named", Some(" /工具/MCP_opaque!? ")), ("unnamed", None)] {
        for (phase, status) in [
            ("tool_execution_start", "in_progress"),
            ("tool_execution_update", "in_progress"),
            ("tool_execution_end", "completed"),
        ] {
            let mut event = json!({"type":phase,"runId":"name-run","toolCallId":id,"args":{},
                "partialResult":{"content":[{"type":"text","text":"working"}]},
                "result":{"content":[{"type":"text","text":"done"}]}});
            if let Some(name) = name {
                event["toolName"] = json!(name);
            }
            events.send(event).unwrap();
            let frame = read_json(&mut replies).await;
            assert_eq!(frame["method"], "session/update");
            assert_eq!(frame["params"]["sessionId"], "s");
            let update = &frame["params"]["update"];
            assert_eq!(update["toolCallId"], id);
            assert_eq!(
                update["sessionUpdate"],
                if phase == "tool_execution_start" {
                    "tool_call"
                } else {
                    "tool_call_update"
                }
            );
            assert_eq!(update["status"], status);
            assert_eq!(update.get("name").and_then(Value::as_str), name);
            if name.is_none() {
                assert!(update.get("name").is_none());
            }
        }
    }
    assert!(!task.is_finished());
    events
        .send(json!({"type":"agent_settled","runId":"name-run","outcome":"completed"}))
        .unwrap();
    assert_eq!(
        tokio::time::timeout(std::time::Duration::from_secs(2), task)
            .await
            .unwrap()
            .unwrap(),
        Ok("end_turn")
    );
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(30), commands.next_line())
            .await
            .is_err()
    );
    session.kill().await;
}
