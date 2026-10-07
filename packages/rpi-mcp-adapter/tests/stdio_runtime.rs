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

    rpi_mcp_adapter::close_from_config(path.to_str().unwrap(), "fake").unwrap();
    let _ = fs::remove_file(path);
}

#[test]
fn stdio_runtime_rejects_unknown_tool() {
    let path = config_path();
    write_config(&path);
    let error = call_from_config(path.to_str().unwrap(), "fake", "missing", json!({})).unwrap_err();
    assert!(error.contains("does not expose tool"));
    rpi_mcp_adapter::close_from_config(path.to_str().unwrap(), "fake").unwrap();
    let _ = fs::remove_file(path);
}

#[test]
fn stdio_timeout_bounds_unresponsive_server() {
    let mut transport = rpi_mcp_adapter::StdioTransport::start(
        env!("CARGO_BIN_EXE_fake_mcp_server"),
        &["--hang".into()],
        &Default::default(),
        None,
    )
    .unwrap();
    transport.set_timeout(1);
    let started = std::time::Instant::now();
    let error = transport
        .request(&rpi_mcp_adapter::tools_list_request(1))
        .unwrap_err();
    assert!(
        error.contains("tools/list timed out after 1 seconds"),
        "{error}"
    );
    assert!(started.elapsed() < std::time::Duration::from_secs(5));
}

#[test]
fn stdio_skips_notifications_before_response() {
    let mut transport = rpi_mcp_adapter::StdioTransport::start(
        env!("CARGO_BIN_EXE_fake_mcp_server"),
        &["--notify".into()],
        &Default::default(),
        None,
    )
    .unwrap();
    transport.set_timeout(2);
    let response = transport
        .request(&rpi_mcp_adapter::tools_list_request(7))
        .unwrap();
    assert_eq!(response.id, 7);
}

fn config_with_mode(mode: &str) -> std::path::PathBuf {
    let path = config_path();
    fs::write(&path, json!({"mcpServers":{"fake":{"command":env!("CARGO_BIN_EXE_fake_mcp_server"),"args":[mode],"timeout":2}}}).to_string()).unwrap();
    path
}
#[test]
fn persistent_session_and_paginated_tools() {
    let path = config_with_mode("--paged");
    let discovered = discover_from_config(path.to_str().unwrap(), "fake").unwrap();
    assert_eq!(discovered.tools.len(), 4);
    let first = call_from_config(path.to_str().unwrap(), "fake", "counter", json!({})).unwrap();
    let second = call_from_config(path.to_str().unwrap(), "fake", "counter", json!({})).unwrap();
    let (pid, count) = first.split_once(':').unwrap();
    assert_eq!(count, "1");
    assert_eq!(second, format!("{pid}:2"));
    assert!(
        call_from_config(path.to_str().unwrap(), "fake", "fail", json!({}))
            .unwrap_err()
            .contains("intentional failure")
    );
    // A tool error must not discard the healthy session.
    assert_eq!(
        call_from_config(path.to_str().unwrap(), "fake", "counter", json!({})).unwrap(),
        format!("{pid}:4")
    );
    rpi_mcp_adapter::close_from_config(path.to_str().unwrap(), "fake").unwrap();
    let fresh = call_from_config(path.to_str().unwrap(), "fake", "counter", json!({})).unwrap();
    assert!(fresh.ends_with(":1"));
    assert_ne!(fresh.split(':').next().unwrap(), pid);
    rpi_mcp_adapter::close_from_config(path.to_str().unwrap(), "fake").unwrap();
    fs::remove_file(path).unwrap();
}
#[test]
fn pagination_rejects_repeated_cursor() {
    let path = config_with_mode("--bad-cursor");
    assert!(discover_from_config(path.to_str().unwrap(), "fake")
        .unwrap_err()
        .contains("repeated"));
    rpi_mcp_adapter::close_from_config(path.to_str().unwrap(), "fake").unwrap();
    fs::remove_file(path).unwrap();
}
#[test]
fn timeout_covers_a_full_stdin_pipe() {
    let mut transport = rpi_mcp_adapter::StdioTransport::start(
        env!("CARGO_BIN_EXE_fake_mcp_server"),
        &["--no-read".into()],
        &Default::default(),
        None,
    )
    .unwrap();
    transport.set_timeout(1);
    let start = std::time::Instant::now();
    let request = rpi_mcp_adapter::Request::new(
        1,
        "large",
        Some(json!({"data":"x".repeat(2 * 1024 * 1024)})),
    );
    assert!(transport
        .request(&request)
        .unwrap_err()
        .contains("timed out"));
    assert!(start.elapsed().as_secs_f32() < 3.0);
}
#[test]
fn cancellation_interrupts_pipe_write_and_cleans_workers() {
    let mut transport = rpi_mcp_adapter::StdioTransport::start(
        env!("CARGO_BIN_EXE_fake_mcp_server"),
        &["--no-read".into()],
        &Default::default(),
        None,
    )
    .unwrap();
    transport.set_timeout(60);
    let control = rpi_mcp_adapter::Control::default();
    let cancel = control.clone();
    let worker = std::thread::spawn(move || {
        let request = rpi_mcp_adapter::Request::new(
            1,
            "large",
            Some(json!({"data":"x".repeat(2 * 1024 * 1024)})),
        );
        transport.request_control(&request, &control)
    });
    std::thread::sleep(std::time::Duration::from_millis(100));
    let start = std::time::Instant::now();
    cancel.cancel();
    assert!(worker.join().unwrap().unwrap_err().contains("cancelled"));
    assert!(start.elapsed().as_secs_f32() < 2.0);
}
#[test]
fn server_ping_is_answered() {
    let path = config_with_mode("--ping");
    assert_eq!(
        call_from_config(
            path.to_str().unwrap(),
            "fake",
            "echo",
            json!({"value":"ping"})
        )
        .unwrap(),
        "echo:ping"
    );
    rpi_mcp_adapter::close_from_config(path.to_str().unwrap(), "fake").unwrap();
    fs::remove_file(path).unwrap();
}
#[test]
fn oversize_unterminated_stdout_is_rejected() {
    let mut transport = rpi_mcp_adapter::StdioTransport::start(
        env!("CARGO_BIN_EXE_fake_mcp_server"),
        &["--oversize".into()],
        &Default::default(),
        None,
    )
    .unwrap();
    transport.set_timeout(5);
    assert!(transport
        .request(&rpi_mcp_adapter::tools_list_request(1))
        .unwrap_err()
        .contains("16 MiB"));
}
#[test]
fn timeout_error_includes_server_stderr() {
    let path = config_with_mode("--hang-call");
    let error = call_from_config(path.to_str().unwrap(), "fake", "echo", json!({})).unwrap_err();
    assert!(error.contains("tools/call timed out"), "{error}");
    assert!(error.contains("deliberately blocked"), "{error}");
    rpi_mcp_adapter::close_from_config(path.to_str().unwrap(), "fake").unwrap();
    fs::remove_file(path).unwrap();
}

#[cfg(windows)]
#[test]
fn timeout_kills_descendants_holding_inherited_pipes() {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{
        GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };
    let mut transport = rpi_mcp_adapter::StdioTransport::start(
        env!("CARGO_BIN_EXE_fake_mcp_server"),
        &["--spawn-child".into()],
        &Default::default(),
        None,
    )
    .unwrap();
    transport.set_timeout(1);
    let start = std::time::Instant::now();
    let error = transport
        .request(&rpi_mcp_adapter::tools_list_request(1))
        .unwrap_err();
    assert!(start.elapsed() < std::time::Duration::from_secs(3));
    let pid: u32 = error
        .split("descendant-pid=")
        .nth(1)
        .unwrap()
        .lines()
        .next()
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    unsafe {
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if !process.is_null() {
            let mut exit = 259; // STILL_ACTIVE
            assert_ne!(GetExitCodeProcess(process, &mut exit), 0);
            CloseHandle(process);
            assert_ne!(exit, 259, "MCP descendant survived transport shutdown");
        }
    }
}
