use rpi_mcp_core::{Control, McpRuntime, RequestContext};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

struct Fixture {
    dir: PathBuf,
    context: RequestContext,
    params: Value,
}
impl Fixture {
    fn new() -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let dir = PathBuf::from("C:/Users/bigfish/.cargo/target/rpi-acp-isolated-review/tmp").join(
            format!(
                "catalog-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            ),
        );
        std::fs::create_dir_all(&dir).unwrap();
        let capture = dir.join("calls.jsonl");
        std::fs::write(
            dir.join("mcp.json"),
            json!({"mcpServers":{"fake":{
                "command":env!("CARGO_BIN_EXE_mcp-catalog-fixture"),"args":[capture],"timeout":5
            }}})
            .to_string(),
        )
        .unwrap();
        Self {
            context: RequestContext {
                scope: "test".into(),
                cwd: dir.clone(),
            },
            dir,
            params: json!({"configPath":"mcp.json","server":"fake"}),
        }
    }
    fn calls(&self) -> Vec<Value> {
        std::fs::read_to_string(self.dir.join("calls.jsonl"))
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

#[test]
fn wire_calls_lossless_pages_templates_and_isolation() {
    let f = Fixture::new();
    let runtime = McpRuntime::new();
    let control = Control::default();
    let page = runtime
        .prompts_list(&f.context, &f.params, &control)
        .unwrap();
    assert_eq!(
        page.prompts[0].arguments.as_ref().unwrap()[0].extra["custom"],
        9
    );
    assert_eq!(page.extra["extension"], 42);
    assert_eq!(page.next_cursor.as_deref(), Some("end"));
    let mut params = f.params.clone();
    params["cursor"] = json!("end");
    assert_eq!(
        runtime
            .prompts_list(&f.context, &params, &control)
            .unwrap()
            .prompts[0]
            .name,
        "second"
    );
    params = f.params.clone();
    params["maxPages"] = json!(2);
    let all = runtime.prompts_list(&f.context, &params, &control).unwrap();
    assert_eq!(all.prompts.len(), 2);
    assert!(all.next_cursor.is_none());
    assert_eq!(all.extra["pageResults"][1]["_meta"]["page"], 2);
    params = f.params.clone();
    params["name"] = json!("first");
    params["arguments"] = json!({"topic":"hello"});
    let result = runtime.prompts_get(&f.context, &params, &control).unwrap();
    assert_eq!(result.messages[0].content.fields["data"], "QUJD");
    assert_eq!(result.messages[1].content.fields["resource"]["custom"], 7);
    assert_eq!(result.messages[2].content.kind, "future");
    assert_eq!(result.extra["_meta"]["modelHints"][0]["name"], "model");
    assert_eq!(
        result.extra["echo"],
        json!({"name":"first","arguments":{"topic":"hello"}})
    );
    assert_eq!(
        PathBuf::from(result.extra["cwd"].as_str().unwrap())
            .canonicalize()
            .unwrap(),
        f.dir.canonicalize().unwrap()
    );
    let pid = result.extra["pid"].clone();
    let other = McpRuntime::new();
    assert_ne!(
        other
            .prompts_get(&f.context, &params, &control)
            .unwrap()
            .extra["pid"],
        pid
    );
    let context = RequestContext {
        scope: "other".into(),
        ..f.context.clone()
    };
    assert_ne!(
        runtime
            .prompts_get(&context, &params, &control)
            .unwrap()
            .extra["pid"],
        pid
    );
    let templates = runtime
        .resource_templates_list(&f.context, &f.params, &control)
        .unwrap();
    assert_eq!(templates.resource_templates[0].uri_template, "mem://{name}");
    assert_eq!(templates.resource_templates[0].extra["_meta"]["hint"], true);
    params["action"] = json!("templates/list");
    assert_eq!(
        runtime.resources(&f.context, &params, &control).unwrap()["resourceTemplates"][0]["name"],
        "memory"
    );
    drop(other);
    drop(runtime);
    let calls = f.calls();
    assert_eq!(
        calls.iter().filter(|c| c["method"] == "initialize").count(),
        3
    );
    assert_eq!(
        calls
            .iter()
            .filter(|c| c["method"] == "prompts/list")
            .count(),
        4
    );
    assert_eq!(calls[0]["params"]["protocolVersion"], "2025-06-18");
    assert_eq!(calls[1]["method"], "notifications/initialized");
    assert_eq!(calls[2]["params"], json!({}));
    assert_eq!(calls[3]["params"], json!({"cursor":"end"}));
    assert!(!calls.iter().any(|c| c["method"] == "tools/list"));
}

#[test]
fn bounds_validation_cycles_and_resumption() {
    let f = Fixture::new();
    let runtime = McpRuntime::new();
    let c = Control::default();
    for cursor in ["cycle", "loop-a"] {
        let mut p = f.params.clone();
        p["cursor"] = json!(cursor);
        p["maxPages"] = json!(100);
        assert!(runtime
            .prompts_list(&f.context, &p, &c)
            .unwrap_err()
            .contains("repeated"));
    }
    let mut p = f.params.clone();
    p["cursor"] = json!("page-0");
    p["maxPages"] = json!(100);
    assert_eq!(
        runtime
            .prompts_list(&f.context, &p, &c)
            .unwrap()
            .next_cursor
            .as_deref(),
        Some("page-100")
    );
    let count = f
        .calls()
        .iter()
        .filter(|c| c["method"] == "prompts/list")
        .count();
    assert_eq!(count, 103);
    p["cursor"] = json!("many");
    assert!(runtime
        .prompts_list(&f.context, &p, &c)
        .unwrap_err()
        .contains("16384"));
    p = f.params.clone();
    p["maxResponseBytes"] = json!(10);
    assert!(runtime
        .prompts_list(&f.context, &p, &c)
        .unwrap_err()
        .contains("byte limit"));
    p["name"] = json!("first");
    assert!(runtime
        .prompts_get(&f.context, &p, &c)
        .unwrap_err()
        .contains("byte limit"));
    let before = f.calls().len();
    for bad in [
        json!({"action":"get","name":"\n"}),
        json!({"action":"get","name":"first","arguments":{"topic":4}}),
        json!({"action":"invalid"}),
    ] {
        let mut p = f.params.clone();
        p.as_object_mut()
            .unwrap()
            .extend(bad.as_object().unwrap().clone());
        assert!(runtime.prompts(&f.context, &p, &c).is_err());
    }
    assert_eq!(f.calls().len(), before);
}

#[test]
fn cancellation_timeout_eof_cleanup_and_reconnect() {
    let f = Fixture::new();
    let runtime = Arc::new(McpRuntime::new());
    let mut p = f.params.clone();
    p["name"] = json!("eof");
    assert!(runtime
        .prompts_get(&f.context, &p, &Control::default())
        .is_err());
    p["name"] = json!("first");
    runtime
        .prompts_get(&f.context, &p, &Control::default())
        .unwrap();
    p["name"] = json!("wait");
    p["timeoutSeconds"] = json!(1);
    let start = Instant::now();
    assert!(runtime
        .prompts_get(&f.context, &p, &Control::default())
        .unwrap_err()
        .contains("timed out"));
    assert!(start.elapsed() < Duration::from_secs(4));
    p["timeoutSeconds"] = json!(30);
    for shutdown in [false, true] {
        let control = Control::default();
        let worker_control = control.clone();
        let worker_runtime = runtime.clone();
        let context = f.context.clone();
        let params = p.clone();
        let before = f
            .calls()
            .iter()
            .filter(|c| c["method"] == "prompts/get")
            .count();
        let worker = std::thread::spawn(move || {
            worker_runtime.prompts_get(&context, &params, &worker_control)
        });
        let start = Instant::now();
        while f
            .calls()
            .iter()
            .filter(|c| c["method"] == "prompts/get")
            .count()
            == before
        {
            assert!(start.elapsed() < Duration::from_secs(5));
            std::thread::sleep(Duration::from_millis(10));
        }
        if shutdown {
            runtime.shutdown_connections();
        } else {
            control.cancel();
        }
        assert!(worker.join().unwrap().unwrap_err().contains("cancelled"));
        assert!(start.elapsed() < Duration::from_secs(3));
    }
    p["name"] = json!("first");
    runtime
        .prompts_get(&f.context, &p, &Control::default())
        .unwrap();
    drop(runtime);
    assert_eq!(
        f.calls()
            .iter()
            .filter(|c| c["method"] == "initialize")
            .count(),
        5
    );
}
