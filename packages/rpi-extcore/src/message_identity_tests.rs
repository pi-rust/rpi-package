fn required_projection() -> MessageProjection {
    let mut projection = MessageProjection::default();
    projection.require_message_ids(true);
    projection
}

fn assistant_frame(kind: &str, id: &str, text: &str, thinking: &str) -> Value {
    json!({"type":kind,"messageId":id,"message":{"role":"assistant","content":[
        {"type":"thinking","thinking":thinking},{"type":"text","text":text}
    ]}})
}

#[test]
fn native_identity_covers_initial_snapshot_compact_fallback_end_and_final_repair() {
    let mut projection = required_projection();
    assert_eq!(
        projection
            .event(&assistant_frame("message_start", "entry", "Hi", "think"))
            .unwrap(),
        vec![
            json!({"kind":"thinking","text":"think","messageId":"entry"}),
            json!({"kind":"text","text":"Hi","messageId":"entry"})
        ]
    );
    for (kind, text) in [("text", " there"), ("thinking", " more")] {
        assert_eq!(
            projection
                .event(&json!({"type":"message_update","messageId":"entry",
            "eventType":format!("{kind}_delta"),"delta":text}))
                .unwrap(),
            vec![json!({"kind":kind,"text":text,"messageId":"entry"})]
        );
    }
    // Snapshot lacks text: this is the old senders' compact fallback path.
    assert_eq!(
        projection
            .event(&json!({"type":"message_update","messageId":"entry",
        "eventType":"text_delta","delta":"!","message":{"role":"assistant",
        "content":[{"type":"thinking","thinking":"think more"}]}}))
            .unwrap(),
        vec![json!({"kind":"text","text":"!","messageId":"entry"})]
    );
    assert_eq!(
        projection
            .event(&assistant_frame(
                "message_end",
                "entry",
                "Hi there! End",
                "think more done"
            ))
            .unwrap(),
        vec![
            json!({"kind":"thinking","text":" done","messageId":"entry"}),
            json!({"kind":"text","text":" End","messageId":"entry"})
        ]
    );
    assert_eq!(
        projection.complete("Hi there! End.").unwrap(),
        Some(json!({"kind":"text","text":".","messageId":"entry"}))
    );
    assert_eq!(projection.message_id.as_deref(), Some("entry"));
}

#[test]
fn identical_messages_with_distinct_native_ids_are_not_text_deduplicated() {
    let mut projection = required_projection();
    for id in ["first", "second"] {
        assert_eq!(
            projection
                .event(&assistant_frame(
                    "message_start",
                    id,
                    "same",
                    "same thought"
                ))
                .unwrap(),
            vec![
                json!({"kind":"thinking","text":"same thought","messageId":id}),
                json!({"kind":"text","text":"same","messageId":id})
            ]
        );
        assert!(projection
            .event(&assistant_frame("message_end", id, "same", "same thought"))
            .unwrap()
            .is_empty());
    }
}

#[test]
fn native_missing_invalid_or_changed_ids_fail_before_mutating_any_text() {
    for invalid in [
        Value::Null,
        json!(""),
        json!("x".repeat(257)),
        json!("a\nb"),
        json!("a\u{0085}b"),
        json!(42),
        json!("different"),
    ] {
        let mut projection = required_projection();
        projection
            .event(&assistant_frame(
                "message_start",
                "entry",
                "before",
                "thought",
            ))
            .unwrap();
        let event = json!({"type":"message_update","messageId":invalid,
            "eventType":"text_delta","delta":"bad"});
        assert!(projection.event(&event).is_err());
        assert_eq!(projection.text, "before");
        assert_eq!(projection.thinking, "thought");
        assert_eq!(projection.message_id.as_deref(), Some("entry"));
    }
    for kind in ["message_start", "message_update", "message_end"] {
        let mut projection = required_projection();
        let mut event = assistant_frame(kind, "entry", "bad", "bad");
        event.as_object_mut().unwrap().remove("messageId");
        assert!(projection.event(&event).is_err());
        assert!(projection.text.is_empty());
        assert!(projection.thinking.is_empty());
    }
    let mut projection = required_projection();
    assert!(projection
        .event(&json!({"type":"message_update","eventType":"thinking_delta","delta":"bad"}))
        .is_err());
}

#[test]
fn native_id_is_bounded_in_utf8_bytes_and_final_text_never_invents_one() {
    let mut projection = required_projection();
    assert!(projection.complete("").unwrap().is_none());
    assert!(projection.complete("unattributed").is_err());
    assert!(projection
        .event(&assistant_frame("message_start", &"x".repeat(256), "", ""))
        .is_ok());
    let mut projection = required_projection();
    assert!(projection
        .event(&assistant_frame("message_start", &"🌊".repeat(65), "", ""))
        .is_err());
}

#[test]
fn legacy_without_ids_retains_exact_delta_shape_and_supplied_mismatch_is_rejected() {
    let mut projection = MessageProjection::default();
    assert_eq!(
        projection
            .event(&json!({"type":"message_update","eventType":"text_delta","delta":"old"}))
            .unwrap(),
        vec![json!({"kind":"text","text":"old"})]
    );
    assert_eq!(
        projection.complete("older").unwrap(),
        Some(json!({"kind":"text","text":"er"}))
    );
    // Attaching a new ID after unidentified output would relabel old text.
    assert!(projection.event(&json!({"type":"message_update","messageId":"new","eventType":"text_delta","delta":"bad"})).is_err());
    projection
        .event(&assistant_frame("message_start", "first", "same", ""))
        .unwrap();
    assert!(projection.event(&json!({"type":"message_update","messageId":"other","eventType":"text_delta","delta":"bad"})).is_err());
    assert_eq!(projection.text, "same");
    assert_eq!(
        projection.complete("same!").unwrap(),
        Some(json!({"kind":"text","text":"!","messageId":"first"}))
    );
}

#[test]
fn legacy_compact_fixture_remains_byte_shape_compatible_without_id_metadata() {
    let frames: Vec<Value> =
        serde_json::from_str(include_str!("../tests/fixtures/compact-deltas.json")).unwrap();
    let mut projection = MessageProjection::default();
    for event in frames {
        for delta in projection.event(&event).unwrap() {
            assert_eq!(delta.as_object().unwrap().len(), 2);
            assert!(delta.get("messageId").is_none());
        }
    }
    assert_eq!(projection.text, "Hello world! Saved.");
}

#[test]
fn live_projection_does_not_mix_out_of_order_ids_or_replace_current_snapshot() {
    let mut live = LiveMessageProjection::default();
    live.project(&mut assistant_frame("message_start", "old", "same", ""));
    live.project(&mut assistant_frame("message_start", "new", "same", ""));
    for mut stale in [
        json!({"type":"message_update","messageId":"old","eventType":"text_delta","contentIndex":1,"delta":"stale"}),
        assistant_frame("message_update", "old", "stale snapshot", ""),
        assistant_frame("message_end", "old", "stale end", ""),
    ] {
        let expected = stale.clone();
        live.project(&mut stale);
        assert_eq!(stale, expected);
    }
    let mut current = json!({"type":"message_update","messageId":"new","eventType":"text_delta","contentIndex":1,"delta":"!"});
    live.project(&mut current);
    assert_eq!(current["message"]["content"][1]["text"], "same!");
    assert_eq!(current["messageId"], "new");
}
