use super::*;
use std::collections::VecDeque;
use std::sync::Mutex;

fn request() -> Request {
    parse_request(&json!({"task":"inspect"})).unwrap()
}
fn completed() -> Value {
    json!({"status":"done","result":{"status":"completed","text":"report","turns":1,"turnLimited":false}})
}

fn scripted(replies: Vec<Result<Value, String>>) -> (Box<Action>, Arc<Mutex<Vec<Value>>>) {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let saved = calls.clone();
    let replies = Mutex::new(VecDeque::from(replies));
    let action = Box::new(move |value: Value| {
        saved.lock().unwrap().push(value.clone());
        if value["op"] == "dispose" || value["op"] == "cancel" {
            return Ok(json!({}));
        }
        replies
            .lock()
            .unwrap()
            .pop_front()
            .expect("unexpected action")
    });
    (action, calls)
}

#[test]
fn registered_schema_is_valid_json_with_only_model_fields_and_task_required() {
    // This is the builder passed to register_tool, not a duplicate schema fixture.
    let registered = delegate_schema();
    assert_eq!(registered.name.to_string_lossy(), "delegate_task");
    let description = registered.description.to_string_lossy();
    for guidance in [
        "host-managed in-memory",
        "not a subagent subprocess",
        "shares the workspace",
        "acceptance",
        "cannot delegate further",
    ] {
        assert!(
            description.contains(guidance),
            "missing guidance: {guidance}"
        );
    }
    let parameters: Value = serde_json::from_str(&registered.parameters.to_string_lossy()).unwrap();
    assert_eq!(parameters["type"], "object");
    assert_eq!(parameters["required"], json!(["task"]));
    assert_eq!(parameters["additionalProperties"], true);
    let properties = parameters["properties"].as_object().unwrap();
    assert_eq!(properties.len(), 4);
    assert!(!properties.contains_key("__rpi"));
    for (name, kind) in [
        ("task", "string"),
        ("role", "string"),
        ("maxTurns", "integer"),
        ("timeoutSeconds", "integer"),
    ] {
        let property = properties[name].as_object().unwrap();
        assert_eq!(property["type"], kind);
        assert!(!property["description"].as_str().unwrap().is_empty());
    }
    assert_eq!(properties["task"]["minLength"], 1);
    // Runtime bounds bytes after role trimming, not JSON Schema character counts.
    assert!(properties["task"].get("maxLength").is_none());
    assert!(properties["role"].get("maxLength").is_none());
    let context = json!({"task":"inspect", "__rpi":{
        "cwd":"D:/project", "sessionId":"parent", "runtimeHandle":42,
        "runtimeKey":"host-only", "future":{"nested":[{"value":true}]}
    }, "ignoredOption":{"nested":[1,2,3]}});
    assert_eq!(parse_request(&context).unwrap(), request());
}

#[test]
fn registered_schema_defaults_match_runtime_and_budgets_are_normalized_not_rejected() {
    let registered = delegate_schema();
    let parameters: Value = serde_json::from_str(&registered.parameters.to_string_lossy()).unwrap();
    let defaults = request();
    let properties = &parameters["properties"];
    assert_eq!(properties["role"]["default"], defaults.role);
    assert_eq!(properties["maxTurns"]["default"], defaults.max_turns);
    assert_eq!(
        properties["timeoutSeconds"]["default"],
        defaults.timeout_seconds
    );
    for (name, minimum, maximum) in [("maxTurns", 1, 32), ("timeoutSeconds", 10, 1800)] {
        assert_eq!(properties[name]["minimum"], minimum);
        assert_eq!(properties[name]["maximum"], maximum);
    }
    for (value, turns, timeout) in [
        (json!(0), 1, 10),
        (json!(1), 1, 10),
        (json!(32), 32, 32),
        (json!(1800), 32, 1800),
        (json!(u64::MAX), 32, 1800),
        (json!(-1), 8, 300),
        (json!(1.0), 8, 300),
        (json!(1.5), 8, 300),
        (json!(1e30), 8, 300),
        (Value::Null, 8, 300),
        (json!(true), 8, 300),
        (json!("12"), 8, 300),
        (json!([]), 8, 300),
        (json!({}), 8, 300),
    ] {
        let parsed = parse_request(&json!({
            "task":"inspect", "maxTurns":value, "timeoutSeconds":value
        }))
        .unwrap();
        assert_eq!(parsed.max_turns, turns, "maxTurns for {value}");
        assert_eq!(
            parsed.timeout_seconds, timeout,
            "timeoutSeconds for {value}"
        );
    }
}

#[test]
fn task_and_role_rules_use_utf8_bytes_and_role_only_is_trimmed() {
    let task = format!(" {} ", "界".repeat(6666)); // exactly 20,000 bytes
    let role = format!("  {}  ", "界".repeat(66));
    let parsed = parse_request(&json!({"task":task,"role":role})).unwrap();
    assert_eq!(parsed.task, task);
    assert_eq!(parsed.role, "界".repeat(66));
    assert!(parse_request(&json!({"task":"界".repeat(6667)})).is_err());
    assert!(parse_request(&json!({"task":"inspect","role":"界".repeat(67)})).is_err());
    // Raw role length may exceed 200 bytes when its trimmed value fits.
    let padded = format!("{}{} ", " ".repeat(201), "x".repeat(200));
    assert_eq!(
        parse_request(&json!({"task":"inspect","role":padded}))
            .unwrap()
            .role,
        "x".repeat(200)
    );
    for role in [Value::Null, json!(false), json!(42), json!([]), json!({})] {
        assert_eq!(
            parse_request(&json!({"task":"inspect","role":role}))
                .unwrap()
                .role,
            "worker"
        );
    }
    for task in [
        Value::Null,
        json!(42),
        json!([]),
        json!({}),
        json!(""),
        json!(" \t\n\u{2003}"),
    ] {
        assert!(parse_request(&json!({"task":task})).is_err());
    }
    for role in ["", " \t\n\u{2003}"] {
        assert!(parse_request(&json!({"task":"inspect","role":role})).is_err());
    }
}

#[test]
fn validates_and_clamps_limits_while_accepting_host_context() {
    let r = parse_request(
        &json!({"task":"inspect","maxTurns":999,"timeoutSeconds":1,"__rpi":{"cwd":"D:/project"}}),
    )
    .unwrap();
    assert_eq!(r.max_turns, 32);
    assert_eq!(r.timeout_seconds, 10);
    assert_eq!(request().max_turns, 8);
    assert_eq!(request().timeout_seconds, 300);
    for value in [
        json!({}),
        json!({"task":" "}),
        json!({"task":"x".repeat(20001)}),
        json!({"task":"x","role":"x".repeat(201)}),
    ] {
        assert!(parse_request(&value).is_err());
    }
}

#[test]
fn forwards_deltas_and_returns_delegation_metadata_then_disposes() {
    let (action, calls) = scripted(vec![
        Ok(json!({"childId":"child-1","sessionId":"child-1"})),
        Ok(json!({"status":"running","deltas":["hello"]})),
        Ok(completed()),
    ]);
    let (tx, rx) = mpsc::channel();
    run_request(
        request(),
        "tool-1".into(),
        &*action,
        tx,
        Arc::new(AtomicBool::new(false)),
    )
    .unwrap();
    assert!(matches!(rx.recv().unwrap(),JobMessage::Partial(text) if text == "hello"));
    let JobMessage::Done(text) = rx.recv().unwrap() else {
        panic!("missing final result")
    };
    let result: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(result["kind"], "rpi-delegation");
    assert_eq!(result["report"], "report");
    assert_eq!(result["sessionId"], "child-1");
    let calls = calls.lock().unwrap();
    assert_eq!(calls[0]["toolCallId"], "tool-1");
    assert_eq!(calls.last().unwrap()["op"], "dispose");
}

#[test]
fn unsupported_host_is_explicit_and_never_spawns_a_process() {
    let (action, calls) = scripted(vec![Err("unknown runtime action id 24".into())]);
    let (tx, _) = mpsc::channel();
    let error = run_request(
        request(),
        "t".into(),
        &*action,
        tx,
        Arc::new(AtomicBool::new(false)),
    )
    .unwrap_err();
    assert!(error.contains("requires a host supporting ChildAgent"));
    assert_eq!(calls.lock().unwrap().len(), 1);
}

#[test]
fn cancellation_before_start_has_no_host_side_effects() {
    let (action, calls) = scripted(vec![]);
    let (tx, _) = mpsc::channel();
    assert!(run_request(
        request(),
        "t".into(),
        &*action,
        tx,
        Arc::new(AtomicBool::new(true))
    )
    .is_err());
    assert!(calls.lock().unwrap().is_empty());
}

#[test]
fn cancellation_after_start_cancels_and_disposes() {
    let cancelled = Arc::new(AtomicBool::new(false));
    let token = cancelled.clone();
    let calls = Arc::new(Mutex::new(Vec::new()));
    let saved = calls.clone();
    let action = move |value: Value| {
        saved.lock().unwrap().push(value.clone());
        match value["op"].as_str().unwrap() {
            "start" => Ok(json!({"childId":"c"})),
            "poll" => {
                token.store(true, Ordering::Release);
                Ok(json!({"status":"running"}))
            }
            _ => Ok(json!({})),
        }
    };
    let (tx, _rx) = mpsc::channel();
    assert!(run_request(request(), "t".into(), &action, tx, cancelled)
        .unwrap_err()
        .contains("cancelled"));
    let ops: Vec<_> = calls
        .lock()
        .unwrap()
        .iter()
        .map(|v| v["op"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(ops, vec!["start", "poll", "cancel", "dispose"]);
}

#[test]
fn errors_and_malformed_replies_always_dispose() {
    for reply in [
        Err("lost bridge".into()),
        Ok(json!({"status":"failed","error":"permission denied"})),
        Ok(json!({"status":"invalid"})),
        Ok(json!({"status":"done"})),
        Ok(json!({"status":"done","result":{"status":"failed","error":"provider failed"}})),
    ] {
        let (action, calls) = scripted(vec![Ok(json!({"childId":"c"})), reply]);
        let (tx, _rx) = mpsc::channel();
        assert!(run_request(
            request(),
            "t".into(),
            &*action,
            tx,
            Arc::new(AtomicBool::new(false))
        )
        .is_err());
        assert_eq!(calls.lock().unwrap().last().unwrap()["op"], "dispose");
    }
}

#[test]
fn streamed_and_final_text_share_a_budget() {
    let (action, calls) = scripted(vec![
        Ok(json!({"childId":"c"})),
        Ok(json!({"status":"running","deltas":["a".repeat(MAX_OUTPUT_BYTES)]})),
        Ok(completed()),
    ]);
    let (tx, _rx) = mpsc::channel();
    assert!(run_request(
        request(),
        "t".into(),
        &*action,
        tx,
        Arc::new(AtomicBool::new(false))
    )
    .unwrap_err()
    .contains("output limit"));
    assert_eq!(calls.lock().unwrap().last().unwrap()["op"], "dispose");
}

#[test]
fn partial_count_is_bounded_independently_of_bytes() {
    let (action, _) = scripted(vec![
        Ok(json!({"childId":"c"})),
        Ok(json!({"status":"running","deltas":vec!["";MAX_PARTIAL_EVENTS+1]})),
    ]);
    let (tx, _rx) = mpsc::channel();
    assert!(run_request(
        request(),
        "t".into(),
        &*action,
        tx,
        Arc::new(AtomicBool::new(false))
    )
    .unwrap_err()
    .contains("output limit"));
}

#[test]
fn destroy_joins_the_worker_and_retires_the_handle() {
    let cancelled = Arc::new(AtomicBool::new(false));
    let token = cancelled.clone();
    let exited = Arc::new(AtomicBool::new(false));
    let done = exited.clone();
    let (tx, rx) = mpsc::channel();
    let worker = thread::spawn(move || {
        let _tx = tx;
        while !token.load(Ordering::Acquire) {
            thread::sleep(Duration::from_millis(1));
        }
        done.store(true, Ordering::Release);
    });
    let handle = JOBS.insert_with_cancel(
        Job {
            receiver: rx,
            completed: false,
            worker: Some(worker),
        },
        cancelled,
    );
    destroy(handle);
    assert!(exited.load(Ordering::Acquire));
    assert!(JOBS.get(handle).is_none());
    destroy(handle);
}

#[test]
fn registration_context_must_match_the_calling_session_or_project() {
    let actual = json!({"sessionId":"s1","cwd":"D:/project"});
    assert!(validate_context(&json!({"sessionId":"s1"}), &actual).is_ok());
    assert!(validate_context(&json!({"cwd":"D:/project"}), &actual).is_ok());
    assert!(validate_context(&json!({"sessionId":"other","cwd":"D:/project"}), &actual).is_err());
    assert!(validate_context(&json!({"cwd":"D:/other"}), &actual).is_err());
    assert!(validate_context(&Value::Null, &actual).is_err());
}
