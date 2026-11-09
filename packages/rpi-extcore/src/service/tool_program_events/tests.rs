use super::*;

const CHILD: &str = "tpchild-11111111-1111-4111-8111-111111111111";
const OTHER: &str = "tpchild-22222222-2222-4222-8222-222222222222";
const PHASE_ID: &str = "tpphase-33333333-3333-4333-8333-333333333333";
const BODY: &str = "44444444-4444-4444-8444-444444444444";

fn ready() -> Value {
    json!({"toolProgramChildEvents":profile(),"supportedEvents":[BEGIN, PHASE, RESYNC]})
}
fn context() -> ChildEventContext {
    ChildEventContext {
        session_id: "session".into(),
        lane: "main".into(),
    }
}
fn begin() -> Value {
    json!({"type":BEGIN,"schemaVersion":1,"sourceSessionId":"session","lane":"main",
        "entryId":CHILD,"parentEntryId":"parent-entry","seq":10,"customType":BEGIN_CUSTOM,
        "data":{"version":1,"childId":CHILD,"origin":{"runId":"run","assistantEntryId":"assistant","toolIndex":2},
        "rootCallId":"call","rootToolName":"program","toolName":"read","ordinal":0,"parentChildId":null}})
}
fn phase(kind: &str) -> Value {
    let closed = kind == "closed";
    let final_phase = kind == "final";
    json!({"type":PHASE,"schemaVersion":1,"sourceSessionId":"session","lane":"main",
        "entryId":PHASE_ID,"parentEntryId":CHILD,"seq":20,"customType":PHASE_CUSTOM,
        "data":{"version":1,"childId":CHILD,"index":if kind == "requested" {0} else {3},"phase":kind,
        "descriptor":if closed {Value::Null} else {json!({"version":1,"descriptorId":format!("tpbody-{BODY}-commit"),
        "bodyId":BODY,"childId":CHILD,"kind":kind,"codec":"native-json-v1","encoding":"base64",
        "byteLength":32769,"chunkBytes":32768,"chunks":2,"sha256":"a".repeat(64)})},
        "isError":final_phase.then_some(false),"cancelled":final_phase.then_some(false),
        "failed":closed.then_some(false),"interrupted":closed.then_some(false)}})
}
fn parse_event(value: &Value) -> ChildEvent {
    parse(&ready(), &context(), value).unwrap().unwrap()
}
fn malformed_event(value: &Value) {
    let error = parse(&ready(), &context(), value).unwrap_err();
    assert_eq!(error.0, ReplyKind::HostUnavailable);
    assert_eq!(
        error.1,
        json!({"error":"native host returned an invalid child event"})
    );
}
fn changed(mut value: Value, pointer: &str, replacement: Value) -> Value {
    *value.pointer_mut(pointer).unwrap() = replacement;
    value
}

#[test]
fn valid_begin_roundtrip_preserves_native_metadata_and_unknown_state() {
    for parent in [Value::Null, json!(OTHER)] {
        let value = changed(begin(), "/data/parentChildId", parent);
        let event = parse_event(&value);
        assert_eq!(serde_json::to_value(&event).unwrap(), value);
        let ChildEvent::Begin(commit) = event else {
            panic!("begin expected")
        };
        let summary = commit.summary();
        assert_eq!(summary.seq, 10);
        assert_eq!(summary.origin.run_id, "run");
        assert_eq!(summary.origin.tool_index, 2);
        assert_eq!(summary.state, query::ChildState::Unknown);
        assert_eq!(
            summary.origin_verification,
            query::ChildOriginVerification::NativeCheckpoint
        );
    }
}

#[test]
fn every_phase_and_all_terminal_flag_combinations_are_metadata_only() {
    let ChildEvent::Begin(begin) = parse_event(&begin()) else {
        unreachable!()
    };
    for kind in ["requested", "prepared", "update", "final", "closed"] {
        for first in [false, true] {
            for second in [false, true] {
                let mut value = phase(kind);
                if kind == "final" {
                    value["data"]["isError"] = json!(first);
                    value["data"]["cancelled"] = json!(second);
                } else if kind == "closed" {
                    value["data"]["failed"] = json!(first);
                    value["data"]["interrupted"] = json!(second);
                }
                let event = parse_event(&value);
                assert_eq!(serde_json::to_value(&event).unwrap(), value);
                let ChildEvent::Phase(commit) = event else {
                    unreachable!()
                };
                assert_eq!(commit.phase().seq, 20);
                assert_eq!(commit.phase().entry_id, PHASE_ID);
                assert_eq!(begin.summary().state, query::ChildState::Unknown);
            }
        }
    }
    // No ordering accumulator or inferred lifecycle: isolated/repeated close is
    // valid metadata, not an execution/parent settlement or synthesized success.
    parse_event(&phase("closed"));
    parse_event(&phase("requested"));
    parse_event(&phase("closed"));
}

#[test]
fn envelope_identity_schema_sequence_parent_and_custom_type_are_strict() {
    for initial in [begin(), phase("closed")] {
        let own_id = initial["entryId"].clone();
        for (pointer, replacements) in [
            (
                "/sourceSessionId",
                vec![json!("foreign"), json!(""), json!("session\n")],
            ),
            ("/lane", vec![json!("other"), json!(""), Value::Null]),
            (
                "/schemaVersion",
                vec![json!(0), json!(2), json!(256), json!("1")],
            ),
            ("/seq", vec![json!(0), json!(-1), json!(1.5), json!("20")]),
            (
                "/entryId",
                vec![
                    json!("entry"),
                    json!("tp-11111111-1111-4111-8111-111111111111"),
                    json!("tpphase-33333333-3333-1333-8333-333333333333"),
                ],
            ),
            (
                "/parentEntryId",
                vec![
                    Value::Null,
                    own_id,
                    json!(""),
                    json!("parent\u{7f}"),
                    json!("p".repeat(513)),
                ],
            ),
            (
                "/customType",
                vec![
                    json!("unknown"),
                    json!(if initial["type"] == BEGIN {
                        PHASE_CUSTOM
                    } else {
                        BEGIN_CUSTOM
                    }),
                ],
            ),
            ("/data/version", vec![json!(0), json!(2)]),
            (
                "/data/childId",
                vec![
                    json!("arbitrary"),
                    json!("tpchild-11111111-1111-1111-8111-111111111111"),
                ],
            ),
        ] {
            for replacement in replacements {
                malformed_event(&changed(initial.clone(), pointer, replacement));
            }
        }
        let mut missing = initial;
        missing.as_object_mut().unwrap().remove("parentEntryId");
        malformed_event(&missing);
    }
    malformed_event(&changed(begin(), "/data/childId", json!(OTHER)));
    malformed_event(&changed(phase("closed"), "/entryId", json!(CHILD)));
    malformed_event(&changed(begin(), "/entryId", json!(PHASE_ID)));
}

#[test]
fn begin_uses_query_summary_validation() {
    for pointer in [
        "/data/origin/runId",
        "/data/origin/assistantEntryId",
        "/data/rootCallId",
        "/data/rootToolName",
        "/data/toolName",
    ] {
        for invalid in ["".to_owned(), "secret\n".to_owned(), "x".repeat(513)] {
            malformed_event(&changed(begin(), pointer, json!(invalid)));
        }
    }
    for parent in [CHILD, "not-native"] {
        malformed_event(&changed(begin(), "/data/parentChildId", json!(parent)));
    }
    malformed_event(&changed(begin(), "/data/ordinal", json!(-1)));
    malformed_event(&changed(
        begin(),
        "/data/origin/toolIndex",
        json!(4294967296_u64),
    ));
}

#[test]
fn unknown_fields_nested_body_and_private_request_id_injections_are_rejected() {
    for mut value in [begin(), phase("final"), phase("closed")] {
        for pointer in ["", "/data"] {
            for field in [
                "body",
                "arguments",
                "result",
                "requestId",
                "privateRequestId",
                "entryId",
                "seq",
                "unknown",
            ] {
                if pointer.is_empty() && ["entryId", "seq"].contains(&field) {
                    continue;
                }
                let mut injected = value.clone();
                injected.pointer_mut(pointer).unwrap()[field] =
                    json!({"secret":"never return this diagnostic"});
                malformed_event(&injected);
            }
        }
        if value["type"] == BEGIN {
            value["data"]["origin"]["requestId"] = json!("private");
        } else if value["data"]["descriptor"].is_object() {
            value["data"]["descriptor"]["body"] = json!("secret");
        } else {
            continue;
        }
        malformed_event(&value);
    }
}

#[test]
fn phase_index_flags_and_descriptor_validation_match_query_contract() {
    for kind in ["requested", "prepared", "update", "final", "closed"] {
        for flag in ["isError", "cancelled", "failed", "interrupted"] {
            let mut value = phase(kind);
            value["data"][flag] = if value["data"][flag].is_null() {
                json!(false)
            } else {
                Value::Null
            };
            malformed_event(&value);
            value["data"][flag] = json!("false");
            malformed_event(&value);
        }
        let mut missing_descriptor = phase(kind);
        missing_descriptor["data"]["descriptor"] = if kind == "closed" {
            phase("final")["data"]["descriptor"].clone()
        } else {
            Value::Null
        };
        malformed_event(&missing_descriptor);
        malformed_event(&changed(
            phase(kind),
            "/data/index",
            json!(if kind == "requested" { 1 } else { 0 }),
        ));
    }
    for (field, bad) in [
        ("version", json!(2)),
        ("descriptorId", json!("path")),
        ("bodyId", json!("not-a-uuid")),
        ("childId", json!(OTHER)),
        ("kind", json!("requested")),
        ("codec", json!("json")),
        ("encoding", json!("raw")),
        ("byteLength", json!(0)),
        ("chunkBytes", json!(16384)),
        ("chunks", json!(1)),
        ("sha256", json!("A".repeat(64))),
    ] {
        let mut value = phase("final");
        value["data"]["descriptor"][field] = bad;
        malformed_event(&value);
    }
}

#[test]
fn event_profile_is_exact_and_all_event_names_are_independently_required() {
    let expected = json!({"version":1,"beginEvent":BEGIN,"phaseEvent":PHASE,"resyncEvent":RESYNC,
        "sequence":"nativeEntrySeq-v1","delivery":"bestEffortResync","maxEventBytes":32768});
    assert_eq!(profile(), expected);
    assert!(supported_ready(&ready())); // no query profile or commands
    for mask in 0..7 {
        let events: Vec<_> = [BEGIN, PHASE, RESYNC]
            .into_iter()
            .enumerate()
            .filter_map(|(index, name)| (mask & (1 << index) != 0).then_some(name))
            .collect();
        let mut r = ready();
        r["supportedEvents"] = json!(events);
        assert!(!supported_ready(&r));
        for event in [
            begin(),
            phase("final"),
            json!({"type":RESYNC,"reason":"commit_stream_lagged","skipped":1}),
        ] {
            assert_eq!(
                parse(&r, &context(), &event).unwrap_err().0,
                ReplyKind::Unsupported
            );
        }
    }
    for field in expected.as_object().unwrap().keys() {
        for replacement in [None, Some(Value::Null), Some(json!("wrong"))] {
            let mut r = ready();
            match replacement {
                None => {
                    r["toolProgramChildEvents"]
                        .as_object_mut()
                        .unwrap()
                        .remove(field);
                }
                Some(v) => {
                    r["toolProgramChildEvents"][field] = v;
                }
            }
            assert_eq!(
                parse(&r, &context(), &begin()).unwrap_err().0,
                ReplyKind::Unsupported
            );
        }
    }
    let mut extra = ready();
    extra["toolProgramChildEvents"]["extra"] = json!(true);
    assert!(!supported_ready(&extra));
    let mut nested = ready();
    let p = nested
        .as_object_mut()
        .unwrap()
        .remove("toolProgramChildEvents")
        .unwrap();
    nested["semantics"] = json!({"toolProgramChildEvents":p});
    assert!(!supported_ready(&nested));
    let query_only = json!({"toolProgramChildren":{"version":1},"supportedCommands":["get_tool_program_children","get_tool_program_child_phases","get_tool_program_child_body"]});
    assert_eq!(
        parse(&query_only, &context(), &begin()).unwrap_err().0,
        ReplyKind::Unsupported
    );
}

#[tokio::test]
async fn supported_host_api_matches_ready_gate_without_sending_commands() {
    for r in [ready(), json!({}), json!({"toolProgramChildren":profile()})] {
        let expected = supported_ready(&r);
        let (session, _events) = HostSession::test_connection_with_ready(tokio::io::sink(), 4, r);
        assert_eq!(supported(&session), expected);
        session.shutdown().await;
    }
}

#[test]
fn unrelated_events_are_ignored_even_on_old_hosts_and_invalid_context() {
    let bad_context = ChildEventContext {
        session_id: String::new(),
        lane: String::new(),
    };
    for value in [
        Value::Null,
        json!({}),
        json!({"type":"agent_settled","body":"x".repeat(40000)}),
        json!({"type":"tool_program_child_begin_extra"}),
        json!({"type":123}),
    ] {
        assert!(parse(&json!({}), &bad_context, &value).unwrap().is_none());
    }
    // Capability failure remains distinct even when a recognized event is bad.
    assert_eq!(
        parse(&json!({}), &bad_context, &json!({"type":BEGIN}))
            .unwrap_err()
            .0,
        ReplyKind::Unsupported
    );
}

#[test]
fn context_and_envelope_ids_are_bounded_control_free_and_exact() {
    for field in ["session", "lane"] {
        for invalid in [
            "".to_owned(),
            "bad\n".to_owned(),
            "bad\u{85}".to_owned(),
            "x".repeat(513),
        ] {
            let mut c = context();
            if field == "session" {
                c.session_id = invalid;
            } else {
                c.lane = invalid;
            }
            let mut value = begin();
            value["sourceSessionId"] = json!(c.session_id);
            value["lane"] = json!(c.lane);
            assert_eq!(
                parse(&ready(), &c, &value).unwrap_err().0,
                ReplyKind::HostUnavailable
            );
        }
    }
    let c = ChildEventContext {
        session_id: "s".repeat(512),
        lane: "l".repeat(512),
    };
    let mut value = begin();
    value["sourceSessionId"] = json!(c.session_id);
    value["lane"] = json!(c.lane);
    value["parentEntryId"] = json!("p".repeat(512));
    parse(&ready(), &c, &value).unwrap().unwrap();
    malformed_event(&changed(begin(), "/data/toolName", json!("é".repeat(257))));
}

#[test]
fn resync_accepts_only_native_reasons_positive_skip_and_no_payload_fields() {
    for reason in [
        "commit_stream_lagged",
        "unknown_native_journal_custom_type",
        "native_journal_event_too_large",
    ] {
        let value = json!({"type":RESYNC,"reason":reason,"skipped":1});
        let event = parse_event(&value);
        assert_eq!(serde_json::to_value(event).unwrap(), value);
        for skipped in [json!(0), json!(-1), json!(1.5), json!("1")] {
            malformed_event(&changed(value.clone(), "/skipped", skipped));
        }
        let mut injected = value;
        injected["data"] = json!({"body":"private"});
        malformed_event(&injected);
    }
    malformed_event(&json!({"type":RESYNC,"reason":"secret/path diagnostic","skipped":1}));
}

fn padded(mut value: Value, pointer: &str, size: usize) -> Value {
    *value.pointer_mut(pointer).unwrap() = json!("");
    let base = serde_json::to_vec(&value).unwrap().len();
    assert!(base <= size);
    *value.pointer_mut(pointer).unwrap() = json!("x".repeat(size - base));
    assert_eq!(serde_json::to_vec(&value).unwrap().len(), size);
    value
}

#[test]
fn exact_complete_event_and_metadata_byte_budgets_have_no_oversize_exception() {
    // Valid strict metadata cannot fill these budgets: each individual string
    // is capped at 512. Test the actual bounded writer at N and N-1 separately
    // from semantic rejection, rather than inventing an allowed padding field.
    let mut event = begin();
    event["unknown"] = json!("");
    let event = padded(event, "/unknown", MAX_EVENT_BYTES);
    bounded(&event, MAX_EVENT_BYTES).unwrap();
    assert!(bounded(&event, MAX_EVENT_BYTES - 1).is_err());
    malformed_event(&event); // even in-budget unknown fields are forbidden
    let event = padded(event, "/unknown", MAX_EVENT_BYTES + 1);
    malformed_event(&event);

    let data = padded(begin()["data"].clone(), "/toolName", MAX_DATA_BYTES);
    bounded(&data, MAX_DATA_BYTES).unwrap();
    assert!(bounded(&data, MAX_DATA_BYTES - 1).is_err());
    let mut event = begin();
    event["data"] = data.clone();
    assert!(serde_json::to_vec(&event).unwrap().len() < MAX_EVENT_BYTES);
    malformed_event(&event); // size alone cannot authorize oversized strings
    event["data"] = padded(data, "/toolName", MAX_DATA_BYTES + 1);
    malformed_event(&event);

    // Full original event budget includes serde-unknown/nested metadata, not
    // merely the typed projection (which would discard or normalize fields).
    let mut event = phase("closed");
    event["data"]["body"] = json!(["private".repeat(MAX_EVENT_BYTES)]);
    malformed_event(&event);
}
