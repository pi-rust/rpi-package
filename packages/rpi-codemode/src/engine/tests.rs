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
                    json!({"protocolVersion":1,"capabilities":{"resultEnvelope":1,"hostAccounting":1,"parentProgress":1},"tools":[{"name":"echo","description":"test","parameters":{"type":"object"}},{"name":"other","description":"test","parameters":{"type":"object"}}],"nextOffset":null}),
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
            json!({"protocolVersion":1,"capabilities":{"resultEnvelope":1,"hostAccounting":1,"parentProgress":1},"tools":[],"nextOffset":if v["offset"]==0 {json!(32)} else {Value::Null}}),
        )
    };
    assert_eq!(list_page(&action, 0, 32).unwrap()["nextOffset"], 32);
    for page in [
        json!({"protocolVersion":0,"tools":[],"nextOffset":null}),
        json!({"protocolVersion":1,"capabilities":{"resultEnvelope":1,"hostAccounting":1,"parentProgress":1},"tools":[],"nextOffset":0}),
        json!({"protocolVersion":1,"capabilities":{"resultEnvelope":1,"hostAccounting":1,"parentProgress":1},"tools":[{}],"nextOffset":null}),
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
            json!({"protocolVersion":1,"capabilities":{"resultEnvelope":1,"hostAccounting":1,"parentProgress":1},"tools":entries,"nextOffset":if offset==0 {json!(1)} else {Value::Null}}),
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
            json!({"protocolVersion":1,"capabilities":{"resultEnvelope":1,"hostAccounting":1,"parentProgress":1},"tools":[{"name":"schema","description":"test","parameters":{"type":"object","properties":{"runtimeKey":{"type":"string"},"ordinary":{"type":"string"}}}}],"nextOffset":null}),
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
                json!({"protocolVersion":1,"capabilities":{"resultEnvelope":1,"hostAccounting":1,"parentProgress":1},
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
fn capabilities_require_all_three_exact_integer_versions_before_guest_execution() {
    for key in ["resultEnvelope", "hostAccounting", "parentProgress"] {
        for invalid in [
            Value::Null,
            json!(0),
            json!(2),
            json!("1"),
            json!(true),
            json!(1.0),
        ] {
            let mut caps = json!({"resultEnvelope":1,"hostAccounting":1,"parentProgress":1});
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
        "capabilities":{"resultEnvelope":1,"hostAccounting":1,"parentProgress":1,"private":"hidden"},"private":"hidden"}))
    };
    let page = list_page(&a, 0, 32).unwrap();
    assert_eq!(
        page["capabilities"],
        json!({"resultEnvelope":1,"hostAccounting":1,"parentProgress":1})
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
fn emitted_reserved_metadata_is_filtered_and_native_text_signature_retained() {
    let r = native_run("tools.emit({runtimeHandle:42,content:[{type:'text',text:'ok',textSignature:'native-signature',runtimeKey:'secret',__rpi:{nested:[{runtimeHandle:42}]}}]}); return 1;",image_result(),false);
    assert_eq!(r["status"], "completed", "{r}");
    assert!(!r.to_string().contains("secret"));
    assert_eq!(
        r["nativeContent"],
        json!([{"type":"text","text":"ok","textSignature":"native-signature"}])
    );
    let output: Value = serde_json::from_str(&crate::tool_result(r)).unwrap();
    assert_eq!(
        output["result"]["content"][0]["textSignature"],
        "native-signature"
    );

    let mut nested = json!({"content":[{"type":"text","text":"ok","meta":{"label":"public","nested":[{"runtimeKey":"secret","__rpiRuntimeKey":"secret","runtimeHandle":42,"__rpi":{}}]}}]});
    public_value(&mut nested);
    assert_eq!(
        nested["content"][0]["meta"],
        json!({"label":"public","nested":[{}]})
    );
    assert!(content::blocks(nested).unwrap_err().contains("unsupported"));
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
fn terminal_error_keeps_both_flags_and_native_blocks_even_when_content_validation_fails() {
    for content in [image_result()["content"].clone(), json!([{"type":"audio"}])] {
        let r = native_run(
            "await tools.read({}); throw Error('must not resume');",
            json!({"content":content,"terminate":true}),
            true,
        );
        assert_eq!(r["terminate"], true, "{r}");
        let output: Value = serde_json::from_str(&crate::tool_result(r)).unwrap();
        assert_eq!(output["isError"], true);
        assert_eq!(output["result"]["terminate"], true);
        if content[0]["type"] == "text" {
            assert_eq!(output["result"]["content"][0], content[0]);
            assert_eq!(output["result"]["content"][1], content[1]);
            assert_eq!(
                output["result"]["content"][2]["text"],
                "codemode error: terminal tool returned isError"
            );
        }
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
