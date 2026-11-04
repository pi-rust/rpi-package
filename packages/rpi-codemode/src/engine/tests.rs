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
                    json!({"protocolVersion":1,"capabilities":{"resultEnvelope":1,"hostAccounting":1,"parentProgress":1,"losslessValues":1,"resultReferences":1,"contentAttachments":1},"tools":[{"name":"echo","description":"test","parameters":{"type":"object"}},{"name":"other","description":"test","parameters":{"type":"object"}}],"nextOffset":null}),
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
fn no_privileged_globals_or_host_identities_and_fresh_state() {
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
    assert!(!text.contains("host-private-id"), "{text}");
    // These fixture fields are ordinary business JSON, not actual credentials.
    assert_eq!(r["result"]["r"]["__rpi"]["runtimeKey"], "secret");
    assert_eq!(r["result"]["r"]["runtimeHandle"], 42);
    cleaned(&h, 1);
    assert_eq!(
        run_fake("globalThis.leak=1; return 1;", "").0["status"],
        "completed"
    );
    assert_eq!(run_fake("return typeof leak;", "").0["result"], "undefined");
}
#[test]
fn invalid_arguments_never_reach_host() {
    for code in [
        "await tools.echo(null)",
        "await tools.echo([])",
        "await tools.call(3,{})",
        "await tools.call('absent',{})",
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
            json!({"protocolVersion":1,"capabilities":{"resultEnvelope":1,"hostAccounting":1,"parentProgress":1,"losslessValues":1,"resultReferences":1,"contentAttachments":1},"tools":[],"nextOffset":if v["offset"]==0 {json!(32)} else {Value::Null}}),
        )
    };
    assert_eq!(list_page(&action, 0, 32).unwrap()["nextOffset"], 32);
    for page in [
        json!({"protocolVersion":0,"tools":[],"nextOffset":null}),
        json!({"protocolVersion":1,"capabilities":{"resultEnvelope":1,"hostAccounting":1,"parentProgress":1,"losslessValues":1,"resultReferences":1,"contentAttachments":1},"tools":[],"nextOffset":0}),
        json!({"protocolVersion":1,"capabilities":{"resultEnvelope":1,"hostAccounting":1,"parentProgress":1,"losslessValues":1,"resultReferences":1,"contentAttachments":1},"tools":[{}],"nextOffset":null}),
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
            json!({"protocolVersion":1,"capabilities":{"resultEnvelope":1,"hostAccounting":1,"parentProgress":1,"losslessValues":1,"resultReferences":1,"contentAttachments":1},"tools":entries,"nextOffset":if offset==0 {json!(1)} else {Value::Null}}),
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
fn catalog_preserves_business_schema_properties() {
    let action = |_| {
        Ok(
            json!({"protocolVersion":1,"capabilities":{"resultEnvelope":1,"hostAccounting":1,"parentProgress":1,"losslessValues":1,"resultReferences":1,"contentAttachments":1},"tools":[{"name":"schema","description":"test","parameters":{"type":"object","properties":{"runtimeKey":{"type":"string"},"ordinary":{"type":"string"}}}}],"nextOffset":null}),
        )
    };
    let page = list_page(&action, 0, 32).unwrap();
    assert_eq!(
        page["tools"][0]["parameters"]["properties"]["runtimeKey"],
        json!({"type":"string"})
    );
    assert_eq!(
        page["tools"][0]["parameters"]["properties"]["ordinary"]["type"],
        "string"
    );
    assert!(list_page(&action, 1024, 32).unwrap_err().contains("1024"));
}

#[test]
fn native_regexp_paths_are_unavailable_including_dynamic_compilation() {
    // Do not only remove globalThis.RegExp: literals, dynamic compilation and
    // implicit string conversion must never enter the native backtracking VM.
    for code in [
        "return /^(a+)+$/.test('a'.repeat(25)+'!');",
        "return new RegExp('^(a+)+$').test('a'.repeat(25)+'!');",
        "return RegExp('^(a+)+$').test('a'.repeat(25)+'!');",
        "return eval(\"/^(a+)+$/.test('a'.repeat(25)+'!')\");",
        "return new Function(\"return /^(a+)+$/.test('a'.repeat(25)+'!');\")();",
        "return ('a'.repeat(25)+'!').match('^(a+)+$');",
        "return ('a'.repeat(25)+'!').search('^(a+)+$');",
        "return [...('a'.repeat(25)+'!').matchAll('^(a+)+$')];",
    ] {
        let began = Instant::now();
        let (r, h) = run_fake(code, "");
        assert_eq!(r["status"], "error", "{code}: {r}");
        assert!(
            began.elapsed() < Duration::from_secs(1),
            "regex path ran native backtracking: {code}: {r}"
        );
        cleaned(&h, 0);
    }
    let (r, h) = run_fake("return {regexp:typeof RegExp, division:8/2, replace:'abc'.replace('a','x'), split:'a,b'.split(','), map:new Map([['a',1]]).get('a')};", "");
    assert_eq!(r["status"], "completed", "{r}");
    assert_eq!(
        r["result"],
        json!({"regexp":"undefined","division":4,"replace":"xbc","split":["a","b"],"map":1})
    );
    cleaned(&h, 0);
}

#[test]
fn caught_oom_is_fatal_before_host_calls_and_during_serialization() {
    let exhaust = "try { const a=[]; for(;;) a.push(new Array(10000).fill('x')); } catch(e) {}";
    for (code, starts) in [
        (format!("{exhaust} return 42;"), 0),
        (
            format!("{exhaust} await tools.echo({{afterOOM:true}}); return 42;"),
            0,
        ),
        (
            format!("tools.echo({{beforeOOM:true}}); {exhaust} return 42;"),
            1,
        ),
        (format!("return {{toJSON(){{{exhaust} return 42;}}}};"), 0),
    ] {
        let (h, action) = host("");
        let r = execute(&code, 5000, action, Arc::new(AtomicBool::new(false)));
        assert_eq!(r["status"], "error", "{r}");
        assert_eq!(r["result"], Value::Null, "{r}");
        assert_eq!(r["error"], "codemode memory budget exceeded", "{r}");
        cleaned(&h, starts);
    }
}

const PNG: &str =
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAusB9Wl6N3sAAAAASUVORK5CYII=";

fn native_host(result: Value, is_error: bool) -> Arc<Action> {
    Arc::new(move |v| {
        Ok(match v["op"].as_str().unwrap() {
            "list" => {
                json!({"protocolVersion":1,"capabilities":{"resultEnvelope":1,"hostAccounting":1,"parentProgress":1,"losslessValues":1,"resultReferences":1,"contentAttachments":1},
            "tools":[{"name":"read","description":"fixture","parameters":{}}],"nextOffset":null})
            }
            "start" => json!({"requestId":"private","status":"running"}),
            "poll" => {
                json!({"requestId":"private","status":"done","isError":is_error,"result":result})
            }
            "dispose" | "cancel" => json!({}),
            _ => panic!("unexpected action"),
        })
    })
}
fn native_run(code: &str, result: Value, is_error: bool) -> Value {
    execute(
        code,
        2000,
        native_host(result, is_error),
        Arc::new(AtomicBool::new(false)),
    )
}
fn image_result() -> Value {
    json!({"content":[{"type":"text","text":"tiny"},{"type":"image","mimeType":"image/png","data":PNG}],
        "details":{"ok":true},"usage":{"cost":1},"addedToolNames":["new"],"terminate":false})
}

#[test]
fn capabilities_require_all_six_exact_integer_versions_before_guest_execution() {
    for key in [
        "resultEnvelope",
        "hostAccounting",
        "parentProgress",
        "losslessValues",
        "resultReferences",
        "contentAttachments",
    ] {
        for invalid in [
            Value::Null,
            json!(0),
            json!(2),
            json!("1"),
            json!(true),
            json!(1.0),
        ] {
            let mut caps = json!({"resultEnvelope":1,"hostAccounting":1,"parentProgress":1,"losslessValues":1,"resultReferences":1,"contentAttachments":1});
            caps[key] = invalid;
            let a: Arc<Action> = Arc::new(move |v| {
                assert_eq!(v["op"], "list", "guest must never start");
                Ok(json!({"protocolVersion":1,"capabilities":caps,"tools":[],"nextOffset":null}))
            });
            assert!(list_page(&*a, 0, 32).unwrap_err().contains(key));
            let r = execute(
                "throw Error('guest ran');",
                2000,
                a,
                Arc::new(AtomicBool::new(false)),
            );
            assert!(r["error"].as_str().unwrap().contains(key), "{r}");
        }
    }
    let a = |_| Ok(json!({"protocolVersion":1,"tools":[],"nextOffset":null}));
    assert!(list_page(&a, 0, 32).unwrap_err().contains("upgrade"));
}

#[test]
fn list_preserves_only_public_capabilities() {
    let a = |_| {
        Ok(json!({"protocolVersion":1,"tools":[],"nextOffset":null,
        "capabilities":{"resultEnvelope":1,"hostAccounting":1,"parentProgress":1,"losslessValues":1,"resultReferences":1,"contentAttachments":1,"private":"hidden"},"private":"hidden"}))
    };
    let page = list_page(&a, 0, 32).unwrap();
    assert_eq!(
        page["capabilities"],
        json!({"resultEnvelope":1,"hostAccounting":1,"parentProgress":1,"losslessValues":1,"resultReferences":1,"contentAttachments":1})
    );
    assert!(!page.to_string().contains("hidden"));
}

#[test]
fn returning_native_result_promotes_blocks_without_base64_duplication_or_accounting() {
    let r = native_run("return await tools.read({});", image_result(), false);
    assert_eq!(r["status"], "completed", "{r}");
    assert_eq!(r["result"]["content"], json!({"promoted":true,"count":2}));
    assert_eq!(r["nativeContent"][1]["data"], PNG);
    let output: Value = serde_json::from_str(&crate::tool_result(r)).unwrap();
    assert_eq!(output["isError"], false);
    assert_eq!(output["result"]["content"][1]["data"], PNG);
    assert_eq!(output.to_string().matches(PNG).count(), 1);
    assert!(output["result"].get("usage").is_none());
    assert!(output["result"].get("addedToolNames").is_none());
}

#[test]
fn explicit_emit_selects_intermediates_while_returning_summary_and_keeps_order() {
    let r = native_run("const r=await tools.read({}); tools.emit(r); tools.emit([{type:'text',text:'last'}]); return {ok:r.details.ok};",image_result(),false);
    assert_eq!(r["status"], "completed", "{r}");
    assert_eq!(r["result"], json!({"ok":true}));
    assert_eq!(r["nativeContent"][2]["text"], "last");
    assert_eq!(r.to_string().matches(PNG).count(), 1);
}

#[test]
fn ordinary_json_and_intermediate_images_are_not_auto_forwarded_or_reinterpreted() {
    let r = native_run(
        "await tools.read({}); return {answer:42,nested:{content:[{type:'unknown'}]}};",
        image_result(),
        false,
    );
    assert_eq!(r["status"], "completed", "{r}");
    assert!(r.get("nativeContent").is_none());
    assert!(!r.to_string().contains(PNG));
    assert_eq!(r["result"]["nested"]["content"][0]["type"], "unknown");
    let r = native_run(
        "return [{type:'image',mimeType:'image/png',data:'AAAA'}];",
        image_result(),
        false,
    );
    assert!(
        r.get("nativeContent").is_none(),
        "arrays need explicit tools.emit"
    );
}

#[test]
fn per_call_raw_content_and_error_result_are_available_before_output_selection() {
    for error in [false, true] {
        let code = if error {
            "try {await tools.read({});} catch(e) {return {data:e.result.content[1].data,cost:e.result.usage.cost};}"
        } else {
            "const r=await tools.read({}); return {data:r.content[1].data,cost:r.usage.cost};"
        };
        let r = native_run(code, image_result(), error);
        assert_eq!(r["status"], "completed", "{r}");
        assert_eq!(r["result"]["data"], PNG);
        assert_eq!(r["result"]["cost"], 1);
    }
}

#[test]
fn malformed_native_blocks_fail_explicitly_for_both_return_and_emit() {
    for content in [
        json!(null),
        json!({}),
        json!([{}]),
        json!([{"type":"text","text":7}]),
        json!([{"type":"text","text":"ok","textSignature":7}]),
        json!([{"type":"text","text":"ok","textSignature":false}]),
        json!([{"type":"text","text":"ok","textSignature":{}}]),
        json!([{"type":"text","text":"ok","textSignature":[]}]),
        json!([{"type":"audio","data":"AAAA"}]),
        json!([{"type":"image","mimeType":"text/plain","data":"AAAA"}]),
        json!([{"type":"image","mimeType":"image/svg+xml","data":"AAAA"}]),
        json!([{"type":"image","mimeType":"image/png"}]),
        json!([{"type":"image","mimeType":"image/png","data":""}]),
        json!([{"type":"image","mimeType":"image/png","data":"AB=="}]),
        json!([{"type":"image","mimeType":"image/png","data":"AA=A"}]),
        json!([{"type":"image","mimeType":"image/png","data":"AAAA\n"}]),
    ] {
        for code in [
            "return await tools.read({});",
            "const r=await tools.read({}); try {tools.emit(r);} catch(e){} return 42;",
        ] {
            let r = native_run(code, json!({"content":content}), false);
            assert_eq!(r["status"], "error", "{content}: {r}");
            assert_eq!(r["isError"], true);
        }
    }
}

#[test]
fn invalid_text_signature_becomes_structured_done_error_with_logs() {
    for code in [
        "print('before invalid signature'); return await tools.read({});",
        "print('before invalid signature'); try {tools.emit(await tools.read({}));} catch(e) {} return 42;",
    ] {
        let r = native_run(code, json!({"content":[{"type":"text","text":"ok","textSignature":7}]}), false);
        let output: Value = serde_json::from_str(&crate::tool_result(r)).unwrap();
        assert_eq!(output["$rpiToolResult"], 1);
        assert_eq!(output["isError"], true);
        assert_eq!(output["result"]["details"]["status"], "error");
        assert_eq!(output["result"]["details"]["logs"][0]["text"], "before invalid signature");
        assert_eq!(output["result"]["details"]["error"], "native textSignature must be null or a string");
        assert!(output["result"]["content"][0]["text"].as_str().unwrap().contains("textSignature"));
    }
}

#[test]
fn native_blocks_reject_unsupported_fields_instead_of_silently_dropping_them() {
    let r = native_run(
        "try {tools.emit([{type:'text',text:'ok',runtimeKey:'business'}]);} catch(e){} return 1;",
        image_result(),
        false,
    );
    assert_eq!(r["status"], "error", "{r}");
    assert_eq!(r["error"], "unsupported native content block metadata");
}

#[test]
fn returned_native_text_signatures_accept_only_absent_null_or_string() {
    for block in [
        json!({"type":"text","text":"ok"}),
        json!({"type":"text","text":"ok","textSignature":null}),
        json!({"type":"text","text":"ok","textSignature":"native-signature"}),
    ] {
        for code in [
            "return await tools.read({});",
            "tools.emit(await tools.read({})); return 1;",
        ] {
            let r = native_run(code, json!({"content":[block]}), false);
            assert_eq!(r["status"], "completed", "{r}");
            let output: Value = serde_json::from_str(&crate::tool_result(r)).unwrap();
            assert_eq!(output["result"]["content"][0], block);
        }
    }
}

#[test]
fn unsupported_block_metadata_is_rejected_for_return_and_caught_emit() {
    for block in [
        json!({"type":"text","text":"ok","meta":{"label":"public"}}),
        json!({"type":"text","text":"ok","meta":{"runtimeKey":"secret"}}),
        json!({"type":"text","text":"ok","unknown":null}),
        json!({"type":"image","mimeType":"image/png","data":"AAAA","meta":{"label":"public"}}),
        json!({"type":"image","mimeType":"image/png","data":"AAAA","textSignature":"not-supported"}),
    ] {
        for code in [
            "print('before'); return await tools.read({});",
            "print('before'); try {tools.emit(await tools.read({}));} catch(e){} return 42;",
        ] {
            let r = native_run(code, json!({"content":[block]}), false);
            assert_eq!(r["status"], "error", "{r}");
            assert!(
                r["error"]
                    .as_str()
                    .unwrap()
                    .contains("unsupported native content block metadata"),
                "{r}"
            );
            assert_eq!(r["logs"][0]["text"], "before");
        }
    }
}

#[test]
fn caught_emit_serialization_failures_latch_native_fatal_before_later_host_effects() {
    for value in [
        "(()=>{const v=[]; v.push(v); return v;})()",
        "[{type:'text',text:1n}]",
        "({toJSON(){throw Error('toJSON failed');}})",
        "undefined",
        "(()=>{})",
        "Symbol('unserializable')",
        "({toJSON(){return undefined;}})",
    ] {
        let code = format!("print('before'); try {{tools.emit({value});}} catch(e) {{}} try {{await tools.echo({{}});}} catch(e) {{}} return {{usage:{{cost:999}},addedToolNames:['forged']}};");
        let (r, h) = run_fake(&code, "");
        assert_eq!(r["status"], "error", "{value}: {r}");
        assert_eq!(r["error"], "invalid emitted JSON", "{value}: {r}");
        assert_eq!(r["logs"][0]["text"], "before");
        assert_eq!(r["result"], Value::Null);
        cleaned(&h, 0);
    }
}

#[test]
fn selected_image_or_empty_output_then_throw_keeps_blocks_logs_and_visible_error() {
    for selection in [
        json!([]),
        json!([{"type":"image","mimeType":"image/png","data":PNG}]),
    ] {
        let code = format!(
            "print('before'); tools.emit({selection}); throw Error('selected output failed');"
        );
        let r = native_run(&code, image_result(), false);
        assert_eq!(r["status"], "error", "{r}");
        assert_eq!(r["nativeContent"], selection);
        let output: Value = serde_json::from_str(&crate::tool_result(r)).unwrap();
        let blocks = output["result"]["content"].as_array().unwrap();
        let selected = selection.as_array().unwrap();
        assert_eq!(&blocks[..selected.len()], selected);
        assert_eq!(blocks.len(), selected.len() + 1);
        assert!(blocks.last().unwrap()["text"]
            .as_str()
            .unwrap()
            .contains("selected output failed"));
        assert_eq!(output["isError"], true);
        assert_eq!(output["result"]["details"]["logs"][0]["text"], "before");
        assert!(output["result"]["details"]["error"]
            .as_str()
            .unwrap()
            .contains("selected output failed"));
        assert_eq!(output["result"]["terminate"], false);
    }
}

#[test]
fn selected_output_error_diagnostic_is_utf8_bounded_and_keeps_full_selection() {
    let r = native_run("tools.emit(Array.from({length:128},()=>({type:'text',text:'selected'}))); throw Error('界'.repeat(4096));", image_result(), false);
    let encoded = crate::tool_result(r);
    assert!(encoded.len() <= MAX_JSON);
    let output: Value = serde_json::from_str(&encoded).unwrap();
    let blocks = output["result"]["content"].as_array().unwrap();
    assert_eq!(
        blocks.len(),
        129,
        "diagnostic is not a guest-selected block"
    );
    assert!(blocks[..128]
        .iter()
        .all(|block| block["text"] == "selected"));
    assert!(blocks[128]["text"].as_str().unwrap().len() <= "codemode error: ".len() + 512);
}

#[test]
fn emitted_and_returned_native_content_share_count_and_byte_limits() {
    for code in [
        "tools.emit(Array.from({length:129},()=>({type:'text',text:'x'})));",
        "tools.emit(Array.from({length:128},()=>({type:'text',text:'x'}))); return {content:[{type:'text',text:'extra'}]};",
        "try {tools.emit([{type:'text',text:'x'.repeat(1048576)}]);} catch(e) {} return 1;",
        "tools.emit([{type:'text',text:'x'.repeat(600000)}]); tools.emit([{type:'text',text:'x'.repeat(600000)}]);",
    ] {
        let r = native_run(code,image_result(),false);
        assert_eq!(r["status"],"error","{code}: {r}");
    }
    let r = native_run("tools.emit([]); return 42;", image_result(), false);
    assert_eq!(r["nativeContent"], json!([]));
}

#[test]
fn terminal_error_forwards_native_reference_without_revalidating_finalized_content() {
    for content in [image_result()["content"].clone(), json!([{"type":"audio"}])] {
        let r = native_run(
            "await tools.read({}); throw Error('must not resume');",
            json!({"content":content,"terminate":true}),
            true,
        );
        assert_eq!(r["terminate"], true, "{r}");
        let output: Value = serde_json::from_str(&crate::tool_result(r)).unwrap();
        assert_eq!(output["isError"], true);
        assert_eq!(
            output["resultReference"],
            json!({"version":1,"id":"receipt-1"})
        );
        assert!(output.get("result").is_none());
        // Finalized native content is now resolved by the owning host, not
        // reserialized or decorated with synthetic diagnostic blocks here.
    }
}

#[test]
fn finalized_errors_keep_bounded_useful_text_without_serializing_images_in_error_message() {
    let r = native_run("try {await tools.read({});} catch(e) {return {message:e.message,data:e.result.content[1].data};}",image_result(),true);
    assert_eq!(r["result"]["message"], "tool returned isError: tiny");
    assert_eq!(r["result"]["data"], PNG);
    assert_eq!(r.to_string().matches(PNG).count(), 1);
}

#[test]
fn native_content_supports_each_documented_mime_and_exact_count_boundary() {
    for mime in ["image/png", "image/jpeg", "image/gif", "image/webp"] {
        let selected =
            content::blocks(json!([{"type":"image","mimeType":mime,"data":"AAA="}])).unwrap();
        assert_eq!(selected[0]["mimeType"], mime);
    }
    for data in ["AA==", "AAA=", "AAAA"] {
        assert!(
            content::blocks(json!([{"type":"image","mimeType":"image/png","data":data}])).is_ok()
        );
    }
    let blocks = vec![json!({"type":"text","text":"ok"}); 128];
    assert_eq!(content::blocks(json!(blocks)).unwrap().len(), 128);
}

#[test]
fn business_property_names_survive_schemas_arguments_results_and_guest_returns() {
    let business = json!({"runtimeKey":"business","runtimeHandle":{"__rpiRuntimeKey":[{"__rpi":{"runtimeKey":"nested"}}]},"__rpi":true,"__rpiRuntimeKey":null});
    let schema = json!({"type":"object","properties":business,"nested":[business]});
    let expected_schema = schema.clone();
    let expected_business = business.clone();
    let action: Arc<Action> = Arc::new(move |v| {
        Ok(match v["op"].as_str().unwrap() {
            "list" => {
                json!({"protocolVersion":1,"capabilities":{"resultEnvelope":1,"hostAccounting":1,"parentProgress":1,"losslessValues":1,"resultReferences":1,"contentAttachments":1},"tools":[{"name":"echo","description":"fixture","parameters":schema}],"nextOffset":null})
            }
            "start" => {
                assert_eq!(v["arguments"], expected_business);
                json!({"requestId":"private","status":"running"})
            }
            "poll" => {
                json!({"requestId":"private","status":"done","result":{"content":[],"details":expected_business,"runtimeKey":"business","__rpi":{"runtimeHandle":3}}})
            }
            "dispose" | "cancel" => json!({}),
            _ => panic!("unexpected action"),
        })
    });
    let code = format!("const a={business}; const r=await tools.echo(a); return {{schema:tools.list()[0].parameters,args:a,raw:r,nested:[{{runtimeKey:'return',runtimeHandle:9,__rpi:{{__rpiRuntimeKey:'return'}}}}]}};");
    let r = execute(&code, 2000, action, Arc::new(AtomicBool::new(false)));
    assert_eq!(r["status"], "completed", "{r}");
    assert_eq!(r["result"]["schema"], expected_schema);
    assert_eq!(r["result"]["args"], business);
    assert_eq!(r["result"]["raw"]["details"], business);
    assert_eq!(r["result"]["raw"]["__rpi"]["runtimeHandle"], 3);
    assert_eq!(
        r["result"]["nested"][0]["__rpi"]["__rpiRuntimeKey"],
        "return"
    );
}

#[test]
fn explicit_native_result_selects_exact_content_details_and_trusted_error_without_accounting() {
    for is_error in [false, true] {
        for content in [
            json!([]),
            json!([{"type":"text","text":"","textSignature":null}]),
            json!([{"type":"text","text":"exact","textSignature":"signature"},{"type":"image","mimeType":"image/png","data":PNG}]),
        ] {
            let details = json!({"runtimeKey":"business","runtimeHandle":42,"__rpi":{"__rpiRuntimeKey":"nested"},"usage":{"cost":7},"addedToolNames":["business"],"status":"business-status"});
            let raw = json!({"content":content,"details":details,"usage":{"cost":1},"addedToolNames":["new"],"terminate":false});
            let code = if is_error {
                "let r; try {await tools.read({});} catch(e) {r=e.result;} r.isError=false; r.usage={cost:999}; r.addedToolNames=['forged']; tools.result(r); return {diagnostic:'kept internally'};"
            } else {
                "const r=await tools.read({}); r.isError=true; r.usage={cost:999}; r.addedToolNames=['forged']; tools.result(r); return {diagnostic:'kept internally'};"
            };
            let r = native_run(code, raw, is_error);
            assert_eq!(r["status"], "completed", "{r}");
            assert_eq!(r["calls"][0]["isError"], is_error);
            assert_eq!(r["result"]["diagnostic"], "kept internally");
            let encoded = crate::tool_result(r);
            let output: Value = serde_json::from_str(&encoded).unwrap();
            assert_eq!(output["isError"], is_error);
            assert_eq!(
                output["result"]["content"], content,
                "no synthetic error block"
            );
            assert_eq!(
                output["result"]["details"], details,
                "no CodeMode summary in exact details"
            );
            assert!(output["result"].get("usage").is_none());
            assert!(output["result"].get("addedToolNames").is_none());
            assert!(!encoded.contains("forged"));
            assert!(!encoded.contains("diagnostic"));
            assert!(encoded.matches(PNG).count() <= 1);
        }
    }
    let output: Value = serde_json::from_str(&crate::tool_result(native_run(
        "return tools.result(await tools.read({}));",
        json!({"content":[]}),
        false,
    )))
    .unwrap();
    assert_eq!(output["result"]["content"], json!([]));
    assert_eq!(output["result"].get("details"), Some(&Value::Null));
    for mutation in ["delete r.details", "r.details=undefined"] {
        for is_error in [false, true] {
            let acquire = if is_error {
                "let r; try {await tools.read({});} catch(e) {r=e.result;}"
            } else {
                "const r=await tools.read({});"
            };
            let output: Value = serde_json::from_str(&crate::tool_result(native_run(
                &format!("{acquire} {mutation}; tools.result(r);"),
                json!({"content":[],"details":{"original":true}}),
                is_error,
            )))
            .unwrap();
            assert_eq!(output["isError"], is_error, "{mutation}: {output}");
            assert_eq!(output["result"]["content"], json!([]));
            assert_eq!(output["result"].get("details"), Some(&Value::Null));
        }
    }
}

#[test]
fn invalid_conflicting_and_multiple_native_result_selections_are_sticky() {
    for code in [
        "try {tools.result({content:[],details:{}});} catch(e) {} return 42;",
        "try {tools.result(null);} catch(e) {} return 42;",
        "const r=await tools.read({}); try {tools.result({...r});} catch(e) {} return 42;",
        "const r=await tools.read({}); tools.result(r); try {tools.result(r);} catch(e) {} return 42;",
        "const [r,s]=await Promise.all([tools.read({}),tools.read({})]); tools.result(r); try {tools.result(s);} catch(e) {} return 42;",
        "const r=await tools.read({}); tools.emit([]); try {tools.result(r);} catch(e) {} return 42;",
        "const r=await tools.read({}); tools.result(r); try {tools.emit([]);} catch(e) {} return 42;",
        "const r=await tools.read({}); r.details={toJSON(){throw Error('bad serialization');}}; try {tools.result(r);} catch(e) {} return 42;",
        "const r=await tools.read({}); r.content=[{type:'audio'}]; try {tools.result(r);} catch(e) {} return 42;",
        "const r=await tools.read({}); r.details='x'.repeat(1048576); try {tools.result(r);} catch(e) {} return 42;",
        "const r=await tools.read({}); tools.result(r); return r;",
    ] {
        let r = native_run(code, image_result(), false);
        assert_eq!(r["status"], "error", "{code}: {r}");
        assert!(r.get("nativeResult").is_none(), "{r}");
        let output: Value = serde_json::from_str(&crate::tool_result(r)).unwrap();
        assert_eq!(output["isError"], true);
        assert_eq!(output["result"]["details"]["status"], "error");
    }
}

#[test]
fn result_association_and_error_flag_survive_guest_intrinsic_mutation() {
    let r = native_run("WeakMap.prototype.get=()=>true; WeakMap.prototype.set=()=>{}; const r=await tools.read({}); r.isError=true; tools.result(r);", image_result(), false);
    assert_eq!(r["status"], "completed", "{r}");
    let output: Value = serde_json::from_str(&crate::tool_result(r)).unwrap();
    assert_eq!(output["isError"], false);
    assert_eq!(output["result"]["details"], json!({"ok":true}));
}

#[test]
fn runtime_failure_timeout_and_oom_after_exact_selection_do_not_become_selected_success() {
    for tail in [
        "throw Error('after selection');",
        "for(;;){}",
        "try {const a=[]; for(;;) a.push(new Array(10000).fill('x'));} catch(e) {} return 42;",
    ] {
        let timeout = if tail == "for(;;){}" { 50 } else { 2000 };
        let r = execute(
            &format!("tools.result(await tools.read({{}})); {tail}"),
            timeout,
            native_host(image_result(), false),
            Arc::new(AtomicBool::new(false)),
        );
        assert!(
            matches!(r["status"].as_str(), Some("error" | "timed_out")),
            "{r}"
        );
        assert!(r.get("nativeResult").is_none());
        let output: Value = serde_json::from_str(&crate::tool_result(r)).unwrap();
        assert_eq!(output["isError"], true);
        assert!(!output.to_string().contains(PNG));
    }
}

#[test]
fn cancellation_after_exact_selection_retains_cancelled_failure() {
    let cancelled = Arc::new(AtomicBool::new(false));
    let token = cancelled.clone();
    let base = native_host(image_result(), false);
    let action: Arc<Action> = Arc::new(move |v| {
        if v["op"] == "start" && v["arguments"]["cancel"] == true {
            token.store(true, Ordering::Release);
        }
        base(v)
    });
    let r = execute(
        "tools.result(await tools.read({})); await tools.read({cancel:true});",
        2000,
        action,
        cancelled,
    );
    assert_eq!(r["status"], "cancelled", "{r}");
    assert!(r.get("nativeResult").is_none());
    let output: Value = serde_json::from_str(&crate::tool_result(r)).unwrap();
    assert_eq!(output["isError"], true);
    assert_eq!(output["result"]["details"]["status"], "cancelled");
}

#[test]
fn later_terminal_child_overrides_exact_selection_without_losing_content_or_flags() {
    for is_error in [false, true] {
        let base = native_host(image_result(), false);
        let polls = std::sync::atomic::AtomicUsize::new(0);
        let action: Arc<Action> = Arc::new(move |v| {
            if v["op"] == "poll" && polls.fetch_add(1, Ordering::AcqRel) != 0 {
                return Ok(
                    json!({"requestId":"private","status":"done","isError":is_error,
                    "result":{"content":[{"type":"text","text":"terminal"}],"details":{"terminal":true},"terminate":true}}),
                );
            }
            base(v)
        });
        let r = execute("tools.result(await tools.read({})); await tools.read({}); throw Error('must not resume');", 2000, action, Arc::new(AtomicBool::new(false)));
        assert_eq!(r["status"], "terminated", "{r}");
        assert!(r.get("nativeResult").is_none());
        let output: Value = serde_json::from_str(&crate::tool_result(r)).unwrap();
        assert_eq!(output["isError"], is_error);
        assert_eq!(
            output["resultReference"],
            json!({"version":1,"id":"receipt-2"})
        );
        assert!(output.get("result").is_none());
        assert!(!output.to_string().contains(PNG));
    }
}

// A reference-protocol peer around the existing tool-outcome fixtures. Fixtures
// describe finalized native results, not a production inline compatibility path.
// Storage survives dispose and is removed only by release or simulated finish.
fn reference_peer(tool: Arc<Action>) -> Arc<Action> {
    #[derive(Default)]
    struct Peer {
        next: usize,
        requests: HashMap<String, String>,
        results: HashMap<String, String>,
    }
    let state = Mutex::new(Peer::default());
    Arc::new(move |mut request| {
        let mut state = state.lock().unwrap();
        let op = request["op"].as_str().unwrap().to_owned();
        if op == "measure_result_content" {
            let id = request["resultId"].as_str().unwrap();
            let result: Value =
                serde_json::from_str(state.results.get(id).ok_or("unknown reference")?).unwrap();
            let blocks = result["content"].as_array().unwrap();
            let selected = match &request["indices"] {
                Value::Null => blocks.iter().collect::<Vec<_>>(),
                Value::Array(indices) => indices
                    .iter()
                    .map(|i| &blocks[i.as_u64().unwrap() as usize])
                    .collect(),
                _ => panic!("invalid measurement indices"),
            };
            let bytes = selected.iter().map(|b| b.to_string().len()).sum::<usize>();
            if bytes > MEMORY {
                return Err("native selected content exceeds 32 MiB".into());
            }
            return Ok(json!({"resultId":id,"contentBlocks":selected.len(),
                "encodedBytes":bytes}));
        }
        if op == "read_result" {
            let id = request["resultId"].as_str().unwrap();
            let text = state.results.get(id).ok_or("unknown reference")?;
            let offset = request["offset"].as_u64().unwrap() as usize;
            let mut end = (offset + request["limit"].as_u64().unwrap() as usize).min(text.len());
            while !text.is_char_boundary(end) {
                end -= 1;
            }
            return Ok(
                json!({"resultId":id,"offset":offset,"data":&text[offset..end],
                "byteLength":text.len(),"nextOffset":if end < text.len() {Some(end)} else {None}}),
            );
        }
        if op == "release_result" {
            let id = request["resultId"].as_str().unwrap();
            state.results.remove(id).ok_or("unknown reference")?;
            return Ok(json!({"resultId":id,"status":"released"}));
        }
        if op == "start" {
            assert_eq!(request["resultMode"], "reference");
        }
        let public_id = request["requestId"].as_str().map(str::to_owned);
        if let Some(id) = &public_id {
            request["requestId"] = json!(state.requests.get(id).ok_or("unknown request")?);
        }
        let mut reply = tool(request)?;
        if op == "start" {
            state.next += 1;
            let id = format!("receipt-{}", state.next);
            state
                .requests
                .insert(id.clone(), reply["requestId"].as_str().unwrap().into());
            reply["requestId"] = json!(id);
        } else if let Some(id) = public_id {
            if reply.get("requestId").is_some() {
                reply["requestId"] = json!(id);
            }
            // Keep the deliberately oversized host-frame fixture malformed.
            if op == "poll"
                && reply["status"] == "done"
                && (reply["result"]["content"].is_array() || reply.to_string().len() <= MAX_JSON)
            {
                let result = reply.as_object_mut().unwrap().remove("result").unwrap();
                let text = result.to_string();
                reply["isError"] = json!(reply["isError"] == true);
                reply["resultReference"] = json!({"version":1,"id":id,"byteLength":text.len(),
                    "contentBlocks":result["content"].as_array().map_or(0, Vec::len),
                    "isError":reply["isError"],"terminate":result["terminate"] == true});
                state.results.insert(id, text);
            }
        }
        Ok(reply)
    })
}
fn execute(code: &str, timeout_ms: u64, action: Arc<Action>, cancelled: Arc<AtomicBool>) -> Value {
    super::execute(code, timeout_ms, reference_peer(action), cancelled)
}

fn large_result() -> Value {
    json!({"content":[{"type":"text","text":"界".repeat(400_000)},
        {"type":"image","mimeType":"image/png","data":PNG}],
        "details":{"nested":[{"value":7}],"text":"details"},
        "usage":{"cost":1},"addedToolNames":["new"],"terminate":false})
}
fn recorded_reference(result: Value, error: bool) -> (Arc<Action>, Arc<Mutex<Vec<Value>>>) {
    let base = reference_peer(native_host(result, error));
    let events = Arc::new(Mutex::new(Vec::new()));
    let shared = events.clone();
    (
        Arc::new(move |v| {
            shared.lock().unwrap().push(v.clone());
            base(v)
        }),
        events,
    )
}
fn direct(code: &str, action: Arc<Action>) -> Value {
    super::execute(code, 5000, action, Arc::new(AtomicBool::new(false)))
}
fn count(events: &Arc<Mutex<Vec<Value>>>, op: &str) -> usize {
    events
        .lock()
        .unwrap()
        .iter()
        .filter(|v| v["op"] == op)
        .count()
}
fn resolve_fixture(action: &Arc<Action>, reference: &Value) -> Value {
    let mut text = String::new();
    loop {
        let r = action(json!({"op":"read_result","resultId":reference["id"],"offset":text.len(),"limit":65536})).unwrap();
        text.push_str(r["data"].as_str().unwrap());
        if r["nextOffset"].is_null() {
            break;
        }
    }
    serde_json::from_str(&text).unwrap()
}
#[test]
fn large_receipts_forward_without_materialization_and_survive_bridge_drop() {
    for error in [false, true] {
        let original = large_result();
        let (action, events) = recorded_reference(original.clone(), error);
        let code = if error {
            "try {await tools.read({});} catch(e) {tools.result(e.result);}"
        } else {
            "tools.result(await tools.read({})); return {diagnostic:true};"
        };
        let r = direct(code, action.clone());
        assert_eq!(r["status"], "completed", "{r}");
        assert_eq!(r["isError"], error);
        assert_eq!(count(&events, "read_result"), 0);
        assert_eq!(count(&events, "release_result"), 0);
        assert_eq!(count(&events, "start"), 1);
        let output: Value = serde_json::from_str(&crate::tool_result(r)).unwrap();
        assert_eq!(
            output,
            json!({"$rpiToolResult":1,"isError":error,
            "resultReference":{"version":1,"id":"receipt-1"}})
        );
        assert_eq!(
            resolve_fixture(&action, &output["resultReference"]),
            original
        );
    }
}
#[test]
fn lazy_receipts_are_enumerable_deeply_frozen_and_forward_original_after_inspection() {
    let (action, events) = recorded_reference(large_result(), false);
    let r = direct("const r=await tools.read({}); const names=Object.keys(r); const copy={...r}; let n=0; for(const f of [()=>{r.content=[]},()=>{r.details.nested[0].value=9},()=>{r.content.push({})},()=>{Object.setPrototypeOf(r,{})}]) {try{f()}catch(e){n++}} tools.result(r); return {n,names,len:copy.content[0].text.length,json:JSON.stringify(r).length};", action.clone());
    assert_eq!(r["status"], "completed", "{r}");
    assert_eq!(r["result"]["n"], 4);
    assert_eq!(r["result"]["len"], 400_000);
    assert_eq!(
        r["result"]["names"],
        json!(["content", "details", "usage", "addedToolNames", "terminate"])
    );
    let reads = count(&events, "read_result");
    assert!(reads > 16);
    assert_eq!(
        resolve_fixture(&action, &r["nativeResultReference"]),
        large_result()
    );
    assert_eq!(count(&events, "release_result"), 0);
}
#[test]
fn lazy_freeze_uses_captured_intrinsics_not_mutable_guest_helpers() {
    let (action, _) = recorded_reference(large_result(), false);
    let r = direct("const frozen=Object.isFrozen; Object.freeze=Object.keys=Object.defineProperty=()=>{throw Error('poison')}; Array.prototype.push=Array.prototype.pop=Array.prototype[Symbol.iterator]=()=>{throw Error('poison')}; WeakMap.prototype.get=()=>({token:1,isError:true}); JSON.parse=()=>{throw Error('poison')}; const r=await tools.read({}); const d=r.details; return {frozen:frozen(d)&&frozen(d.nested)&&frozen(d.nested[0]),value:d.nested[0].value};",action);
    assert_eq!(r["status"], "completed", "{r}");
    assert_eq!(r["result"], json!({"frozen":true,"value":7}));
}
#[test]
fn releasing_large_receipts_supports_bounded_loops_and_small_release_is_noop() {
    for result in [large_result(), image_result()] {
        let large = result.to_string().len() > MAX_JSON;
        let (action, events) = recorded_reference(result, false);
        let r = direct(
            "for(let i=0;i<24;i++){const r=await tools.read({}); tools.release(r);} return 24;",
            action,
        );
        assert_eq!(r["status"], "completed", "{r}");
        assert_eq!(r["result"], 24);
        assert_eq!(count(&events, "start"), 24);
        assert_eq!(count(&events, "release_result"), 24);
        assert_eq!(count(&events, "read_result") > 0, !large);
        assert!(r.get("usage").is_none());
    }
}
#[test]
fn release_invalidates_cached_reads_and_selection_and_identity_cannot_be_forged() {
    for tail in [
        "tools.release(r); try {r.details} catch(e){} return 1;",
        "void r.details; tools.release(r); try {r.details} catch(e){} return 1;",
        "tools.release(r); try {tools.result(r)} catch(e){} return 1;",
        "tools.result(r); try {tools.release(r)} catch(e){} return 1;",
        "try {tools.result(Object.create(r))} catch(e){} return 1;",
        "tools.result(r); try {tools.emit([])} catch(e){} return 1;",
        "tools.emit([]); try {tools.result(r)} catch(e){} return 1;",
        "tools.result(r); try {tools.result(r)} catch(e){} return 1;",
    ] {
        let (action, events) = recorded_reference(large_result(), false);
        let r = direct(&format!("const r=await tools.read({{}}); {tail}"), action);
        assert_eq!(r["status"], "error", "{tail}: {r}");
        assert!(r.get("nativeResultReference").is_none());
        assert_eq!(count(&events, "release_result"), 1);
    }
    let (action, events) = recorded_reference(large_result(), false);
    let r=direct("for(const r of [{content:[]},Object.create(null),null,7]) {try {tools.release(r); throw Error('accepted');} catch(e) {if(e.message==='accepted') throw e;}} return 4;",action);
    assert_eq!(r["result"], 4);
    assert_eq!(count(&events, "start"), 0);
}
#[test]
fn large_selected_receipts_are_discarded_on_throw_timeout_oom_and_cancellation() {
    for tail in [
        "throw Error('later');",
        "for(;;){}",
        "try{const a=[];for(;;)a.push(new Array(10000).fill('x'));}catch(e){}",
    ] {
        let (action, events) = recorded_reference(large_result(), false);
        let r = super::execute(
            &format!("tools.result(await tools.read({{}})); {tail}"),
            if tail == "for(;;){}" { 50 } else { 5000 },
            action,
            Arc::new(AtomicBool::new(false)),
        );
        assert!(
            matches!(r["status"].as_str(), Some("error" | "timed_out")),
            "{r}"
        );
        assert!(r.get("nativeResultReference").is_none());
        assert_eq!(count(&events, "release_result"), 1);
        assert_eq!(count(&events, "read_result"), 0);
    }
    let (base, events) = recorded_reference(large_result(), false);
    let cancel = Arc::new(AtomicBool::new(false));
    let token = cancel.clone();
    let action: Arc<Action> = Arc::new(move |v| {
        if v["op"] == "start" && v["arguments"]["cancel"] == true {
            token.store(true, Ordering::Release);
        }
        base(v)
    });
    let r = super::execute(
        "tools.result(await tools.read({})); await tools.read({cancel:true});",
        5000,
        action,
        cancel,
    );
    assert_eq!(r["status"], "cancelled", "{r}");
    assert!(r.get("nativeResultReference").is_none());
    assert!(count(&events, "release_result") >= 1);
}
#[test]
fn large_terminal_child_never_resumes_and_keeps_native_flags_and_payload() {
    for error in [false, true] {
        let mut original = large_result();
        original["terminate"] = json!(true);
        let (action, events) = recorded_reference(original.clone(), error);
        let r = direct(
            "try{await tools.read({});}catch(e){} throw Error('resumed');",
            action.clone(),
        );
        assert_eq!(r["status"], "terminated", "{r}");
        assert_eq!(r["isError"], error);
        assert_eq!(r["terminate"], true);
        assert_eq!(count(&events, "read_result"), 0);
        assert_eq!(count(&events, "release_result"), 0);
        assert_eq!(
            resolve_fixture(&action, &r["nativeResultReference"]),
            original
        );
    }
}

#[test]
fn manifests_reject_missing_wrong_and_surplus_fields_without_guest_recovery() {
    for (field, bad) in [
        ("version", json!(2)),
        ("version", json!(1.0)),
        ("id", json!("other")),
        ("byteLength", json!(0)),
        ("byteLength", json!(MEMORY + 1)),
        ("byteLength", json!(-1)),
        ("byteLength", json!(1.5)),
        ("contentBlocks", json!(u64::MAX)),
        ("isError", json!(true)),
        ("terminate", Value::Null),
    ] {
        let (base, events) = recorded_reference(image_result(), false);
        let field = field.to_owned();
        let action: Arc<Action> = Arc::new(move |v| {
            let poll = v["op"] == "poll";
            let mut r = base(v)?;
            if poll {
                r["resultReference"][&field] = bad.clone();
            }
            Ok(r)
        });
        let r = direct("try{await tools.read({});}catch(e){} return 42;", action);
        assert_eq!(r["status"], "error", "{r}");
        assert_eq!(count(&events, "read_result"), 0);
        assert_eq!(count(&events, "release_result"), 1);
    }
    for mode in [
        "inline",
        "both",
        "missing",
        "outer_error",
        "running_inline",
        "poll_id",
        "start_inline",
    ] {
        let (base, _) = recorded_reference(image_result(), false);
        let action: Arc<Action> = Arc::new(move |v| {
            let op = v["op"].clone();
            let mut r = base(v)?;
            if op == "poll" {
                match mode {
                    "inline" => {
                        r.as_object_mut().unwrap().remove("resultReference");
                        r["result"] = image_result();
                    }
                    "both" => r["result"] = image_result(),
                    "missing" => {
                        r.as_object_mut().unwrap().remove("resultReference");
                    }
                    "outer_error" => r["isError"] = Value::Null,
                    "running_inline" => {
                        r["status"] = json!("running");
                        r["result"] = image_result();
                    }
                    "poll_id" => r["requestId"] = json!("other"),
                    _ => {}
                }
            }
            if op == "start" && mode == "start_inline" {
                r["result"] = image_result();
            }
            Ok(r)
        });
        let r = direct("try{await tools.read({});}catch(e){} return 42;", action);
        assert_eq!(r["status"], "error", "{mode}: {r}");
    }
}
#[test]
fn chunk_validation_rejects_bad_identity_lengths_progress_eof_and_json() {
    for mode in [
        "id",
        "offset",
        "length",
        "empty",
        "oversize",
        "cursor",
        "early_eof",
        "late_eof",
        "surplus",
        "invalid_json",
        "content_count",
        "flag",
    ] {
        let (base, events) = recorded_reference(image_result(), false);
        let action: Arc<Action> = Arc::new(move |v| {
            let read = v["op"] == "read_result";
            let mut r = base(v)?;
            if read {
                match mode {
                    "id" => r["resultId"] = json!("other"),
                    "offset" => r["offset"] = json!(1),
                    "length" => r["byteLength"] = json!(1),
                    "empty" => r["data"] = json!(""),
                    "oversize" => r["data"] = json!("x".repeat(MAX_JSON + 1)),
                    "cursor" => r["nextOffset"] = json!(0),
                    "early_eof" => {
                        r["data"] = json!("{");
                        r["nextOffset"] = Value::Null;
                    }
                    "late_eof" => r["nextOffset"] = r["byteLength"].clone(),
                    "surplus" => r["result"] = json!({}),
                    "invalid_json" => {
                        let n = r["data"].as_str().unwrap().len();
                        r["data"] = json!("x".repeat(n));
                    }
                    "content_count" => {
                        let text = r["data"]
                            .as_str()
                            .unwrap()
                            .replace("\"type\":\"text\"", "\"type\":\"xxxx\"");
                        r["data"] = json!(text);
                        r["data"] = json!(r["data"]
                            .as_str()
                            .unwrap()
                            .replace("\"content\"", "\"xxxxxxx\""));
                    }
                    "flag" => {
                        r["data"] = json!(r["data"]
                            .as_str()
                            .unwrap()
                            .replace("\"terminate\":false", "\"terminate\":null "));
                    }
                    _ => unreachable!(),
                }
            }
            Ok(r)
        });
        let r = direct("try{await tools.read({});}catch(e){} return 42;", action);
        assert_eq!(r["status"], "error", "{mode}: {r}");
        assert_eq!(count(&events, "release_result"), 1);
    }
}
#[test]
fn chunk_checks_cancel_and_deadline_between_reads_and_stable_total_length() {
    for mode in ["cancel", "deadline", "changed_length"] {
        let (base, events) = recorded_reference(large_result(), false);
        let cancel = Arc::new(AtomicBool::new(false));
        let token = cancel.clone();
        let reads = std::sync::atomic::AtomicUsize::new(0);
        let action: Arc<Action> = Arc::new(move |v| {
            let read = v["op"] == "read_result";
            let mut r = base(v)?;
            if read {
                let n = reads.fetch_add(1, Ordering::AcqRel);
                match mode {
                    "cancel" => token.store(true, Ordering::Release),
                    "deadline" => std::thread::sleep(Duration::from_millis(1100)),
                    "changed_length" if n == 1 => r["byteLength"] = json!(MEMORY),
                    _ => {}
                }
            }
            Ok(r)
        });
        let r = super::execute(
            "const r=await tools.read({}); try{void r.details;}catch(e){} return 42;",
            if mode == "deadline" { 1000 } else { 5000 },
            action,
            cancel,
        );
        assert_ne!(r["status"], "completed", "{mode}: {r}");
        assert_eq!(
            count(&events, "read_result"),
            if mode == "changed_length" { 2 } else { 1 }
        );
        assert_eq!(count(&events, "release_result"), 1);
    }
}

#[test]
fn lazy_freeze_does_not_invoke_poisoned_descriptor_prototypes() {
    let (action, _) = recorded_reference(large_result(), false);
    let r=direct("Object.defineProperty(Object.prototype,'get',{get(){throw Error('descriptor trap')},configurable:true}); Object.defineProperty(Object.prototype,'value',{get(){throw Error('descriptor trap')},configurable:true}); const r=await tools.read({}); return r.details.nested[0].value;",action);
    assert_eq!(r["status"], "completed", "{r}");
    assert_eq!(r["result"], 7);
}
#[test]
fn release_helper_does_not_shadow_explicit_native_release_call() {
    let base = reference_peer(native_host(image_result(), false));
    let names = Arc::new(Mutex::new(Vec::new()));
    let shared = names.clone();
    let action: Arc<Action> = Arc::new(move |v| {
        if v["op"] == "start" {
            shared.lock().unwrap().push(v["name"].clone());
        }
        let list = v["op"] == "list";
        let mut r = base(v)?;
        if list {
            r["tools"]
                .as_array_mut()
                .unwrap()
                .push(json!({"name":"release","description":"native alias","parameters":{}}));
        }
        Ok(r)
    });
    let r = direct(
        "const r=await tools.call('release',{}); tools.release(r); return r.details;",
        action,
    );
    assert_eq!(r["status"], "completed", "{r}");
    assert_eq!(*names.lock().unwrap(), vec![json!("release")]);
}
#[test]
fn cleanup_releases_unselected_large_references_and_preserves_only_one_handoff() {
    let (action, events) = recorded_reference(large_result(), false);
    let r=direct("const a=await tools.read({}); const b=await tools.read({}); const c=await tools.read({}); tools.result(b);",action.clone());
    assert_eq!(r["status"], "completed", "{r}");
    assert_eq!(r["nativeResultReference"]["id"], "receipt-2");
    assert_eq!(count(&events, "read_result"), 0);
    assert_eq!(count(&events, "release_result"), 2);
    for id in ["receipt-1", "receipt-3"] {
        assert!(action(json!({"op":"read_result","resultId":id,"offset":0,"limit":4})).is_err());
    }
    assert_eq!(
        resolve_fixture(&action, &r["nativeResultReference"]),
        large_result()
    );
}
#[test]
fn large_details_and_png_payloads_forward_without_loading_or_duplicate_accounting() {
    for result in [
        json!({"content":[],"details":{"data":"x".repeat(1_200_000)},"terminate":false}),
        json!({"content":[{"type":"image","mimeType":"image/png","data":"AAAA".repeat(300_000)}],"details":null,"terminate":false}),
    ] {
        let (action, events) = recorded_reference(result.clone(), false);
        let r = direct("tools.result(await tools.read({}));", action.clone());
        assert_eq!(r["status"], "completed", "{r}");
        assert_eq!(count(&events, "read_result"), 0);
        assert_eq!(
            resolve_fixture(&action, &r["nativeResultReference"]),
            result
        );
        let encoded = crate::tool_result(r);
        assert!(encoded.len() < 256);
        assert!(!encoded.contains("usage"));
        assert!(!encoded.contains("addedToolNames"));
    }
}

#[test]
fn retained_count_limit_and_host_quota_fail_closed_after_selection() {
    let (action, events) = recorded_reference(large_result(), false);
    let r = direct(
        "try{for(let i=0;i<17;i++)await tools.read({});}catch(e){} return 42;",
        action,
    );
    assert_eq!(r["status"], "error", "{r}");
    assert!(r["error"].as_str().unwrap().contains("reference limit"));
    assert_eq!(count(&events, "release_result"), 17);
    let (base, events) = recorded_reference(large_result(), false);
    let polls = std::sync::atomic::AtomicUsize::new(0);
    let action: Arc<Action> = Arc::new(move |v| {
        if v["op"] == "poll" && polls.fetch_add(1, Ordering::AcqRel) > 0 {
            return Ok(
                json!({"requestId":v["requestId"],"status":"failed","error":"invocation-tree result reference byte quota exceeded"}),
            );
        }
        base(v)
    });
    let r = direct(
        "tools.result(await tools.read({})); try{await tools.read({});}catch(e){} return 42;",
        action,
    );
    assert_eq!(r["status"], "error", "{r}");
    assert!(r.get("nativeResultReference").is_none());
    assert!(r["error"].as_str().unwrap().contains("quota"));
    assert_eq!(count(&events, "release_result"), 1);
}
#[test]
fn lazy_inspection_is_not_unlimited_guest_memory_and_caught_oom_releases_native_storage() {
    let original = json!({"content":[{"type":"text","text":"x".repeat(20*1024*1024)}],"details":null,"terminate":false});
    let (action, events) = recorded_reference(original, false);
    let r = direct(
        "const r=await tools.read({}); try{void r.content;}catch(e){} return 42;",
        action,
    );
    assert_eq!(r["status"], "error", "{r}");
    assert_eq!(r["error"], "codemode memory budget exceeded");
    assert_eq!(count(&events, "release_result"), 1);
    assert!(r.get("nativeResultReference").is_none());
}
#[test]
fn forged_or_reused_request_identity_cannot_select_a_stale_receipt() {
    let (base, events) = recorded_reference(large_result(), false);
    let action: Arc<Action> = Arc::new(move |v| {
        let start = v["op"] == "start";
        let mut r = base(v)?;
        if start {
            r["requestId"] = json!("receipt-1");
        }
        Ok(r)
    });
    let r = direct(
        "tools.result(await tools.read({})); try{await tools.read({});}catch(e){} return 42;",
        action,
    );
    assert_eq!(r["status"], "error", "{r}");
    assert!(r["error"].as_str().unwrap().contains("reused request"));
    assert!(r.get("nativeResultReference").is_none());
    assert_eq!(count(&events, "release_result"), 1);
}

#[test]
fn later_terminal_large_receipt_overrides_a_prior_large_selection() {
    for error in [false, true] {
        let mut terminal = large_result();
        terminal["terminate"] = json!(true);
        terminal["details"] = json!({"terminal":true});
        let expected = terminal.clone();
        let base = native_host(large_result(), false);
        let polls = std::sync::atomic::AtomicUsize::new(0);
        let tool: Arc<Action> = Arc::new(move |v| {
            if v["op"] == "poll" && polls.fetch_add(1, Ordering::AcqRel) > 0 {
                return Ok(
                    json!({"requestId":"private","status":"done","isError":error,"result":terminal}),
                );
            }
            base(v)
        });
        let peer = reference_peer(tool);
        let events = Arc::new(Mutex::new(Vec::new()));
        let shared = events.clone();
        let action: Arc<Action> = Arc::new(move |v| {
            shared.lock().unwrap().push(v.clone());
            peer(v)
        });
        let r = direct(
            "tools.result(await tools.read({})); await tools.read({}); throw Error('resumed');",
            action.clone(),
        );
        assert_eq!(r["status"], "terminated", "{r}");
        assert_eq!(r["isError"], error);
        assert_eq!(r["nativeResultReference"]["id"], "receipt-2");
        assert_eq!(count(&events, "release_result"), 1);
        assert_eq!(count(&events, "read_result"), 0);
        assert_eq!(
            resolve_fixture(&action, &r["nativeResultReference"]),
            expected
        );
    }
}
#[test]
fn lifecycle_read_release_and_dispose_errors_are_not_replayed_or_caught_as_success() {
    for op in ["read_result", "release_result", "dispose"] {
        let (base, events) = recorded_reference(large_result(), false);
        let failures = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let shared = failures.clone();
        let action: Arc<Action> = Arc::new(move |v| {
            if v["op"] == op {
                shared.fetch_add(1, Ordering::AcqRel);
                return Err("uncertain lifecycle failure".into());
            }
            base(v)
        });
        let r=direct("try {const r=await tools.read({}); if(true)void r.details; tools.release(r);}catch(e){} return 42;",action);
        assert_eq!(r["status"], "error", "{op}: {r}");
        assert!(r.get("nativeResultReference").is_none());
        assert_eq!(failures.load(Ordering::Acquire), 1, "must not retry {op}");
        assert_eq!(count(&events, "start"), 1);
    }
}
#[test]
fn large_receipts_do_not_widen_arbitrary_return_limits() {
    for tail in [
        "return r;",
        "return r.details;",
        "return 'x'.repeat(1048577);",
    ] {
        let mut original = large_result();
        original["details"] = json!("x".repeat(1_200_000));
        let (action, events) = recorded_reference(original, false);
        let r = direct(&format!("const r=await tools.read({{}}); {tail}"), action);
        assert_eq!(r["status"], "error", "{tail}: {r}");
        assert!(r.get("nativeResultReference").is_none());
        assert_eq!(count(&events, "release_result"), 1);
    }
}

fn resolved_parts(action: &Arc<Action>, parts: &Value) -> Vec<Value> {
    let mut output = Vec::new();
    for part in parts.as_array().unwrap() {
        if let Some(blocks) = part["content"].as_array() {
            output.extend(blocks.iter().cloned());
        } else {
            let result = resolve_fixture(action, &part["reference"]);
            let blocks = result["content"].as_array().unwrap();
            match &part["indices"] {
                Value::Null => output.extend(blocks.iter().cloned()),
                Value::Array(indices) => output.extend(
                    indices
                        .iter()
                        .map(|i| blocks[i.as_u64().unwrap() as usize].clone()),
                ),
                _ => panic!("invalid selected indices"),
            }
        }
    }
    output
}

#[test]
fn whole_large_emit_is_measured_without_reading_or_serializing_receipt() {
    let original = large_result();
    let (base, events) = recorded_reference(original.clone(), false);
    let action: Arc<Action> = Arc::new(move |v| {
        assert_ne!(v["op"], "read_result", "emission must not materialize");
        base(v)
    });
    let r = direct(
        "tools.emit(await tools.read({})); return {ok:true};",
        action,
    );
    assert_eq!(r["status"], "completed", "{r}");
    assert_eq!(r["result"], json!({"ok":true}));
    assert_eq!(
        r["nativeContentParts"],
        json!([{"reference":{"version":1,"id":"receipt-1"},"indices":null}])
    );
    assert_eq!(count(&events, "start"), 1);
    assert_eq!(count(&events, "measure_result_content"), 1);
    assert_eq!(count(&events, "read_result"), 0);
    assert_eq!(count(&events, "release_result"), 0);
    let encoded: Value = serde_json::from_str(&crate::tool_result(r)).unwrap();
    assert_eq!(encoded["isError"], false);
    assert!(encoded["result"].get("content").is_none());
    assert!(encoded["result"]["details"]
        .get("nativeContentParts")
        .is_none());
    assert!(encoded["result"]["details"].get("nativeContent").is_none());
    assert!(!encoded.to_string().contains(PNG));
}

#[test]
fn two_large_images_mix_with_inline_and_returned_content_in_exact_order() {
    let original = json!({"content":[{"type":"text","text":"signed","textSignature":"native-signature"},
        {"type":"image","mimeType":"image/png","data":"AAAA".repeat(300_000)}],"details":null});
    let (action, events) = recorded_reference(original.clone(), false);
    let r = direct("const a=await tools.read({}); const b=await tools.read({}); tools.emit([{type:'text',text:'before',textSignature:null}]); tools.emit(a); tools.emit([{type:'text',text:'between'}]); tools.emit(b,[1,0,1]); tools.emit([{type:'text',text:'after'}]); return {content:[{type:'text',text:'returned'}],ok:true};", action.clone());
    assert_eq!(r["status"], "completed", "{r}");
    assert_eq!(r["result"]["content"], json!({"promoted":true,"count":1}));
    let encoded = crate::tool_result(r);
    assert!(encoded.len() < 4096);
    assert!(!encoded.contains("AAAA"));
    let envelope: Value = serde_json::from_str(&encoded).unwrap();
    assert_eq!(envelope["contentParts"].as_array().unwrap().len(), 6);
    let blocks = resolved_parts(&action, &envelope["contentParts"]);
    assert_eq!(
        blocks,
        vec![
            json!({"type":"text","text":"before","textSignature":null}),
            original["content"][0].clone(),
            original["content"][1].clone(),
            json!({"type":"text","text":"between"}),
            original["content"][1].clone(),
            original["content"][0].clone(),
            original["content"][1].clone(),
            json!({"type":"text","text":"after"}),
            json!({"type":"text","text":"returned"})
        ]
    );
    assert_eq!(count(&events, "start"), 2);
    assert_eq!(count(&events, "measure_result_content"), 2);
    assert_eq!(count(&events, "release_result"), 0);
    assert!(envelope["result"].get("usage").is_none());
    assert!(envelope["result"].get("addedToolNames").is_none());
}

#[test]
fn large_emit_selections_support_null_empty_reordering_duplicates_and_repeated_receipt() {
    for (selection, indices) in [
        ("", Value::Null),
        (",undefined", Value::Null),
        (",null", Value::Null),
        (",[]", json!([])),
        (",[1,0,1]", json!([1, 0, 1])),
    ] {
        let (action, events) = recorded_reference(large_result(), false);
        let r = direct(&format!("const r=await tools.read({{}}); tools.emit(r{selection}); tools.emit(r,[]); return 9;"), action.clone());
        assert_eq!(r["status"], "completed", "{selection}: {r}");
        assert_eq!(r["nativeContentParts"][0]["indices"], indices);
        assert_eq!(r["nativeContentParts"][1]["indices"], json!([]));
        assert_eq!(count(&events, "measure_result_content"), 2);
        assert_eq!(count(&events, "start"), 1);
        assert_eq!(count(&events, "read_result"), 0);
        assert_eq!(count(&events, "release_result"), 0);
        let blocks = resolved_parts(&action, &r["nativeContentParts"]);
        assert_eq!(
            blocks.len(),
            if indices.is_null() {
                2
            } else {
                indices.as_array().unwrap().len()
            }
        );
    }
}

#[test]
fn small_mutable_index_selection_and_captured_intrinsics_preserve_inline_path() {
    let r = native_run("const r=await tools.read({}); r.content[0].text='mutated'; Array.isArray=()=>false; Number.isInteger=()=>false; Object.defineProperty=()=>{throw Error('poison')}; JSON.stringify=()=>{throw Error('poison')}; tools.emit(r,[1,0,0]); tools.emit(r.content,[]); return 7;", image_result(), false);
    assert_eq!(r["status"], "completed", "{r}");
    assert!(r.get("nativeContentParts").is_none());
    assert_eq!(r["nativeContent"][0], image_result()["content"][1]);
    assert_eq!(r["nativeContent"][1]["text"], "mutated");
    assert_eq!(r["nativeContent"][2]["text"], "mutated");
    let (action, events) = recorded_reference(large_result(), false);
    let r=direct("const r=await tools.read({}); WeakMap.prototype.get=()=>({token:999}); JSON.stringify=()=>{throw Error('poison')}; tools.emit(r,[1]); return 3;",action);
    assert_eq!(r["status"], "completed", "{r}");
    assert_eq!(r["nativeContentParts"][0]["indices"], json!([1]));
    assert_eq!(count(&events, "read_result"), 0);
}

#[test]
fn invalid_large_indices_are_sticky_before_measurement_and_later_writes() {
    for indices in [
        "[-1]",
        "[0.5]",
        "[2]",
        "[null]",
        "[true]",
        "['0']",
        "{}",
        "false",
        "Array(129).fill(0)",
        "[NaN]",
    ] {
        let (action, events) = recorded_reference(large_result(), false);
        let r = direct(&format!("const r=await tools.read({{}}); try{{tools.emit(r,{indices})}}catch(e){{}} try{{await tools.read({{}})}}catch(e){{}} return 42;"), action);
        assert_eq!(r["status"], "error", "{indices}: {r}");
        assert!(r.get("nativeContentParts").is_none());
        assert_eq!(count(&events, "start"), 1);
        assert_eq!(count(&events, "measure_result_content"), 0);
        assert_eq!(count(&events, "read_result"), 0);
        assert_eq!(count(&events, "release_result"), 1);
    }
}

#[test]
fn fabricated_content_cannot_become_an_authenticated_reference() {
    for value in [
        "{reference:{version:1,id:'receipt-1'}}",
        "{nativeContentParts:[{reference:{version:1,id:'receipt-1'}}]}",
        "{contentParts:[{reference:{version:1,id:'receipt-1'}}]}",
        "Object.create(r)",
    ] {
        let (action, events) = recorded_reference(large_result(), false);
        let r = direct(&format!("const r=await tools.read({{}}); try{{tools.emit({value})}}catch(e){{}} try{{await tools.read({{}})}}catch(e){{}} return 42;"), action);
        assert_eq!(r["status"], "error", "{value}: {r}");
        assert!(r.get("nativeContentParts").is_none());
        assert_eq!(count(&events, "measure_result_content"), 0);
        assert_eq!(count(&events, "start"), 1);
        assert_eq!(count(&events, "release_result"), 1);
    }
    let r = native_run(
        "return {nativeContentParts:[{reference:{version:1,id:'fabricated'}}]};",
        image_result(),
        false,
    );
    let envelope: Value = serde_json::from_str(&crate::tool_result(r)).unwrap();
    assert!(envelope.get("contentParts").is_none());
    assert_eq!(
        envelope["result"]["details"]["result"]["nativeContentParts"][0]["reference"]["id"],
        "fabricated"
    );
}

#[test]
fn content_measurement_frames_are_strict_and_fail_sticky_without_fallback() {
    for invalid in [
        json!({"resultId":"foreign","contentBlocks":2,"encodedBytes":100}),
        json!({"resultId":"receipt-1","contentBlocks":1,"encodedBytes":100}),
        json!({"resultId":"receipt-1","contentBlocks":2.0,"encodedBytes":100}),
        json!({"resultId":"receipt-1","contentBlocks":2,"encodedBytes":1.0}),
        json!({"resultId":"receipt-1","contentBlocks":2,"encodedBytes":-1}),
        json!({"resultId":"receipt-1","contentBlocks":2,"encodedBytes":0}),
        json!({"resultId":"receipt-1","contentBlocks":2,"encodedBytes":33554433_u64}),
        json!({"resultId":"receipt-1","contentBlocks":2,"encodedBytes":100,"payload":"x".repeat(MAX_JSON)}),
    ] {
        let (base, events) = recorded_reference(large_result(), false);
        let measures = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let calls = measures.clone();
        let action: Arc<Action> = Arc::new(move |v| {
            if v["op"] == "measure_result_content" {
                calls.fetch_add(1, Ordering::AcqRel);
                return Ok(invalid.clone());
            }
            base(v)
        });
        let r=direct("const r=await tools.read({}); try{tools.emit(r)}catch(e){} try{await tools.read({})}catch(e){} return 42;",action);
        assert_eq!(r["status"], "error", "{r}");
        assert!(r.get("nativeContentParts").is_none());
        assert_eq!(measures.load(Ordering::Acquire), 1);
        assert_eq!(count(&events, "start"), 1);
        assert_eq!(count(&events, "read_result"), 0);
        assert_eq!(count(&events, "release_result"), 1);
    }
}

#[test]
fn emitted_receipt_release_is_sticky_and_never_deletes_selected_storage() {
    let (action, events) = recorded_reference(large_result(), false);
    let r=direct("const r=await tools.read({}); tools.emit(r); try{tools.release(r)}catch(e){} try{await tools.read({})}catch(e){} return 42;",action.clone());
    assert_eq!(r["status"], "error", "{r}");
    assert_eq!(count(&events, "release_result"), 0);
    assert_eq!(count(&events, "start"), 1);
    assert_eq!(
        resolved_parts(&action, &r["nativeContentParts"]),
        large_result()["content"].as_array().unwrap().clone()
    );
    let envelope: Value = serde_json::from_str(&crate::tool_result(r)).unwrap();
    assert_eq!(envelope["isError"], true);
    assert_eq!(envelope["contentParts"].as_array().unwrap().len(), 2);
    assert!(envelope["contentParts"][1]["content"][0]["text"]
        .as_str()
        .unwrap()
        .contains("cannot be released"));
}

#[test]
fn emission_pins_only_selected_refs_and_released_receipts_cannot_emit() {
    let (action, events) = recorded_reference(large_result(), false);
    let r=direct("const a=await tools.read({}); const b=await tools.read({}); const c=await tools.read({}); tools.emit(b,[]);",action.clone());
    assert_eq!(r["status"], "completed", "{r}");
    assert_eq!(count(&events, "release_result"), 2);
    assert_eq!(r["nativeContentParts"][0]["reference"]["id"], "receipt-2");
    for id in ["receipt-1", "receipt-3"] {
        assert!(
            action(json!({"op":"measure_result_content","resultId":id,"indices":null})).is_err()
        );
    }
    let (action, events) = recorded_reference(large_result(), false);
    let r=direct("const r=await tools.read({}); tools.release(r); try{tools.emit(r)}catch(e){} try{await tools.read({})}catch(e){} return 1;",action);
    assert_eq!(r["status"], "error", "{r}");
    assert_eq!(count(&events, "release_result"), 1);
    assert_eq!(count(&events, "measure_result_content"), 0);
    assert_eq!(count(&events, "start"), 1);
}

#[test]
fn recovered_large_error_emission_does_not_force_outer_error() {
    let (action, events) = recorded_reference(large_result(), true);
    let r = direct(
        "try{await tools.read({})}catch(e){tools.emit(e.result,[1])} return {recovered:true};",
        action,
    );
    assert_eq!(r["status"], "completed", "{r}");
    assert_eq!(r["isError"], false);
    assert_eq!(r["calls"][0]["isError"], true);
    assert_eq!(r["result"], json!({"recovered":true}));
    assert_eq!(count(&events, "read_result"), 0);
    let envelope: Value = serde_json::from_str(&crate::tool_result(r)).unwrap();
    assert_eq!(envelope["isError"], false);
    assert_eq!(envelope["contentParts"].as_array().unwrap().len(), 1);
}

#[test]
fn large_emits_survive_later_throw_deadline_and_oom_with_last_error_block() {
    for tail in [
        "throw Error('later');",
        "for(;;){}",
        "try{const a=[];for(;;)a.push(new Array(10000).fill('x'));}catch(e){}",
    ] {
        let (action, events) = recorded_reference(large_result(), false);
        let r = super::execute(
            &format!("tools.emit(await tools.read({{}})); {tail}"),
            if tail == "for(;;){}" { 2000 } else { 5000 },
            action.clone(),
            Arc::new(AtomicBool::new(false)),
        );
        assert!(
            matches!(r["status"].as_str(), Some("error" | "timed_out")),
            "{r}"
        );
        assert_eq!(count(&events, "read_result"), 0);
        assert_eq!(count(&events, "release_result"), 0);
        let envelope: Value = serde_json::from_str(&crate::tool_result(r)).unwrap();
        assert_eq!(envelope["isError"], true);
        let blocks = resolved_parts(&action, &envelope["contentParts"]);
        assert_eq!(&blocks[..2], large_result()["content"].as_array().unwrap());
        assert!(blocks.last().unwrap()["text"]
            .as_str()
            .unwrap()
            .starts_with("codemode error:"));
    }
}

#[test]
fn mixed_content_block_part_and_inline_byte_quotas_are_sticky() {
    for (tail, expected_parts) in [
        ("for(let i=0;i<64;i++)tools.emit(r); try{tools.emit([{type:'text',text:'overflow'}])}catch(e){}",64),
        ("for(let i=0;i<128;i++)tools.emit(r,[]); try{tools.emit(r,[])}catch(e){}",128),
        ("tools.emit(r,[]); tools.emit([{type:'text',text:'x'.repeat(600000)}]); try{tools.emit([{type:'text',text:'x'.repeat(600000)}])}catch(e){}",2),
    ] {
        let (action,events)=recorded_reference(large_result(),false);
        let r=direct(&format!("const r=await tools.read({{}}); {tail} try{{await tools.read({{}})}}catch(e){{}} return 42;"),action);
        assert_eq!(r["status"],"error","{r}");
        // Repeated whole-large content can hit the 32 MiB byte budget first.
        let parts=r["nativeContentParts"].as_array().unwrap();
        if expected_parts==64 { assert!(parts.len()<64); } else { assert_eq!(parts.len(),expected_parts); }
        assert_eq!(count(&events,"start"),1);
        assert_eq!(count(&events,"read_result"),0);
        assert_eq!(count(&events,"release_result"),0);
    }
    // 128 selected native blocks succeed, including repeated selections.
    let (action, events) = recorded_reference(large_result(), false);
    let r=direct("const r=await tools.read({}); tools.emit(r,Array(128).fill(1)); try{tools.emit(r,[1])}catch(e){} try{await tools.read({})}catch(e){} return 42;",action);
    assert_eq!(r["status"], "error", "{r}");
    assert_eq!(r["nativeContentParts"].as_array().unwrap().len(), 1);
    assert_eq!(
        r["nativeContentParts"][0]["indices"]
            .as_array()
            .unwrap()
            .len(),
        128
    );
    assert_eq!(count(&events, "start"), 1);
}

#[test]
fn mixed_content_array_budget_counts_brackets_commas_and_inline_bytes() {
    for (bytes, inline, success) in [
        (MEMORY - OUTPUT_DIAGNOSTIC_RESERVE - 2, false, true),
        (MEMORY - OUTPUT_DIAGNOSTIC_RESERVE - 1, false, false),
        (MEMORY - OUTPUT_DIAGNOSTIC_RESERVE - 2, true, false),
        (MEMORY - 2, false, false),
    ] {
        let (base, events) = recorded_reference(large_result(), false);
        let action: Arc<Action> = Arc::new(move |v| {
            if v["op"] == "measure_result_content" {
                return Ok(
                    json!({"resultId":v["resultId"],"contentBlocks":1,"encodedBytes":bytes}),
                );
            }
            base(v)
        });
        let r=direct(&format!("const r=await tools.read({{}}); {} try{{tools.emit(r,[1])}}catch(e){{}} try{{await tools.read({{}})}}catch(e){{}} return 42;",if inline {"tools.emit([{type:'text',text:'x'}]);"} else {""}),action);
        assert_eq!(
            r["status"],
            if success { "completed" } else { "error" },
            "{r}"
        );
        assert_eq!(count(&events, "start"), if success { 2 } else { 1 });
        assert_eq!(r.get("nativeContentParts").is_some(), success);
        assert_eq!(count(&events, "read_result"), 0);
    }
    // Two measured bytesums fit alone but not with their comma and brackets.
    let (base, events) = recorded_reference(large_result(), false);
    let action: Arc<Action> = Arc::new(move |v| {
        if v["op"] == "measure_result_content" {
            return Ok(
                json!({"resultId":v["resultId"],"contentBlocks":1,"encodedBytes":MEMORY/2-1}),
            );
        }
        base(v)
    });
    let r=direct("const r=await tools.read({}); tools.emit(r,[1]); try{tools.emit(r,[1])}catch(e){} try{await tools.read({})}catch(e){}",action);
    assert_eq!(r["status"], "error", "{r}");
    assert_eq!(r["nativeContentParts"].as_array().unwrap().len(), 1);
    assert_eq!(count(&events, "start"), 1);
}

#[test]
fn large_emission_and_full_result_selection_conflicts_stay_sticky() {
    for tail in [
        "tools.emit(r,[]); try{tools.result(r)}catch(e){}",
        "tools.result(r); try{tools.emit(r,[])}catch(e){}",
    ] {
        let (action, events) = recorded_reference(large_result(), false);
        let r=direct(&format!("const r=await tools.read({{}}); {tail} try{{await tools.read({{}})}}catch(e){{}} return 42;"),action);
        assert_eq!(r["status"], "error", "{r}");
        assert!(r.get("nativeResultReference").is_none());
        assert_eq!(count(&events, "start"), 1);
        assert_eq!(count(&events, "read_result"), 0);
        assert_eq!(
            count(&events, "release_result"),
            if tail.starts_with("tools.result") {
                1
            } else {
                0
            }
        );
    }
}

#[test]
fn terminal_reference_overrides_two_emitted_refs_and_releases_other_storage() {
    for error in [false, true] {
        let mut terminal = large_result();
        terminal["terminate"] = json!(true);
        let expected = terminal.clone();
        let base = native_host(large_result(), false);
        let polls = std::sync::atomic::AtomicUsize::new(0);
        let tool: Arc<Action> = Arc::new(move |v| {
            if v["op"] == "poll" && polls.fetch_add(1, Ordering::AcqRel) >= 2 {
                return Ok(
                    json!({"requestId":"private","status":"done","isError":error,"result":terminal}),
                );
            }
            base(v)
        });
        let peer = reference_peer(tool);
        let events = Arc::new(Mutex::new(Vec::new()));
        let shared = events.clone();
        let action: Arc<Action> = Arc::new(move |v| {
            shared.lock().unwrap().push(v.clone());
            peer(v)
        });
        let r=direct("const a=await tools.read({}); const b=await tools.read({}); tools.emit(a); tools.emit(b,[1]); tools.emit([{type:'text',text:'discard'}]); await tools.read({}); throw Error('resumed');",action.clone());
        assert_eq!(r["status"], "terminated", "{r}");
        assert_eq!(r["isError"], error);
        assert!(r.get("nativeContentParts").is_none());
        assert_eq!(count(&events, "read_result"), 0);
        assert_eq!(count(&events, "release_result"), 2);
        let envelope: Value = serde_json::from_str(&crate::tool_result(r)).unwrap();
        assert!(envelope.get("contentParts").is_none());
        assert!(envelope.get("result").is_none());
        assert_eq!(envelope["resultReference"]["id"], "receipt-3");
        assert_eq!(
            resolve_fixture(&action, &envelope["resultReference"]),
            expected
        );
    }
}

#[test]
fn cancellation_during_or_after_measurement_never_hands_off_emitted_refs() {
    for cancel_at in [1, 2] {
        let (base, events) = recorded_reference(large_result(), false);
        let cancel = Arc::new(AtomicBool::new(false));
        let token = cancel.clone();
        let measurements = std::sync::atomic::AtomicUsize::new(0);
        let action: Arc<Action> = Arc::new(move |v| {
            let measure = v["op"] == "measure_result_content";
            let reply = base(v)?;
            if measure && measurements.fetch_add(1, Ordering::AcqRel) + 1 == cancel_at {
                token.store(true, Ordering::Release);
            }
            Ok(reply)
        });
        let r = super::execute(
            "const r=await tools.read({}); tools.emit(r); tools.emit(r,[]); return 42;",
            5000,
            action,
            cancel,
        );
        assert_eq!(r["status"], "cancelled", "{r}");
        assert_eq!(r["isError"], true);
        assert!(r.get("nativeContentParts").is_none());
        assert_eq!(count(&events, "release_result"), 1);
        assert_eq!(count(&events, "read_result"), 0);
    }
}

#[test]
fn lost_binding_host_measurement_and_lifecycle_errors_suppress_all_attachments() {
    for op in ["measure_result_content", "start", "poll", "dispose"] {
        let (base, events) = recorded_reference(large_result(), false);
        let attempts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let seen = attempts.clone();
        let expired = AtomicBool::new(false);
        let action: Arc<Action> = Arc::new(move |v| {
            if expired.load(Ordering::Acquire)
                || (v["op"] == op && seen.fetch_add(1, Ordering::AcqRel) > 0)
            {
                expired.store(true, Ordering::Release);
                return Err("lost native invocation binding".into());
            }
            base(v)
        });
        let r=direct("const r=await tools.read({}); tools.emit(r); try{tools.emit(r,[]); await tools.read({})}catch(e){} return 42;",action);
        assert_eq!(r["status"], "error", "{op}: {r}");
        assert!(r.get("nativeContentParts").is_none(), "{op}: {r}");
        assert_eq!(count(&events, "read_result"), 0);
        assert_eq!(
            count(&events, "release_result"),
            0,
            "expired owner rejects cleanup too"
        );
    }
}

#[test]
fn large_emission_error_framing_can_drop_plan_but_keeps_bounded_explicit_error() {
    let (action, events) = recorded_reference(large_result(), false);
    let r = direct(
        "tools.emit(await tools.read({}),[]); return 'x'.repeat(1048000);",
        action,
    );
    assert_eq!(r["status"], "completed", "{r}");
    assert_eq!(
        count(&events, "release_result"),
        0,
        "handoff retained until native finish, not plugin destroy"
    );
    let text = crate::tool_result(r);
    assert!(text.len() <= MAX_JSON);
    let envelope: Value = serde_json::from_str(&text).unwrap();
    // Increase the diagnostic metadata explicitly to exercise the independent
    // Done-frame fallback, not guest or attachment payload truncation.
    let text = crate::tool_result(
        json!({"status":"completed","isError":false,"result":"x".repeat(MAX_JSON),"nativeContentParts":envelope["contentParts"],"logs":[],"calls":[],"terminate":false}),
    );
    assert!(text.len() <= MAX_JSON);
    let envelope: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(envelope["isError"], true);
    assert!(envelope.get("contentParts").is_none());
    assert!(envelope["result"]["details"]["error"]
        .as_str()
        .unwrap()
        .contains("exceeds 1 MiB"));
}

#[test]
fn emitted_reference_survives_throw_while_pending_calls_are_cancelled_and_disposed() {
    let (base, events) = recorded_reference(large_result(), false);
    let action: Arc<Action> = Arc::new(move |v| {
        // No finalized storage exists for the unawaited second child. Its
        // best-effort release is expected to fail, not invalidate receipt-1.
        base(v)
    });
    let r=direct("tools.emit(await tools.read({})); tools.read({}); throw Error('later with pending child');",action.clone());
    assert_eq!(r["status"], "error", "{r}");
    assert_eq!(r["nativeContentParts"][0]["reference"]["id"], "receipt-1");
    assert_eq!(count(&events, "cancel"), 1);
    assert_eq!(count(&events, "dispose"), 2);
    assert_eq!(count(&events, "release_result"), 1);
    assert_eq!(count(&events, "read_result"), 0);
    assert_eq!(
        resolved_parts(&action, &r["nativeContentParts"]),
        large_result()["content"].as_array().unwrap().clone()
    );
}

#[test]
fn lost_owner_during_unselected_reference_cleanup_cannot_emit_success() {
    let (base, events) = recorded_reference(large_result(), false);
    let action: Arc<Action> = Arc::new(move |v| {
        if v["op"] == "release_result" {
            return Err("owner expired".into());
        }
        base(v)
    });
    let r = direct(
        "const a=await tools.read({}); const b=await tools.read({}); tools.emit(a); return 42;",
        action,
    );
    assert_eq!(r["status"], "error", "{r}");
    assert_eq!(r["isError"], true);
    assert!(r.get("nativeContentParts").is_none());
    assert_eq!(count(&events, "start"), 2);
    assert_eq!(count(&events, "read_result"), 0);
}

#[test]
fn small_invalid_indices_and_large_indices_serialization_fail_sticky() {
    for indices in ["[-1]", "[2]", "[0.25]", "{}", "'0'", "Array(129).fill(0)"] {
        let r=native_run(&format!("const r=await tools.read({{}}); try{{tools.emit(r,{indices})}}catch(e){{}} try{{await tools.read({{}})}}catch(e){{}} return 42;"),image_result(),false);
        assert_eq!(r["status"], "error", "{indices}: {r}");
        assert_eq!(r["calls"].as_array().unwrap().len(), 1);
        assert!(r.get("nativeContentParts").is_none());
    }
    for indices in [
        "[1n]",
        "(()=>{const a=[];a.push(a);return a})()",
        "{toJSON(){throw Error('indices')}}",
    ] {
        let (action, events) = recorded_reference(large_result(), false);
        let r=direct(&format!("const r=await tools.read({{}}); try{{tools.emit(r,{indices})}}catch(e){{}} try{{await tools.read({{}})}}catch(e){{}} return 42;"),action);
        assert_eq!(r["status"], "error", "{indices}: {r}");
        assert_eq!(count(&events, "start"), 1);
        assert_eq!(count(&events, "measure_result_content"), 0);
        assert_eq!(count(&events, "read_result"), 0);
        assert_eq!(count(&events, "release_result"), 1);
    }
}

fn overflow_result() -> Value {
    json!({"content":[{"type":"text","text":"x".repeat(1_800_000)},
        {"type":"image","mimeType":"image/png","data":"AAAA".repeat(300_000)}],"details":null})
}

fn recorded_overflow() -> (Arc<Action>, Arc<Mutex<Vec<Value>>>) {
    let (base, events) = recorded_reference(overflow_result(), false);
    let action: Arc<Action> = Arc::new(move |v| {
        let list = v["op"] == "list";
        let mut reply = base(v)?;
        if list {
            reply["tools"]
                .as_array_mut()
                .unwrap()
                .push(json!({"name":"write","description":"fixture mutation","parameters":{}}));
        }
        Ok(reply)
    });
    (action, events)
}

const EMIT_THEN_OVERFLOW: &str = "const a=await tools.read({}); const b=await tools.read({}); const c=await tools.read({}); const d=await tools.read({}); tools.emit(a,[1]); tools.emit(a,[1]); tools.emit(b,[1]); try{tools.emit(c,Array(20).fill(0))}catch(e){} try{await tools.write({})}catch(e){} try{tools.result(a)}catch(e){} return 42;";

#[test]
fn ordinary_native_measure_quota_error_recovers_only_distinct_pinned_receipts() {
    let original = overflow_result();
    let (action, events) = recorded_overflow();
    let r = direct(EMIT_THEN_OVERFLOW, action.clone());
    assert_eq!(r["status"], "error", "{r}");
    assert!(r["error"].as_str().unwrap().contains("32 MiB"));
    assert!(r.get("nativeResultReference").is_none());
    assert_eq!(r["nativeContentParts"].as_array().unwrap().len(), 3);
    assert_eq!(
        count(&events, "start"),
        4,
        "sticky error blocks later writes"
    );
    assert_eq!(count(&events, "read_result"), 0);
    assert_eq!(
        count(&events, "release_result"),
        2,
        "all and only unselected refs"
    );
    let recorded = events.lock().unwrap();
    let probes = recorded
        .iter()
        .filter(|v| v["op"] == "measure_result_content" && v["indices"] == json!([]))
        .collect::<Vec<_>>();
    assert_eq!(probes.len(), 2);
    let ids: HashSet<_> = probes
        .iter()
        .map(|v| v["resultId"].as_str().unwrap())
        .collect();
    assert_eq!(ids, HashSet::from(["receipt-1", "receipt-2"]));
    let first_probe = recorded
        .iter()
        .position(|v| v["op"] == "measure_result_content" && v["indices"] == json!([]))
        .unwrap();
    assert_eq!(
        recorded[..first_probe]
            .iter()
            .filter(|v| v["op"] == "release_result")
            .count(),
        2
    );
    assert_eq!(
        recorded
            .iter()
            .filter(|v| v["op"] == "measure_result_content"
                && v["indices"].as_array().is_some_and(|a| a.len() == 20))
            .count(),
        1,
        "failed selection is never replayed"
    );
    drop(recorded);
    let envelope: Value = serde_json::from_str(&crate::tool_result(r)).unwrap();
    assert_eq!(envelope["isError"], true);
    let parts = envelope["contentParts"].as_array().unwrap();
    assert_eq!(parts.len(), 4);
    assert!(parts[3]["content"][0]["text"]
        .as_str()
        .unwrap()
        .starts_with("codemode error:"));
    let blocks = resolved_parts(&action, &envelope["contentParts"]);
    assert_eq!(&blocks[..3], &vec![original["content"][1].clone(); 3]);
}

#[test]
fn emitted_liveness_probe_requires_exact_bounded_zero_measurements_for_every_owner() {
    for invalid in [
        None,
        Some(json!({"resultId":"foreign","contentBlocks":0,"encodedBytes":0})),
        Some(json!({"resultId":"receipt-2","contentBlocks":1,"encodedBytes":0})),
        Some(json!({"resultId":"receipt-2","contentBlocks":0,"encodedBytes":1})),
        Some(json!({"resultId":"receipt-2","contentBlocks":0.0,"encodedBytes":0})),
        Some(json!({"resultId":"receipt-2","contentBlocks":0,"encodedBytes":0.0})),
        Some(json!({"resultId":"receipt-2","contentBlocks":0,"encodedBytes":0,"extra":true})),
        Some(
            json!({"resultId":"receipt-2","contentBlocks":0,"encodedBytes":0,"extra":"x".repeat(MAX_JSON)}),
        ),
        Some(json!({"resultId":"receipt-2","contentBlocks":0,"encodedBytes":0,"status":"failed"})),
    ] {
        let (base, events) = recorded_overflow();
        let probes = Arc::new(Mutex::new(Vec::new()));
        let seen = probes.clone();
        let action: Arc<Action> = Arc::new(move |v| {
            if v["op"] == "measure_result_content" && v["indices"] == json!([]) {
                seen.lock().unwrap().push(v["resultId"].clone());
                if v["resultId"] == "receipt-2" {
                    return invalid
                        .clone()
                        .ok_or_else(|| "expired owning invocation".into());
                }
            }
            base(v)
        });
        let r = direct(EMIT_THEN_OVERFLOW, action);
        assert_eq!(r["status"], "error", "{r}");
        assert!(r.get("nativeContentParts").is_none(), "{r}");
        assert!(r.get("nativeResultReference").is_none());
        let seen = probes.lock().unwrap();
        assert!(seen.contains(&json!("receipt-2")));
        assert_eq!(seen.iter().collect::<HashSet<_>>().len(), seen.len());
        assert_eq!(count(&events, "start"), 4);
        assert_eq!(count(&events, "release_result"), 4);
        assert_eq!(count(&events, "read_result"), 0);
    }
}

#[test]
fn cancellation_never_recovers_emitted_receipts_even_during_liveness_probe() {
    for cancel_on_probe in [false, true] {
        let (base, events) = recorded_overflow();
        let cancel = Arc::new(AtomicBool::new(false));
        let token = cancel.clone();
        let probes = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let seen = probes.clone();
        let action: Arc<Action> = Arc::new(move |v| {
            let probe = v["op"] == "measure_result_content" && v["indices"] == json!([]);
            let overflow = v["indices"].as_array().is_some_and(|a| a.len() == 20);
            let reply = base(v);
            if probe {
                seen.fetch_add(1, Ordering::AcqRel);
            }
            if (cancel_on_probe && probe) || (!cancel_on_probe && overflow) {
                token.store(true, Ordering::Release);
            }
            reply
        });
        let r = super::execute(EMIT_THEN_OVERFLOW, 5000, action, cancel);
        assert_ne!(r["status"], "completed", "{r}");
        assert!(r.get("nativeContentParts").is_none());
        assert_eq!(probes.load(Ordering::Acquire), usize::from(cancel_on_probe));
        assert_eq!(count(&events, "release_result"), 4);
        assert_eq!(count(&events, "read_result"), 0);
    }
}

#[test]
fn known_unselected_cleanup_failure_forbids_liveness_recovery() {
    for (failed_reply, explicit_release) in
        [(false, false), (true, false), (false, true), (true, true)]
    {
        let (base, events) = recorded_overflow();
        let probes = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let seen = probes.clone();
        let action: Arc<Action> = Arc::new(move |v| {
            if v["op"] == "measure_result_content" && v["indices"] == json!([]) {
                seen.fetch_add(1, Ordering::AcqRel);
            }
            if v["op"] == "release_result" && v["resultId"] == "receipt-3" {
                return if failed_reply {
                    Ok(json!({"resultId":"receipt-3","status":"failed"}))
                } else {
                    Err("uncertain cleanup".into())
                };
            }
            base(v)
        });
        let code = if explicit_release {
            EMIT_THEN_OVERFLOW.replace(
                "try{tools.emit(c,Array(20).fill(0))}",
                "try{tools.release(c)}",
            )
        } else {
            EMIT_THEN_OVERFLOW.to_owned()
        };
        let r = direct(&code, action);
        assert_eq!(r["status"], "error", "{r}");
        assert!(r.get("nativeContentParts").is_none());
        assert_eq!(probes.load(Ordering::Acquire), 0);
        assert_eq!(
            count(&events, "release_result"),
            3,
            "other unselected and both pinned refs cleaned"
        );
        assert_eq!(count(&events, "read_result"), 0);
    }
}

#[test]
fn diagnostic_reserve_keeps_near_native_limit_content_and_escaped_error_in_budget() {
    // Measurement metadata models a content array right at the engine bound;
    // payload stays native, so this test need not allocate a 32 MiB guest value.
    let bytes = MEMORY - OUTPUT_DIAGNOSTIC_RESERVE - 2;
    let (base, events) = recorded_reference(large_result(), false);
    let action: Arc<Action> = Arc::new(move |v| {
        if v["op"] == "measure_result_content" {
            return Ok(json!({"resultId":v["resultId"],"contentBlocks":1,"encodedBytes":bytes}));
        }
        base(v)
    });
    let r = direct(
        "tools.emit(await tools.read({}),[1]); throw Error(String.fromCharCode(1).repeat(512));",
        action,
    );
    assert_eq!(r["status"], "error", "{r}");
    assert!(r.get("nativeContentParts").is_some());
    assert_eq!(count(&events, "read_result"), 0);
    let envelope: Value = serde_json::from_str(&crate::tool_result(r)).unwrap();
    let parts = envelope["contentParts"].as_array().unwrap();
    assert_eq!(parts.len(), 2);
    let diagnostic = &parts[1]["content"][0];
    assert_eq!(
        diagnostic["text"].as_str().unwrap(),
        format!(
            "codemode error: {}",
            char::from(1u8).to_string().repeat(512)
        )
    );
    assert!(diagnostic.to_string().len() > 3072);
    assert!(bytes + 1 + diagnostic.to_string().len() + 2 <= MEMORY);
}

// Use the same recorded bridge for both representations and include a real
// mutating tool, so catching a bad directive must not admit a later start.
fn indices_fixture(large: bool) -> (Arc<Action>, Arc<Mutex<Vec<Value>>>) {
    let (base, events) = recorded_reference(
        if large {
            large_result()
        } else {
            image_result()
        },
        false,
    );
    let action: Arc<Action> = Arc::new(move |v| {
        let list = v["op"] == "list";
        let mut reply = base(v)?;
        if list {
            reply["tools"]
                .as_array_mut()
                .unwrap()
                .push(json!({"name":"write","description":"mutation","parameters":{}}));
        }
        Ok(reply)
    });
    (action, events)
}

#[test]
fn raw_emit_indices_reject_coercion_holes_and_invalid_values_sticky() {
    for large in [false, true] {
        for indices in [
            "NaN",
            "Infinity",
            "-Infinity",
            "0.5",
            "0",
            "true",
            "'0'",
            "new Number(0)",
            "new Number(NaN)",
            "{toJSON(){return null}}",
            "{toJSON(){return [1]}}",
            "{toJSON(){tools.write({});return null}}",
            "Object.create({toJSON(){return [1]}})",
            "{toJSON(){throw Error('must not serialize')}}",
            "Array(1)",
            "[undefined]",
            "[null]",
            "[NaN]",
            "[Infinity]",
            "[-Infinity]",
            "[0.5]",
            "[-1]",
            "[new Number(0)]",
            "[{toJSON(){return 0}}]",
            "[1n]",
            "[Symbol('index')]",
            "Array(129).fill(0)",
            "Object.assign([NaN],{toJSON(){return [0]}})",
            "Object.assign([2],{toJSON(){return [0]}})",
            "new Proxy([],{get(t,k){return k==='length' ? NaN : 0}})",
            "new Proxy([],{get(t,k){return k==='length' ? -1 : 0}})",
            "new Proxy([],{get(t,k){return k==='length' ? 0.5 : 0}})",
            "new Proxy([],{get(t,k){return k==='length' ? Infinity : 0}})",
            "new Proxy([0],{get(t,k){if(k==='0')throw Error('index getter');return t[k]}})",
        ] {
            let (action, events) = indices_fixture(large);
            let r = direct(&format!("const r=await tools.read({{}}); try{{tools.emit(r,{indices})}}catch(e){{}} try{{await tools.write({{}})}}catch(e){{}} return 42;"), action);
            assert_eq!(r["status"], "error", "large={large}, {indices}: {r}");
            assert_eq!(r["result"], Value::Null);
            assert!(r.get("nativeContentParts").is_none(), "{r}");
            assert!(r.get("nativeContent").is_none(), "no blocks may leak: {r}");
            assert_eq!(count(&events, "start"), 1, "large={large}, {indices}");
            assert_eq!(count(&events, "measure_result_content"), 0);
            assert_eq!(count(&events, "release_result"), 1);
            assert!(!events
                .lock()
                .unwrap()
                .iter()
                .any(|e| e["op"] == "start" && e["name"] == "write"));
        }
    }
}

#[test]
fn emit_indices_ignore_caller_array_to_json_and_arbitrary_metadata() {
    for large in [false, true] {
        for setup in [
            "indices.toJSON=()=>null;",
            "indices.toJSON=()=>[0];",
            "Object.defineProperty(indices,'toJSON',{get(){throw Error('toJSON read')}});",
            "Object.defineProperty(indices,'metadata',{get(){throw Error('metadata read')}});",
            "indices.toJSON=()=>{tools.write({});return null};",
        ] {
            let (action, events) = indices_fixture(large);
            let r = direct(&format!("const r=await tools.read({{}}); const indices=[1,1]; {setup} tools.emit(r,indices); return 9;"), action.clone());
            assert_eq!(r["status"], "completed", "large={large}, {setup}: {r}");
            let expected = if large {
                large_result()
            } else {
                image_result()
            };
            let blocks = if large {
                assert_eq!(r["nativeContentParts"][0]["indices"], json!([1, 1]));
                resolved_parts(&action, &r["nativeContentParts"])
            } else {
                r["nativeContent"].as_array().unwrap().clone()
            };
            assert_eq!(blocks, vec![expected["content"][1].clone(); 2]);
            assert_eq!(count(&events, "start"), 1);
            assert_eq!(count(&events, "measure_result_content"), usize::from(large));
        }
    }
}

#[test]
fn emit_indices_snapshot_proxy_length_and_each_index_exactly_once() {
    for large in [false, true] {
        let (action, events) = indices_fixture(large);
        let r = direct("const r=await tools.read({}); let lengths=0, reads=0; const indices=new Proxy([1],{get(t,k){if(k==='length'){if(++lengths>1)throw Error('length reread');return 1} if(k==='0'){if(++reads>1)throw Error('index reread');return 1} throw Error('unexpected property '+String(k))}}); tools.emit(r,indices); return {lengths,reads};", action.clone());
        assert_eq!(r["status"], "completed", "large={large}: {r}");
        assert_eq!(r["result"], json!({"lengths":1,"reads":1}));
        let expected = if large {
            large_result()
        } else {
            image_result()
        };
        if large {
            assert_eq!(r["nativeContentParts"][0]["indices"], json!([1]));
            assert_eq!(
                resolved_parts(&action, &r["nativeContentParts"]),
                vec![expected["content"][1].clone()]
            );
        } else {
            assert_eq!(r["nativeContent"], json!([expected["content"][1]]));
        }
        assert_eq!(count(&events, "start"), 1);
    }
}

#[test]
fn emit_indices_normalization_uses_captured_intrinsics_and_safe_array_prototypes() {
    for large in [false, true] {
        let boundary = format!("[{}]", vec!["1"; 128].join(","));
        for selection in ["[]", "[1]", "[1,0,1]", &boundary] {
            let (action, events) = indices_fixture(large);
            let r = direct(&format!("const r=await tools.read({{}}); const indices={selection}; const define=Object.defineProperty; const descriptor=Object.create(null); descriptor.set=()=>{{throw Error('array setter')}}; descriptor.configurable=true; define(Array.prototype,{},descriptor); Array.prototype.toJSON=()=>null; Number.prototype.toJSON=()=>null; Number.isInteger=()=>false; Array.isArray=()=>false; Object.setPrototypeOf=Object.freeze=Object.create=Object.defineProperty=()=>{{throw Error('poison')}}; JSON.stringify=()=>null; tools.emit(r,indices); return 9;", if large { 0 } else { 127 }), action.clone());
            assert_eq!(r["status"], "completed", "large={large}, {selection}: {r}");
            let indices: Value = serde_json::from_str(selection).unwrap();
            let expected = if large {
                large_result()
            } else {
                image_result()
            };
            let expected: Vec<Value> = indices
                .as_array()
                .unwrap()
                .iter()
                .map(|i| expected["content"][i.as_u64().unwrap() as usize].clone())
                .collect();
            if large {
                assert_eq!(r["nativeContentParts"][0]["indices"], indices);
                assert_eq!(count(&events, "read_result"), 0);
                assert_eq!(resolved_parts(&action, &r["nativeContentParts"]), expected);
            } else {
                assert_eq!(r["nativeContent"], json!(expected));
            }
            assert_eq!(count(&events, "start"), 1);
        }
    }
}

#[test]
fn small_emit_indices_keep_all_empty_subset_duplicates_and_raw_mutation_semantics() {
    for raw in [false, true] {
        for (selection, indices) in [
            ("", vec![0, 1]),
            (",undefined", vec![0, 1]),
            (",null", vec![0, 1]),
            (",[]", vec![]),
            (",[1,0,1]", vec![1, 0, 1]),
        ] {
            let (action, events) = indices_fixture(false);
            let source = if raw { "r.content" } else { "r" };
            let r = direct(&format!("const r=await tools.read({{}}); r.content[0].text='mutated'; tools.emit({source}{selection}); return 9;"), action);
            assert_eq!(r["status"], "completed", "raw={raw}, {selection}: {r}");
            let mut expected = image_result();
            expected["content"][0]["text"] = json!("mutated");
            let blocks: Vec<Value> = indices
                .into_iter()
                .map(|i| expected["content"][i].clone())
                .collect();
            assert_eq!(r["nativeContent"], json!(blocks));
            assert_eq!(count(&events, "start"), 1);
        }
    }
}
