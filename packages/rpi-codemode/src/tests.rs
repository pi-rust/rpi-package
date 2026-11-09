use super::*;
use rpi_plugin_sdk::StepResultTag;
use std::time::{Duration, Instant};

static ABI_HOST_TEST_LOCK: Mutex<()> = Mutex::new(());

#[test]
fn registration_schema_is_canonical_and_allows_host_injection() {
    let schema = schema();
    assert_eq!(schema.name.to_string_lossy(), "codemode");
    let v: Value = serde_json::from_str(&schema.parameters.to_string_lossy()).unwrap();
    assert_eq!(v["type"], "object");
    assert_eq!(v["x-rpi-execution-mode"], "sequential");
    assert_eq!(v["x-rpi-result-envelope"], 1);
    assert_eq!(v["additionalProperties"], true);
    assert_eq!(v["properties"]["action"]["default"], "execute");
    assert_eq!(
        v["properties"]["action"]["enum"],
        json!(["list", "execute"])
    );
    assert_eq!(v["properties"]["timeoutMs"]["default"], 30000);
    assert_eq!(v["properties"]["timeoutMs"]["maximum"], 120000);
    assert_eq!(v["properties"]["limit"]["default"], 32);
    for name in ["action", "code", "timeoutMs", "offset", "limit"] {
        assert!(!v["properties"][name]["description"]
            .as_str()
            .unwrap()
            .is_empty());
    }
    assert_eq!(v["allOf"][0]["then"]["required"], json!(["code"]));
    assert!(v["properties"].get("__rpi").is_none());
    assert!(engine::parse_request(
        &json!({"action":"list","__rpi":{"runtimeHandle":1,"runtimeKey":"key"},"unknown":true})
    )
    .is_ok());
    schema
        .name
        .free_with(Some(rpi_plugin_sdk::callbacks::free_string));
    schema
        .description
        .free_with(Some(rpi_plugin_sdk::callbacks::free_string));
    schema
        .parameters
        .free_with(Some(rpi_plugin_sdk::callbacks::free_string));
}
#[test]
fn broker_errors_preserve_numeric_diagnostics_but_not_actual_credentials() {
    rpi_plugin_sdk::string_handler!(fn numeric_error(_action, input, out, _data) {
        let request: Value = serde_json::from_str(input).unwrap();
        assert_eq!(request["__rpiRuntimeKey"], "actual-binding-key");
        out.write(json!({"error":format!(
            "native content exceeds 32 MiB; count 3; bytes 32000; key actual-binding-key; request {}",
            request["requestId"].as_str().unwrap())}).to_string());
        1
    });
    for handle in [3, 32] {
        let action = bind(
            Some(RuntimeFns {
                action: numeric_error,
                free: rpi_plugin_sdk::callbacks::free_string,
            }),
            &json!({"__rpi":{"runtimeHandle":handle,"runtimeKey":"actual-binding-key"}}),
        )
        .unwrap();
        let error = action(json!({"op":"poll","requestId":"tp-private"})).unwrap_err();
        assert_eq!(error, "ToolProgram action 30 failed: native content exceeds 32 MiB; count 3; bytes 32000; key [redacted]; request [request]");
        assert!(!error.contains("actual-binding-key"));
        assert!(!error.contains("tp-private"));
    }
}

#[test]
fn missing_host_and_missing_invocation_fail_closed() {
    assert!(bind(None, &json!({})).err().unwrap().contains("upgrade"));
    extern "C" fn action(
        _: u32,
        _: StbStringRef,
        _: *mut StbString,
        _: *mut std::ffi::c_void,
    ) -> i32 {
        panic!("must not call without binding")
    }
    let fns = RuntimeFns {
        action,
        free: rpi_plugin_sdk::callbacks::free_string,
    };
    assert!(bind(Some(fns), &json!({}))
        .err()
        .unwrap()
        .contains("runtimeHandle"));
    assert!(bind(Some(fns), &json!({"__rpi":{"runtimeHandle":1}}))
        .err()
        .unwrap()
        .contains("runtimeKey"));
}
#[test]
fn output_preserves_summary_error_and_terminate() {
    for status in ["completed", "terminated", "error", "cancelled", "timed_out"] {
        let v: Value = serde_json::from_str(&tool_result(json!({"status":status,
            "terminate":status == "terminated","logs":["partial"],"calls":[{"name":"write"}]})))
        .unwrap();
        assert_eq!(v["$rpiToolResult"], 1);
        assert_eq!(v["isError"], !matches!(status, "completed" | "terminated"));
        assert_eq!(v["result"]["terminate"], status == "terminated");
        assert_eq!(v["result"]["details"]["status"], status);
        assert_eq!(v["result"]["details"]["logs"][0], "partial");
        assert_eq!(v["result"]["details"]["calls"][0]["name"], "write");
        assert!(v["result"].get("usage").is_none());
        assert!(v["result"].get("addedToolNames").is_none());
    }
    let mut value = json!({"text":"prefix secret suffix","__rpi":{"runtimeHandle":1},"nested":{"runtimeKey":"secret"}});
    redact(&mut value, "secret");
    assert!(!value.to_string().contains("secret"));
    assert_eq!(value["__rpi"]["runtimeHandle"], 1);
}
#[test]
fn abi_poll_is_nonblocking_cancel_is_signal_only_destroy_joins() {
    let cancelled = Arc::new(AtomicBool::new(false));
    let token = cancelled.clone();
    let (sender, receiver) = mpsc::channel();
    let worker = thread::spawn(move || {
        let action: Arc<Action> = Arc::new(|_| {
            Ok(
                json!({"protocolVersion":1,"capabilities":{"resultEnvelope":1,"hostAccounting":1,"parentProgress":1,"losslessValues":1,"resultReferences":1,"contentAttachments":1},"tools":[],"nextOffset":null}),
            )
        });
        let r = engine::execute("for(;;){}", 10000, action, token);
        let _ = sender.send(Ok(r));
    });
    let handle = JOBS.insert_with_cancel(
        Job {
            receiver,
            worker: Some(worker),
            completed: false,
        },
        cancelled,
    );
    let start = Instant::now();
    assert_eq!(
        poll(handle, None, std::ptr::null_mut()).tag,
        StepResultTag::Pending
    );
    assert!(start.elapsed() < Duration::from_millis(100));
    cancel(handle);
    assert!(start.elapsed() < Duration::from_millis(100));
    destroy(handle);
    assert!(start.elapsed() < Duration::from_secs(2));
    assert!(JOBS.get(handle).is_none());
    destroy(handle);
    cancel(handle);
}

#[test]
fn final_encoded_output_has_its_own_budget_without_losing_termination() {
    let text = tool_result(
        json!({"status":"terminated","result":"x".repeat(engine::MAX_JSON/2),"terminate":true,"logs":[],"calls":[{"name":"write","status":"done"}]}),
    );
    assert!(text.len() <= engine::MAX_JSON);
    let r: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(r["isError"], true);
    assert_eq!(r["result"]["terminate"], true);
    assert_eq!(r["result"]["details"]["result"], Value::Null);
    assert_eq!(r["result"]["details"]["calls"][0]["name"], "write");
    assert!(r["result"]["details"]["error"]
        .as_str()
        .unwrap()
        .contains("encoded codemode output"));
}

fn await_abi(handle: StepHandle) -> rpi_plugin_sdk::callbacks::Step {
    let until = Instant::now() + Duration::from_secs(3);
    loop {
        let result = rpi_plugin_sdk::take_step_result!(poll(handle, None, std::ptr::null_mut()));
        if !matches!(result, rpi_plugin_sdk::callbacks::Step::Pending(_)) {
            return result;
        }
        assert!(Instant::now() < until, "ABI worker stalled");
        thread::sleep(Duration::from_millis(1));
    }
}
#[test]
fn abi_failed_execution_is_done_error_with_partial_work() {
    let _serial = ABI_HOST_TEST_LOCK.lock().unwrap();
    rpi_plugin_sdk::string_handler!(fn fake_action(_action, input, out, _data) {
        let request: Value = serde_json::from_str(input).unwrap();
        assert_eq!(request["__rpiRuntimeKey"], "abi-test-key");
        out.write(json!({"protocolVersion":1,"capabilities":{"resultEnvelope":1,"hostAccounting":1,"parentProgress":1,"losslessValues":1,"resultReferences":1,"contentAttachments":1},"tools":[],"nextOffset":null}).to_string());
        0
    });
    let previous = HOST.lock().unwrap().replace(RuntimeFns {
        action: fake_action,
        free: rpi_plugin_sdk::callbacks::free_string,
    });
    let params = json!({"code":"print('partial'); throw new Error('boom');","__rpi":{"runtimeHandle":1,"runtimeKey":"abi-test-key"}});
    let handle = execute(
        StbStringRef::from_str("test-abi-failure"),
        StbString::from_string(params.to_string()),
        Some(rpi_plugin_sdk::callbacks::free_string),
    );
    // execute snapshots the host binding before returning.
    *HOST.lock().unwrap() = previous;
    let result = await_abi(handle);
    destroy(handle);
    let rpi_plugin_sdk::callbacks::Step::Done(text) = result else {
        panic!("failure was not ABI Done: {result:?}");
    };
    let envelope: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(envelope["isError"], true);
    let value = &envelope["result"]["details"];
    assert_eq!(value["status"], "error");
    assert_eq!(value["logs"][0]["text"], "partial");
    assert!(value["error"].as_str().unwrap().contains("boom"));
}
#[test]
fn abi_cancelled_worker_reports_done_error_and_disposes_active_calls() {
    use std::sync::atomic::Ordering;
    let active = Arc::new(AtomicBool::new(false));
    let disposed = Arc::new(AtomicBool::new(false));
    let a = active.clone();
    let d = disposed.clone();
    let action: Arc<Action> = Arc::new(move |v| {
        Ok(match v["op"].as_str().unwrap() {
            "list" => {
                json!({"protocolVersion":1,"capabilities":{"resultEnvelope":1,"hostAccounting":1,"parentProgress":1,"losslessValues":1,"resultReferences":1,"contentAttachments":1},"tools":[{"name":"wait","description":"test","parameters":{}}],"nextOffset":null})
            }
            "start" => {
                a.store(true, Ordering::Release);
                json!({"requestId":"private","status":"running"})
            }
            "poll" => json!({"requestId":"private","status":"running"}),
            "release_result" => json!({"resultId":"private","status":"released"}),
            "cancel" => json!({"status":"cancelled"}),
            "dispose" => {
                d.store(true, Ordering::Release);
                json!({"status":"disposed"})
            }
            _ => panic!("unexpected action"),
        })
    });
    let token = Arc::new(AtomicBool::new(false));
    let cancel_token = token.clone();
    let (sender, receiver) = mpsc::channel();
    let worker = thread::spawn(move || {
        let _ = sender.send(Ok(engine::execute(
            "await tools.wait({});",
            10000,
            action,
            token,
        )));
    });
    let handle = JOBS.insert_with_cancel(
        Job {
            receiver,
            worker: Some(worker),
            completed: false,
        },
        cancel_token,
    );
    let until = Instant::now() + Duration::from_secs(2);
    while !active.load(Ordering::Acquire) {
        assert!(Instant::now() < until);
        thread::sleep(Duration::from_millis(1));
    }
    cancel(handle);
    let result = await_abi(handle);
    destroy(handle);
    let rpi_plugin_sdk::callbacks::Step::Done(text) = result else {
        panic!("cancel was not ABI Done: {result:?}");
    };
    let envelope: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(envelope["isError"], true);
    let value = &envelope["result"]["details"];
    assert_eq!(value["status"], "cancelled");
    assert_eq!(value["calls"][0]["status"], "cancelled");
    assert!(disposed.load(Ordering::Acquire));
    assert!(JOBS.get(handle).is_none());
}

#[test]
fn abi_capability_negotiation_fails_closed_for_execute_and_list_and_missing_binding() {
    let _serial = ABI_HOST_TEST_LOCK.lock().unwrap();
    rpi_plugin_sdk::string_handler!(fn fake_action(_action, input, out, _data) {
        let request: Value = serde_json::from_str(input).unwrap();
        assert_eq!(request["op"],"list","legacy host must not execute guest calls");
        let mut caps = json!({"resultEnvelope":1,"hostAccounting":1,"parentProgress":1,"losslessValues":1,"resultReferences":1,"contentAttachments":1});
        let key = request["__rpiRuntimeKey"].as_str().unwrap();
        if key != "good" { caps.as_object_mut().unwrap().remove(key); }
        out.write(json!({"protocolVersion":1,"capabilities":caps,"tools":[],"nextOffset":null}).to_string());
        0
    });
    for action in ["execute", "list"] {
        for key in [
            "resultEnvelope",
            "hostAccounting",
            "parentProgress",
            "losslessValues",
            "resultReferences",
            "contentAttachments",
            "good",
        ] {
            let previous = HOST.lock().unwrap().replace(RuntimeFns {
                action: fake_action,
                free: rpi_plugin_sdk::callbacks::free_string,
            });
            let params = json!({"action":action,"code":"throw Error('guest should not run');",
                "__rpi":{"runtimeHandle":1,"runtimeKey":key}});
            let handle = execute(
                StbStringRef::from_str("negotiation"),
                StbString::from_string(params.to_string()),
                Some(rpi_plugin_sdk::callbacks::free_string),
            );
            *HOST.lock().unwrap() = previous;
            let result = await_abi(handle);
            destroy(handle);
            match result {
                rpi_plugin_sdk::callbacks::Step::Err(text) if key != "good" => {
                    assert!(text.contains(key), "{text}")
                }
                rpi_plugin_sdk::callbacks::Step::Done(text) if key == "good" => {
                    let v: Value = serde_json::from_str(&text).unwrap();
                    assert_eq!(v["$rpiToolResult"], 1);
                    assert_eq!(v["isError"], action == "execute");
                    if action == "list" {
                        assert_eq!(v["result"]["details"]["capabilities"]["hostAccounting"], 1);
                    }
                }
                other => panic!("unexpected result {action}/{key}: {other:?}"),
            }
        }
    }
    let previous = HOST.lock().unwrap().take();
    let handle = execute(
        StbStringRef::from_str("missing-binding"),
        StbString::from_string(json!({"code":"return 1;"}).to_string()),
        Some(rpi_plugin_sdk::callbacks::free_string),
    );
    *HOST.lock().unwrap() = previous;
    let result = await_abi(handle);
    destroy(handle);
    assert!(matches!(result, rpi_plugin_sdk::callbacks::Step::Err(_)));
}

fn abi_summary(value: Value) -> Value {
    let (sender, receiver) = mpsc::channel();
    sender.send(Ok(value)).unwrap();
    let handle = JOBS.insert_with_cancel(
        Job {
            receiver,
            worker: None,
            completed: false,
        },
        Arc::new(AtomicBool::new(false)),
    );
    let result = await_abi(handle);
    destroy(handle);
    let rpi_plugin_sdk::callbacks::Step::Done(text) = result else {
        panic!("expected Done, got {result:?}")
    };
    assert!(text.len() <= engine::MAX_JSON);
    serde_json::from_str(&text).unwrap()
}

#[test]
fn abi_done_envelope_roundtrips_native_content_and_combined_error_termination() {
    for is_error in [false, true] {
        let v = abi_summary(
            json!({"status":"terminated","isError":is_error,"terminate":true,
            "result":{"content":{"promoted":true,"count":1}},
            "nativeContent":[{"type":"image","mimeType":"image/png","data":"AAAA"}]}),
        );
        assert_eq!(v["$rpiToolResult"], 1);
        assert_eq!(v["isError"], is_error);
        assert_eq!(v["result"]["terminate"], true);
        assert_eq!(v["result"]["content"][0]["type"], "image");
        assert!(v["result"]["details"].get("nativeContent").is_none());
    }
}

#[test]
fn abi_oversized_native_and_json_outputs_compact_to_done_error_preserving_termination() {
    for selected in [false, true] {
        for is_error in [false, true] {
            let mut value = json!({"status":"terminated","isError":is_error,"terminate":true,
                "result":"x".repeat(engine::MAX_JSON/2),"logs":["partial"],"calls":[]});
            if selected {
                value["nativeContent"] = json!([{"type":"image","mimeType":"image/png","data":"AAAA".repeat(engine::MAX_JSON/8)}]);
            }
            let v = abi_summary(value);
            assert_eq!(v["isError"], true);
            assert_eq!(v["result"]["terminate"], true);
            assert_eq!(v["result"]["details"]["status"], "error");
            assert_eq!(v["result"]["details"]["logs"][0], "partial");
        }
    }
}

#[test]
fn actual_binding_key_guard_covers_property_names_without_collision_or_overwrite() {
    let mut value = json!({"runtimeKey":"business","runtimeHandle":42,"__rpi":{"__rpiRuntimeKey":"business"},"prefix secret suffix":{"data":"hidden"},"prefix [redacted] suffix":{"data":"keep"},"nested":[{"secret":"credential-named","[redacted]":"keep","data":"prefix secret suffix"}]});
    redact(&mut value, "secret");
    assert!(!value.to_string().contains("secret"));
    assert_eq!(value["prefix [redacted] suffix"]["data"], "keep");
    assert_eq!(value["nested"][0]["[redacted]"], "keep");
    assert_eq!(value["nested"][0]["data"], "prefix [redacted] suffix");
    assert_eq!(value["runtimeKey"], "business");
    assert_eq!(value["runtimeHandle"], 42);
    assert_eq!(value["__rpi"]["__rpiRuntimeKey"], "business");
}

#[test]
fn bound_host_response_preserves_business_names_but_never_actual_credentials() {
    rpi_plugin_sdk::string_handler!(fn fake_action(_action, input, out, _data) {
        let request: Value = serde_json::from_str(input).unwrap();
        assert_eq!(request["__rpiRuntimeKey"], "actual-binding-key");
        out.write(json!({"protocolVersion":1,"capabilities":{"resultEnvelope":1,"hostAccounting":1,"parentProgress":1,"losslessValues":1,"resultReferences":1,"contentAttachments":1},"tools":[{"name":"fixture","description":"fixture","parameters":{"type":"object","properties":{"runtimeKey":{"type":"string"},"runtimeHandle":{"type":"number"},"__rpi":{"type":"object"},"__rpiRuntimeKey":{"type":"string"},"actual-binding-key":{"type":"string"},"[redacted]":{"type":"boolean"}},"nested":[{"runtimeKey":"business","text":"actual-binding-key"}]}}],"nextOffset":null}).to_string());
        0
    });
    let action = bind(
        Some(RuntimeFns {
            action: fake_action,
            free: rpi_plugin_sdk::callbacks::free_string,
        }),
        &json!({"__rpi":{"runtimeHandle":1,"runtimeKey":"actual-binding-key"}}),
    )
    .unwrap();
    let page = engine::list_page(&*action, 0, 32).unwrap();
    assert!(!page.to_string().contains("actual-binding-key"));
    let properties = &page["tools"][0]["parameters"]["properties"];
    assert_eq!(properties["runtimeKey"]["type"], "string");
    assert_eq!(properties["runtimeHandle"]["type"], "number");
    assert_eq!(properties["__rpi"]["type"], "object");
    assert_eq!(properties["__rpiRuntimeKey"]["type"], "string");
    assert_eq!(properties["[redacted]"]["type"], "boolean");
    assert_eq!(
        page["tools"][0]["parameters"]["nested"][0]["runtimeKey"],
        "business"
    );
}
