use rpi_mcp_adapter::{call_from_config, discover_from_config};
use serde_json::json;
use std::fs;
use std::time::{SystemTime, UNIX_EPOCH};

fn config_path() -> std::path::PathBuf {
    let suffix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!("rpi-mcp-test-{}-{suffix}.json", std::process::id()))
}

fn write_config(path: &std::path::Path) {
    let command = env!("CARGO_BIN_EXE_fake_mcp_server");
    let config = json!({
        "mcpServers": {
            "fake": {
                "command": command,
                "enabled": true,
                "timeout": 5
            }
        }
    });
    fs::write(path, config.to_string()).unwrap();
}

#[test]
fn stdio_runtime_discovers_and_calls_tool() {
    let path = config_path();
    write_config(&path);

    let discovered = discover_from_config(path.to_str().unwrap(), "fake").unwrap();
    assert_eq!(discovered.name, "fake");
    assert_eq!(discovered.info.name, "rpi-fake-mcp");
    assert_eq!(discovered.tools.len(), 1);
    assert_eq!(discovered.tools[0].name, "echo");

    let result = call_from_config(
        path.to_str().unwrap(),
        "fake",
        "echo",
        json!({"value":"hello"}),
    )
    .unwrap();
    assert_eq!(result, "echo:hello");

    let _ = fs::remove_file(path);
}

#[test]
fn stdio_runtime_rejects_unknown_tool() {
    let path = config_path();
    write_config(&path);
    let error = call_from_config(path.to_str().unwrap(), "fake", "missing", json!({})).unwrap_err();
    assert!(error.contains("does not expose tool"));
    let _ = fs::remove_file(path);
}
