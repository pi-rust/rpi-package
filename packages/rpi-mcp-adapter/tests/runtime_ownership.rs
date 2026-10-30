#![forbid(unsafe_code)]

use rpi_mcp_core::{call_result_text, Control, McpRuntime, RequestContext};
use serde_json::{json, Value};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

fn updates(runtime: &McpRuntime) -> Vec<rpi_mcp_core::ResourceUpdate> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let batch = runtime.take_resource_updates(Some("same-scope"));
        if !batch.is_empty() || Instant::now() >= deadline {
            return batch;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn identical_scope_and_config_are_isolated_between_runtime_owners() {
    let path = std::env::temp_dir().join(format!(
        "rpi-mcp-owner-{}-{}.json",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::write(
        &path,
        json!({"mcpServers":{"fake":{
            "command":env!("CARGO_BIN_EXE_fake_mcp_server"),
            "args":["--paged", "--stateful-resources"], "timeout":5
        }}})
        .to_string(),
    )
    .unwrap();
    let context = RequestContext {
        scope: "same-scope".into(),
        cwd: std::env::current_dir().unwrap(),
    };
    let control = Control::default();
    let first = std::sync::Arc::new(McpRuntime::new());
    let last_owner = first.clone();
    let second = McpRuntime::new();
    let base = json!({"configPath":path, "server":"fake"});
    let call = |runtime: &McpRuntime| {
        let mut params = base.clone();
        params["tool"] = json!("counter");
        params["arguments"] = json!({});
        call_result_text(&runtime.call(&context, &params, &control).unwrap())
    };
    let identity = |value: String| -> (u32, u32) {
        let (pid, counter) = value.split_once(':').unwrap();
        (pid.parse().unwrap(), counter.parse().unwrap())
    };
    let (pid_a, count_a) = identity(call(&first));
    let (pid_b, count_b) = identity(call(&second));
    assert_ne!(
        pid_a, pid_b,
        "same scope/config reused another owner's process"
    );
    assert_eq!((count_a, count_b), (1, 1));
    assert_eq!(identity(call(&first)), (pid_a, 2));
    assert_eq!(identity(call(&second)), (pid_b, 2));

    let resource = |runtime: &McpRuntime, action: &str, uri: &str| {
        let mut params: Value = base.clone();
        params["action"] = json!(action);
        params["uri"] = json!(uri);
        runtime.resources(&context, &params, &control).unwrap()
    };
    resource(&first, "subscribe", "mem://first");
    resource(&second, "subscribe", "mem://second");
    call(&first);
    let batch = updates(&first);
    assert_eq!(batch.len(), 1);
    assert_eq!(batch[0].uri, "mem://first");
    assert!(second.take_resource_updates(None).is_empty());
    call(&second);
    let batch = updates(&second);
    assert_eq!(batch.len(), 1);
    assert_eq!(batch[0].uri, "mem://second");
    assert!(first.take_resource_updates(None).is_empty());

    // Releasing one Arc must not close the runtime. Releasing its last owner
    // must close it without an explicit close/shutdown call.
    drop(first);
    assert_eq!(identity(call(&last_owner)).0, pid_a);
    let started = Instant::now();
    drop(last_owner);
    assert!(started.elapsed() < Duration::from_secs(3));
    #[cfg(windows)]
    {
        assert!(!rpi_plugin_sdk::process::process_is_running(pid_a).unwrap());
        assert!(rpi_plugin_sdk::process::process_is_running(pid_b).unwrap());
    }
    assert_eq!(identity(call(&second)).0, pid_b);
    drop(second);
    #[cfg(windows)]
    assert!(!rpi_plugin_sdk::process::process_is_running(pid_b).unwrap());
    std::fs::remove_file(path).unwrap();
}
