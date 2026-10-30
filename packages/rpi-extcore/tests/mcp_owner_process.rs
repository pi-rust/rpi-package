#![cfg(feature = "test-support")]
use rpi_extcore::{
    host::HostSession,
    mcp::{self, McpConfig, McpOptions, McpOwner, McpServerConfig, StdioServer},
};
use serde_json::{json, Value};
use std::{collections::BTreeMap, time::Duration};
use tokio::io::{AsyncBufReadExt, BufReader};

#[tokio::test]
async fn owner_shutdown_joins_published_server_process_and_metadata_cleanup() {
    let (input, output) = tokio::io::duplex(1024 * 1024);
    let (host, events) = HostSession::test_connection_with_ready(
        input,
        128,
        json!({"type":"ready","protocol":"rpi","supportedCommands":["configure_external_tools","external_tool_response"]}),
    );
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let replies = events.clone();
    tokio::spawn(async move {
        let mut lines = BufReader::new(output).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            let request: Value = serde_json::from_str(&line).unwrap();
            let _ = tx.send(request.clone());
            let _ = replies.send(json!({"type":"response","id":request["id"],"success":true}));
        }
    });
    let owner = McpOwner::default();
    let marker = uuid::Uuid::new_v4().to_string();
    let config = McpConfig {
        scope: "owned-process".into(),
        cwd: std::env::current_dir().unwrap(),
        discovery_timeout: Duration::from_secs(10),
        servers: BTreeMap::from([(
            "fixture".into(),
            McpServerConfig::Stdio(StdioServer {
                command: env!("CARGO_BIN_EXE_rpi-extcore-mcp-fixture").into(),
                args: vec![],
                env: BTreeMap::from([("OWNED_PROCESS_MARKER".into(), marker.clone())]),
                cwd: None,
                enabled: true,
                timeout: 30,
                exposure: mcp::Exposure::Direct,
            }),
        )]),
    };
    let binding = mcp::attach_with_options(
        host.clone(),
        config,
        McpOptions {
            owner: Some(owner.clone()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let configured = rx.recv().await.unwrap();
    events.send(json!({"type":"external_tool_request","requestId":"pid","toolName":configured["tools"][0]["name"],"arguments":{}})).unwrap();
    let response = tokio::time::timeout(Duration::from_secs(10), rx.recv())
        .await
        .unwrap()
        .unwrap();
    let pid = response["result"]["structuredContent"]["pid"]
        .as_u64()
        .unwrap();
    let path = std::fs::read_dir(std::env::temp_dir())
        .unwrap()
        .filter_map(Result::ok)
        .find_map(|entry| {
            let path = entry.path();
            if !entry.file_name().to_string_lossy().starts_with("rpi-mcp-") {
                return None;
            }
            std::fs::read_to_string(&path)
                .ok()
                .filter(|s| s.contains(&marker))
                .map(|_| path)
        })
        .expect("private metadata exists before shutdown");
    drop(binding);
    owner.shutdown().await.unwrap();
    assert!(
        !path.exists(),
        "joined worker must remove its private metadata"
    );
    #[cfg(windows)]
    {
        let status = std::process::Command::new("powershell.exe").args(["-NoProfile", "-Command",
            &format!("if (Get-Process -Id {pid} -ErrorAction SilentlyContinue) {{ exit 1 }} else {{ exit 0 }}")]).status().unwrap();
        assert!(
            status.success(),
            "owned MCP server must be reaped before shutdown returns"
        );
    }
    #[cfg(target_os = "linux")]
    assert!(!std::path::Path::new(&format!("/proc/{pid}")).exists());
    assert!(rx.recv().await.unwrap()["tools"]
        .as_array()
        .unwrap()
        .is_empty());
    assert!(
        tokio::time::timeout(Duration::from_millis(50), rx.recv())
            .await
            .is_err(),
        "no late worker callbacks"
    );
    host.terminate().await;
}
