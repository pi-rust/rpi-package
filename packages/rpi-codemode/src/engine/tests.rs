use super::*;
use std::collections::HashMap;
use std::sync::Mutex;

#[derive(Default)]
struct Fake {
    events: Vec<Value>,
    jobs: HashMap<String, Value>,
    starts: usize,
    mode: String,
}
fn host(mode: &str) -> (Arc<Mutex<Fake>>, Arc<Action>) {
    let state = Arc::new(Mutex::new(Fake {
        mode: mode.into(),
        ..Fake::default()
    }));
    let shared = state.clone();
    let action: Arc<Action> = Arc::new(move |v| {
        let mut s = shared.lock().unwrap();
        s.events.push(v.clone());
        match v["op"].as_str().unwrap() {
            "list" => {
                if s.mode == "old" {
                    return Err("unknown runtime action 30".into());
                }
                Ok(
                    json!({"protocolVersion":1,"tools":[{"name":"echo","description":"test","parameters":{"type":"object"}},{"name":"other","description":"test","parameters":{"type":"object"}}],"nextOffset":null}),
                )
            }
            "start" => {
                s.starts += 1;
                if s.mode == "start_error" {
                    return Err("uncertain side effect".into());
                }
                let id = format!("host-private-id-{}", s.starts);
                s.jobs.insert(id.clone(), v["arguments"].clone());
                Ok(
                    json!({"requestId":id,"status":if s.mode == "start_status" {"bad"} else {"running"}}),
                )
            }
            "poll" => {
                let id = v["requestId"].as_str().unwrap();
                if s.mode == "poll_error" {
                    return Err("bridge broke".into());
                }
                if s.mode == "wait" {
                    return Ok(json!({"requestId":id,"status":"running"}));
                }
                if s.mode == "concurrent" {
                    assert!(s.starts >= 2, "first completion before second start");
                }
                if s.mode == "failed" {
                    return Ok(
                        json!({"requestId":id,"status":"failed","error":"host failed after writing"}),
                    );
                }
                if s.mode == "bad_poll" {
                    return Ok(json!({"requestId":id,"status":"wat"}));
                }
                if s.mode == "big_host" {
                    return Ok(
                        json!({"requestId":id,"status":"done","result":{"details":"x".repeat(MAX_JSON)}}),
                    );
                }
                let args = s.jobs.get(id).unwrap();
                Ok(
                    json!({"requestId":id,"status":"done","isError":matches!(s.mode.as_str(), "isError" | "terminate_error"),
                    "result":{"content":[{"type":"text","text":"done"}],"details":args,
                    "terminate":matches!(s.mode.as_str(), "terminate" | "terminate_error"),"addedToolNames":["new"],"usage":{"cost":1},
                    "__rpi":{"runtimeKey":"secret"},"runtimeHandle":42}}),
                )
            }
            "cancel" => Ok(json!({"requestId":v["requestId"],"status":"cancelled"})),
            "dispose" => {
                s.jobs.remove(v["requestId"].as_str().unwrap());
                Ok(json!({"requestId":v["requestId"],"status":"disposed"}))
            }
            _ => panic!("unexpected operation"),
        }
    });
    (state, action)
}
fn run_fake(code: &str, mode: &str) -> (Value, Arc<Mutex<Fake>>) {
    let (host, action) = host(mode);
    let result = execute(code, 2000, action, Arc::new(AtomicBool::new(false)));
    (result, host)
}
fn cleaned(s: &Arc<Mutex<Fake>>, starts: usize) {
    let s = s.lock().unwrap();
    assert!(s.jobs.is_empty(), "leaked host jobs: {:?}", s.events);
    assert_eq!(s.starts, starts);
    assert_eq!(
        s.events.iter().filter(|e| e["op"] == "dispose").count(),
        starts
    );
}

#[test]
fn sequential_data_dependencies_and_full_result() {
    let (r,h) = run_fake("const first = await tools.echo({value:7}); return await tools.call('other',{value:first.details.value+1});","");
    assert_eq!(r["status"], "completed", "{r}");
    assert_eq!(r["result"]["details"]["value"], 8);
    assert_eq!(r["result"]["usage"]["cost"], 1);
    assert_eq!(r["result"]["addedToolNames"], json!(["new"]));
    cleaned(&h, 2);
}
#[test]
fn promise_all_really_starts_both_before_first_poll() {
    let (r, h) = run_fake(
        "return await Promise.all([tools.echo({n:1}),tools.other({n:2})]);",
        "concurrent",
    );
    assert_eq!(r["status"], "completed", "{r}");
    assert_eq!(r["result"][0]["details"]["n"], 1);
    let s = h.lock().unwrap();
    let first_poll = s.events.iter().position(|e| e["op"] == "poll").unwrap();
    assert_eq!(
        s.events[..first_poll]
            .iter()
            .filter(|e| e["op"] == "start")
            .count(),
        2
    );
    drop(s);
    cleaned(&h, 2);
}
#[test]
fn errors_preserve_partial_side_effects_and_cleanup_siblings() {
    for mode in [
        "isError",
        "failed",
        "poll_error",
        "bad_poll",
        "start_status",
        "big_host",
    ] {
        let (r, h) = run_fake(
            "return await Promise.all([tools.echo({write:1}),tools.other({write:2})]);",
            mode,
        );
        assert_eq!(r["status"], "error", "{mode}: {r}");
        cleaned(&h, if mode == "start_status" { 1 } else { 2 });
    }
    let (r, h) = run_fake(
        "await tools.echo({write:1}); throw new Error('after side effect');",
        "",
    );
    assert_eq!(r["status"], "error");
    assert!(r["error"].as_str().unwrap().contains("after side effect"));
    cleaned(&h, 1);
}
#[test]
fn is_error_can_be_caught_but_is_recorded() {
    let (r, h) = run_fake(
        "try { await tools.echo({}); } catch(e) { return e.message; }",
        "isError",
    );
    assert_eq!(r["status"], "completed", "{r}");
    assert!(r["result"].as_str().unwrap().contains("isError"));
    assert_eq!(r["calls"][0]["isError"], true);
    cleaned(&h, 1);
}
#[test]
fn terminate_stops_program_and_cleans_other_calls() {
    let (r, h) = run_fake(
        "await Promise.all([tools.echo({}),tools.other({})]); throw new Error('must not resume');",
        "terminate",
    );
    assert_eq!(r["status"], "terminated", "{r}");
    assert_eq!(r["terminate"], true);
    cleaned(&h, 2);
}
#[test]
fn js_failure_and_unawaited_success_cancel_outstanding_calls() {
    for code in [
        "tools.echo({}); throw new Error('boom');",
        "tools.echo({}); return 4;",
    ] {
        let (r, h) = run_fake(code, "wait");
        assert!(r["status"] == "error" || r["status"] == "completed");
        cleaned(&h, 1);
        assert_eq!(
            h.lock()
                .unwrap()
                .events
                .iter()
                .filter(|e| e["op"] == "cancel")
                .count(),
            1
        );
    }
}
#[test]
fn no_privileged_globals_or_host_metadata_and_fresh_state() {
    let code="const r=await tools.echo({}); return {globals:[typeof process,typeof require,typeof fetch,typeof fs,typeof __rpi],r,catalog:tools.list()};";
    let (r, h) = run_fake(code, "");
    assert_eq!(r["status"], "completed", "{r}");
    assert_eq!(
        r["result"]["globals"],
        json!([
            "undefined",
            "undefined",
            "undefined",
            "undefined",
            "undefined"
        ])
    );
    let text = r.to_string();
    for forbidden in [
        "runtimeKey",
        "runtimeHandle",
        "host-private-id",
        "__rpi",
        "secret",
    ] {
        assert!(!text.contains(forbidden), "{text}");
    }
    cleaned(&h, 1);
    assert_eq!(
        run_fake("globalThis.leak=1; return 1;", "").0["status"],
        "completed"
    );
    assert_eq!(run_fake("return typeof leak;", "").0["result"], "undefined");
}
#[test]
fn invalid_arguments_and_reserved_metadata_never_reach_host() {
    for code in [
        "await tools.echo(null)",
        "await tools.echo([])",
        "await tools.call(3,{})",
        "await tools.call('absent',{})",
        "await tools.echo({runtimeKey:'fake'})",
        "await tools.echo({x:{__rpi:{a:1}}})",
    ] {
        let (r, h) = run_fake(code, "");
        assert_eq!(r["status"], "error", "{code}: {r}");
        cleaned(&h, 0);
    }
}
#[test]
fn bounded_code_arguments_result_logs_calls_memory_and_stack() {
    let (r, h) = run_fake(&" ".repeat(MAX_CODE + 1), "");
    assert_eq!(r["status"], "error");
    cleaned(&h, 0);
    for (code, expected) in [
        ("return 'x'.repeat(1048577);", 0),
        ("print('x'.repeat(65537));", 0),
        ("for(let i=0;i<257;i++) console.log(i);", 0),
        ("await tools.echo({x:'x'.repeat(1048576)});", 0),
        (
            "await Promise.all(Array.from({length:9},()=>tools.echo({})));",
            8,
        ),
        ("for(let i=0;i<129;i++) await tools.echo({});", 128),
        (
            "tools.echo({}); const a=[]; for(;;) a.push(new Array(10000).fill('memory'));",
            1,
        ),
        ("tools.echo({}); function f(){return f()+1;} f();", 1),
        ("return {get x(){throw new Error('serialization')}};", 0),
        ("const x={};x.x=x;return x;", 0),
    ] {
        let (r, h) = run_fake(code, "");
        assert_eq!(r["status"], "error", "{code}: {r}");
        cleaned(&h, expected);
    }
}
#[test]
fn logging_and_undefined_return() {
    let (r, _) = run_fake(
        "print('hello', {n:1}); console.info('i'); console.warn('w'); console.error('e');",
        "",
    );
    assert_eq!(r["status"], "completed", "{r}");
    assert_eq!(r["result"], Value::Null);
    assert_eq!(r["logs"][0]["text"], "hello {\"n\":1}");
    assert_eq!(r["logs"].as_array().unwrap().len(), 4);
}
#[test]
fn pending_promise_without_source_fails_promptly() {
    let (r, _) = run_fake("await new Promise(()=>{});", "");
    assert_eq!(r["status"], "error", "{r}");
    assert!(r["error"].as_str().unwrap().contains("no runnable jobs"));
}
#[test]
fn timeouts_interrupt_sync_and_microtask_loops_and_cleanup() {
    for code in [
        "tools.echo({}); for(;;){}",
        "tools.echo({}); while(true) await Promise.resolve();",
        "await tools.echo({});",
    ] {
        let (h, a) = host("wait");
        let start = Instant::now();
        let r = execute(code, 50, a, Arc::new(AtomicBool::new(false)));
        assert_eq!(r["status"], "timed_out", "{code}: {r}");
        assert!(start.elapsed() < Duration::from_secs(2));
        cleaned(&h, 1);
    }
}
#[test]
fn cancellation_interrupts_sync_and_microtask_loops() {
    for code in [
        "tools.echo({}); for(;;){}",
        "tools.echo({}); while(true) await Promise.resolve();",
        "await tools.echo({});",
    ] {
        let (h, a) = host("wait");
        let cancel = Arc::new(AtomicBool::new(false));
        let token = cancel.clone();
        let worker = std::thread::spawn(move || execute(code, 2000, a, token));
        let until = Instant::now() + Duration::from_secs(1);
        while h.lock().unwrap().starts == 0 && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(1));
        }
        cancel.store(true, Ordering::Release);
        let r = worker.join().unwrap();
        assert_eq!(r["status"], "cancelled", "{code}: {r}");
        cleaned(&h, 1);
    }
}
#[test]
fn old_host_and_uncertain_start_fail_explicitly_without_retry() {
    let (r, h) = run_fake("return 1;", "old");
    assert_eq!(r["status"], "error");
    assert!(r["error"]
        .as_str()
        .unwrap()
        .contains("no direct-call fallback"));
    cleaned(&h, 0);
    let (r, h) = run_fake("await tools.echo({});", "start_error");
    assert_eq!(r["status"], "error");
    let s = h.lock().unwrap();
    assert_eq!(s.starts, 1);
    assert_eq!(s.events.iter().filter(|e| e["op"] == "start").count(), 1);
}
#[test]
fn parser_is_strict_but_accepts_host_and_unknown_fields() {
    assert!(matches!(
        parse_request(&json!({"code":"return 1","__rpi":{"runtimeKey":"secret"},"unknown":true})),
        Ok(Request::Execute {
            timeout_ms: 30000,
            ..
        })
    ));
    for v in [
        json!(null),
        json!({}),
        json!({"action":null}),
        json!({"action":"bad"}),
        json!({"code":" "}),
        json!({"code":1}),
        json!({"code":"x","timeoutMs":0}),
        json!({"code":"x","timeoutMs":120001}),
        json!({"code":"x","timeoutMs":1.5}),
        json!({"action":"list","offset":-1}),
        json!({"action":"list","limit":0}),
    ] {
        assert!(parse_request(&v).is_err(), "{v}");
    }
    assert!(matches!(
        parse_request(&json!({"action":"list"})),
        Ok(Request::List {
            offset: 0,
            limit: 32
        })
    ));
}
#[test]
fn paginated_catalog_is_bounded_and_validated() {
    let action = |v: Value| {
        Ok(
            json!({"protocolVersion":1,"tools":[],"nextOffset":if v["offset"]==0 {json!(32)} else {Value::Null}}),
        )
    };
    assert_eq!(list_page(&action, 0, 32).unwrap()["nextOffset"], 32);
    for page in [
        json!({"protocolVersion":0,"tools":[],"nextOffset":null}),
        json!({"protocolVersion":1,"tools":[],"nextOffset":0}),
        json!({"protocolVersion":1,"tools":[{}],"nextOffset":null}),
    ] {
        assert!(list_page(&move |_| Ok(page.clone()), 0, 32).is_err());
    }
}

#[test]
fn catching_budget_errors_does_not_restore_success() {
    for code in [
        "try { print('x'.repeat(65537)); } catch(e) {} return 1;",
        "try { await tools.echo({x:'x'.repeat(1048576)}); } catch(e) {} return 1;",
        "try { for(let i=0;i<129;i++) await tools.echo({}); } catch(e) {} return 1;",
    ] {
        let (r, h) = run_fake(code, "");
        assert_eq!(r["status"], "error", "{r}");
        assert!(h.lock().unwrap().jobs.is_empty());
    }
}
#[test]
fn import_cannot_load_files_or_native_modules() {
    let (r, _) = run_fake("return await import('std');", "");
    assert_eq!(r["status"], "error", "{r}");
    let (r, _) = run_fake("return await import('file:///etc/passwd');", "");
    assert_eq!(r["status"], "error", "{r}");
}
#[test]
fn cancellation_before_execution_never_lists_or_starts() {
    let (h, a) = host("");
    let r = execute("return 1;", 1000, a, Arc::new(AtomicBool::new(true)));
    assert_eq!(r["status"], "cancelled");
    assert!(h.lock().unwrap().events.is_empty());
}
#[test]
fn paginated_execute_discovers_only_bounded_contract_entries() {
    let events = Arc::new(Mutex::new(Vec::new()));
    let shared = events.clone();
    let a: Arc<Action> = Arc::new(move |v| {
        shared.lock().unwrap().push(v.clone());
        let offset = v["offset"].as_u64().unwrap();
        let entries = if offset == 0 {
            vec![json!({"name":"a","description":"first","parameters":{},"internal":"hidden"})]
        } else {
            vec![json!({"name":"b","description":"second","parameters":{}})]
        };
        Ok(
            json!({"protocolVersion":1,"tools":entries,"nextOffset":if offset==0 {json!(1)} else {Value::Null}}),
        )
    });
    let r = execute(
        "return tools.list();",
        1000,
        a,
        Arc::new(AtomicBool::new(false)),
    );
    assert_eq!(r["status"], "completed", "{r}");
    assert_eq!(r["result"].as_array().unwrap().len(), 2);
    assert!(r["result"][0].get("internal").is_none());
    assert_eq!(events.lock().unwrap().len(), 2);
}

#[test]
fn lifecycle_failure_cannot_be_caught_as_success() {
    let (r, h) = run_fake(
        "try { await tools.echo({}); } catch(e) {} return 1;",
        "failed",
    );
    assert_eq!(r["status"], "error", "{r}");
    assert!(r["error"]
        .as_str()
        .unwrap()
        .contains("host lifecycle failed"));
    cleaned(&h, 1);
}
#[test]
fn captured_intrinsics_survive_guest_mutation_and_functions_are_frozen() {
    let (r,h) = run_fake("const frozen = Object.isFrozen(tools.echo) && Object.isFrozen(tools.call); Map.prototype.set = Map.prototype.get = Map.prototype.delete = () => {throw Error('poison');}; JSON.parse = JSON.stringify = () => {throw Error('poison');}; Array.isArray = () => true; Array.prototype.map = () => {throw Error('poison');}; const r = await tools.echo({n:7}); print('still', {working:true}); return {frozen,n:r.details.n};", "");
    assert_eq!(r["status"], "completed", "{r}");
    assert_eq!(r["result"], json!({"frozen":true,"n":7}));
    cleaned(&h, 1);
}
#[test]
fn terminal_result_with_is_error_still_terminates_without_resuming() {
    let (r, h) = run_fake(
        "try { await tools.echo({}); } catch(e) {} throw Error('resumed');",
        "terminate_error",
    );
    assert_eq!(r["status"], "terminated", "{r}");
    assert_eq!(r["calls"][0]["isError"], true);
    cleaned(&h, 1);
}
#[test]
fn serialization_cannot_catch_budget_failure_and_report_success() {
    let (r, _) = run_fake(
        "return {toJSON(){try {print('x'.repeat(65537));}catch(e){} return 1;}};",
        "",
    );
    assert_eq!(r["status"], "error", "{r}");
}

#[test]
fn promise_job_exception_aborts_even_if_main_function_returned() {
    // Repeat both Error and null throws to exercise wrapper refcount balancing,
    // including exceptions that cannot be detected by checking catch() != null.
    for throw in ["throw new Error('job exploded');", "throw null;"] {
        for _ in 0..4 {
            let code = format!("tools.echo({{}}); const p = Promise.resolve(1); p.constructor = {{[Symbol.species]: function(executor) {{ executor(() => {{{throw}}}, () => {{}}); }}}}; p.then(() => 1); return 42;");
            let (r, h) = run_fake(&code, "wait");
            assert_eq!(r["status"], "error", "{r}");
            if throw.contains("Error") {
                assert!(r["error"].as_str().unwrap().contains("job exploded"), "{r}");
            }
            cleaned(&h, 1);
        }
    }
}

#[test]
fn catalog_filtering_is_explicitly_lossy_for_reserved_schema_properties() {
    let action = |_| {
        Ok(
            json!({"protocolVersion":1,"tools":[{"name":"schema","description":"test","parameters":{"type":"object","properties":{"runtimeKey":{"type":"string"},"ordinary":{"type":"string"}}}}],"nextOffset":null}),
        )
    };
    let page = list_page(&action, 0, 32).unwrap();
    assert!(page["tools"][0]["parameters"]["properties"]
        .get("runtimeKey")
        .is_none());
    assert_eq!(
        page["tools"][0]["parameters"]["properties"]["ordinary"]["type"],
        "string"
    );
    assert!(list_page(&action, 1024, 32).unwrap_err().contains("1024"));
}
