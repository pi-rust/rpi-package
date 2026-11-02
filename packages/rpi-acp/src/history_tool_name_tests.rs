#[test]
fn replay_tool_names_preserve_strings_and_omit_other_types() {
    for name in [
        None,
        Some(Value::Null),
        Some(json!(17)),
        Some(json!(false)),
        Some(json!([])),
        Some(json!({})),
        Some(json!("")),
        Some(json!(" /工具/MCP_opaque!? ")),
        Some(json!("todo")),
    ] {
        let mut block = json!({"type":"toolCall","id":"call","arguments":{}});
        let mut message = json!({"role":"toolResult","toolCallId":"call","isError":true,"content":[{"type":"text","text":"result"}],"details":{"todos":[]}});
        if let Some(name) = &name {
            block["name"] = name.clone();
            message["toolName"] = name.clone();
        }
        let starts = message_updates(&json!({"role":"assistant","content":[block]}));
        let ends = message_updates(&message);
        assert_eq!(starts.len(), 1);
        assert_eq!(ends.len(), 1);
        for update in [&starts[0], &ends[0]] {
            assert_eq!(
                update.get("name").and_then(Value::as_str),
                name.as_ref().and_then(Value::as_str)
            );
            if name.as_ref().and_then(Value::as_str).is_none() {
                assert!(update.get("name").is_none());
            }
        }
        // Adding replay metadata must not route todo through live end formatting.
        let mut expected = translate_event(&json!({"type":"tool_execution_end","toolCallId":"call","isError":true,"result":message})).unwrap();
        if let Some(name) = name.as_ref().and_then(Value::as_str) {
            expected["name"] = json!(name);
        }
        assert_eq!(ends[0], expected);
        assert_eq!(ends[0]["rawOutput"], message);
        assert_eq!(ends[0]["status"], "failed");
        assert!(ends[0].get("kind").is_none());
    }
}

#[test]
fn replay_missing_result_name_does_not_guess_from_adjacent_call() {
    let entries: HashMap<String, Value> = [
        ("a", json!({"parentId":null,"type":"message","message":{"role":"assistant","content":[{"type":"toolCall","id":"call","name":"read","arguments":{}}]}})),
        ("t", json!({"parentId":"a","type":"message","message":{"role":"toolResult","toolCallId":"call","isError":false,"content":[]}})),
    ].into_iter().map(|(id, value)| (id.into(), value)).collect();
    let updates = branch_updates(&entries, Some("t")).unwrap();
    assert_eq!(updates.len(), 2);
    assert_eq!(updates[0]["name"], "read");
    assert!(updates[1].get("name").is_none());
    assert_eq!(updates[1]["status"], "completed");
}
