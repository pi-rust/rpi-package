use super::*;
use rpi_plugin_sdk::StepResultTag;
use std::time::{Duration, Instant};

#[test]
fn registration_schema_is_canonical_and_allows_host_injection() {
    let schema = schema();
    assert_eq!(schema.name.to_string_lossy(), "codemode");
    let v: Value = serde_json::from_str(&schema.parameters.to_string_lossy()).unwrap();
    assert_eq!(v["type"], "object");
    assert_eq!(v["x-rpi-execution-mode"], "sequential");
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
fn output_preserves_summary_and_terminate() {
    for status in ["completed", "terminated"] {
        let v: Value = serde_json::from_str(
            &tool_result(json!({"status":status,"terminate":status == "terminated","calls":[]}))
                .unwrap(),
        )
        .unwrap();
        assert_eq!(v["terminate"], status == "terminated");
        assert_eq!(v["details"]["status"], status);
    }
    for status in ["error", "cancelled", "timed_out"] {
        let text =
            tool_result(json!({"status":status,"logs":["partial"],"calls":[{"name":"write"}]}))
                .unwrap_err();
        let v: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(v["status"], status);
        assert_eq!(v["logs"][0], "partial");
        assert_eq!(v["calls"][0]["name"], "write");
    }
    let mut value = json!({"text":"prefix secret suffix","__rpi":{"runtimeHandle":1},"nested":{"runtimeKey":"secret"}});
    redact(&mut value, "secret");
    assert!(!value.to_string().contains("secret"));
    assert!(!value.to_string().contains("runtimeHandle"));
}
#[test]
fn abi_poll_is_nonblocking_cancel_is_signal_only_destroy_joins() {
    let cancelled = Arc::new(AtomicBool::new(false));
    let token = cancelled.clone();
    let (sender, receiver) = mpsc::channel();
    let worker = thread::spawn(move || {
        let action: Arc<Action> =
            Arc::new(|_| Ok(json!({"protocolVersion":1,"tools":[],"nextOffset":null})));
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
    ).unwrap_err();
    assert!(text.len() <= engine::MAX_JSON);
    let r: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(r["terminate"], true);
    assert_eq!(r["result"], Value::Null);
    assert_eq!(r["calls"][0]["name"], "write");
    assert!(r["error"]
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
fn abi_failed_execution_is_err_not_done_even_with_partial_work() {
    rpi_plugin_sdk::string_handler!(fn fake_action(_action, input, out, _data) {
        let request: Value = serde_json::from_str(input).unwrap();
        assert_eq!(request["__rpiRuntimeKey"], "abi-test-key");
        out.write(json!({"protocolVersion":1,"tools":[],"nextOffset":null}).to_string());
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
    let rpi_plugin_sdk::callbacks::Step::Err(text) = result else {
        panic!("failure was not ABI Err: {result:?}");
    };
    let value: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(value["status"], "error");
    assert_eq!(value["logs"][0]["text"], "partial");
    assert!(value["error"].as_str().unwrap().contains("boom"));
}
#[test]
fn abi_cancelled_worker_reports_err_and_disposes_active_calls() {
    use std::sync::atomic::Ordering;
    let active = Arc::new(AtomicBool::new(false));
    let disposed = Arc::new(AtomicBool::new(false));
    let a = active.clone();
    let d = disposed.clone();
    let action: Arc<Action> = Arc::new(move |v| {
        Ok(match v["op"].as_str().unwrap() {
            "list" => {
                json!({"protocolVersion":1,"tools":[{"name":"wait","description":"test","parameters":{}}],"nextOffset":null})
            }
            "start" => {
                a.store(true, Ordering::Release);
                json!({"requestId":"private","status":"running"})
            }
            "poll" => json!({"requestId":"private","status":"running"}),
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
    let rpi_plugin_sdk::callbacks::Step::Err(text) = result else {
        panic!("cancel was not ABI Err: {result:?}");
    };
    let value: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(value["status"], "cancelled");
    assert_eq!(value["calls"][0]["status"], "cancelled");
    assert!(disposed.load(Ordering::Acquire));
    assert!(JOBS.get(handle).is_none());
}
