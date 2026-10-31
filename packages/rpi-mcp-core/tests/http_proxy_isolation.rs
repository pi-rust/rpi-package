//! Proxy environment is confined to fresh subprocesses, never this test process.
use rpi_mcp_core::{
    ConfiguredHttp, Control, CoreRequestContext, DiscoveryLimits, HttpServer, McpRuntime,
};
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::process::{Command, Stdio};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

struct Canary {
    address: String,
    requests: Arc<Mutex<Vec<String>>>,
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}
impl Canary {
    fn new(ip: &str) -> Self {
        let listener = TcpListener::bind((ip, 0)).unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap().to_string();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let (r, s) = (requests.clone(), stop.clone());
        let worker = thread::spawn(move || {
            while !s.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        stream.set_nonblocking(false).unwrap();
                        stream
                            .set_read_timeout(Some(Duration::from_secs(5)))
                            .unwrap();
                        stream
                            .set_write_timeout(Some(Duration::from_secs(5)))
                            .unwrap();
                        let mut header = Vec::new();
                        let mut byte = [0];
                        while !header.ends_with(b"\r\n\r\n") {
                            stream.read_exact(&mut byte).unwrap();
                            header.push(byte[0]);
                            assert!(header.len() < 32768);
                        }
                        let header = String::from_utf8(header).unwrap();
                        let len = header
                            .lines()
                            .find_map(|line| {
                                let (name, value) = line.split_once(':')?;
                                name.eq_ignore_ascii_case("content-length")
                                    .then(|| value.trim().parse::<usize>().unwrap())
                            })
                            .unwrap_or(0);
                        assert!(len < 65536);
                        let mut body = vec![0; len];
                        stream.read_exact(&mut body).unwrap();
                        r.lock().unwrap().push(header);
                        let value: Value = if body.is_empty() {
                            Value::Null
                        } else {
                            serde_json::from_slice(&body).unwrap()
                        };
                        let (status, body) = if value.get("id").is_some() {
                            let result = if value["method"] == "initialize" {
                                json!({"protocolVersion":"2025-03-26","capabilities":{"tools":{}},"serverInfo":{"name":"canary","version":"1"}})
                            } else {
                                json!({"tools":[{"name":"canary","inputSchema":{"type":"object"}}]})
                            };
                            (
                                200,
                                json!({"jsonrpc":"2.0","id":value["id"],"result":result})
                                    .to_string(),
                            )
                        } else {
                            (202, String::new())
                        };
                        write!(stream, "HTTP/1.1 {status} Canary\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(2))
                    }
                    Err(e) => panic!("canary accept: {e}"),
                }
            }
        });
        Self {
            address,
            requests,
            stop,
            worker: Some(worker),
        }
    }
    fn records(&self) -> Vec<String> {
        self.requests.lock().unwrap().clone()
    }
}
impl Drop for Canary {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        self.worker.take().unwrap().join().unwrap();
    }
}

#[test]
fn configured_no_proxy_and_legacy_proxy_in_isolated_processes() {
    // Use the universally available loopback address (macOS need not have an
    // alias for 127.0.0.2). The legacy half is the positive control proving the
    // same endpoint is actually proxied with this isolated environment.
    let direct = Canary::new("127.0.0.1");
    let proxy = Canary::new("127.0.0.1");
    let url = format!("http://{}/mcp", direct.address);
    for mode in ["configured", "legacy"] {
        let mut child = Command::new(std::env::current_exe().unwrap());
        child.env_clear();
        // Positive allowlist, no inherited auth, home, agent, proxy or secret settings.
        for (key, value) in std::env::vars_os() {
            if [
                "SYSTEMROOT",
                "WINDIR",
                "PATH",
                "PATHEXT",
                "TEMP",
                "TMP",
                "SYSTEMDRIVE",
            ]
            .iter()
            .any(|allowed| key.to_string_lossy().eq_ignore_ascii_case(allowed))
            {
                child.env(key, value);
            }
        }
        let proxy_url = format!("http://proxy-user:PROXY-SECRET-CANARY@{}", proxy.address);
        for key in [
            "HTTP_PROXY",
            "HTTPS_PROXY",
            "ALL_PROXY",
            "http_proxy",
            "https_proxy",
            "all_proxy",
        ] {
            child.env(key, &proxy_url);
        }
        child
            .env("NO_PROXY", "")
            .env("no_proxy", "")
            .env("RPI_PROXY_CHILD_MODE", mode)
            .env("RPI_PROXY_CHILD_URL", &url)
            .args(["--exact", "proxy_child", "--ignored", "--nocapture"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = child.spawn().unwrap();
        let until = Instant::now() + Duration::from_secs(30);
        loop {
            if child.try_wait().unwrap().is_some() {
                break;
            }
            if Instant::now() >= until {
                child.kill().unwrap();
                let output = child.wait_with_output().unwrap();
                panic!("proxy child timed out: {output:?}");
            }
            thread::sleep(Duration::from_millis(10));
        }
        let output = child.wait_with_output().unwrap();
        assert!(output.status.success(), "{mode}: {output:?}");
        if mode == "configured" {
            let records = direct.records();
            assert_eq!(records.len(), 3, "initialize, initialized, tools/list");
            assert!(
                proxy.records().is_empty(),
                "configured path contacted proxy"
            );
            for request in records {
                assert!(request.starts_with("POST /mcp HTTP/1.1\r\n"));
                assert!(!request.to_ascii_lowercase().contains("proxy-authorization"));
                assert!(!request.contains("PROXY-SECRET-CANARY"));
            }
        } else {
            assert_eq!(
                direct.records().len(),
                3,
                "legacy must not contact direct listener"
            );
            let records = proxy.records();
            assert_eq!(records.len(), 1);
            assert!(records[0].starts_with(&format!("POST {url} HTTP/1.1\r\n")));
            assert!(records[0]
                .to_ascii_lowercase()
                .contains("proxy-authorization: basic "));
        }
    }
}

#[test]
#[ignore = "only launched by isolated parent with explicit proxy environment"]
fn proxy_child() {
    let mode = std::env::var("RPI_PROXY_CHILD_MODE").unwrap();
    let config: HttpServer = serde_json::from_value(
        json!({"url":std::env::var("RPI_PROXY_CHILD_URL").unwrap(),"timeout":5}),
    )
    .unwrap();
    if mode == "configured" {
        let context = CoreRequestContext {
            scope: "proxy-child".into(),
            cwd: std::env::current_dir().unwrap(),
        };
        let target = ConfiguredHttp::new(&context, "canary", config).unwrap();
        let found = McpRuntime::new()
            .discover_http(
                &target,
                DiscoveryLimits {
                    max_tools: 1,
                    max_catalog_bytes: 1024,
                },
                &Control::default(),
            )
            .unwrap();
        assert_eq!(found.tools[0].name, "canary");
    } else {
        assert_eq!(mode, "legacy");
        // Same legacy HTTP constructor used by config-file runtime access.
        let response = rpi_mcp_core::http_request(
            &rpi_mcp_core::McpServerConfig::Http(config),
            &rpi_mcp_core::initialize_request(1, "proxy-child", "1"),
            5,
        )
        .unwrap();
        assert!(response.result.is_some());
    }
}
