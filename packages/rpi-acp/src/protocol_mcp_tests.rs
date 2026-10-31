#[test]
fn mcp_capabilities_advertise_http_but_not_sse() {
    let capabilities = initialize_result()["agentCapabilities"]["mcpCapabilities"].clone();
    assert_eq!(capabilities, json!({"http":true,"sse":false}));
}

#[tokio::test]
async fn old_open_session_helper_still_validates_cwd_without_spawning() {
    assert!(open_session("unused", PathBuf::from("relative"), None)
        .await
        .is_err());
}

/// All three dispatcher paths use prepare_session_tools before replay/response:
/// new/fresh load/fresh resume pass Some(empty) when omitted; live load/resume
/// pass None. Use an old host to prove that empty/omitted config sends no new RPC.
#[tokio::test]
async fn mcp_empty_and_omitted_configuration_preserve_old_host() {
    for explicit in [false, true] {
        let (stdin, commands) = tokio::io::duplex(8192);
        let (rpc, events) = rpi_extcore::host::HostSession::test_connection_with_ready(
            stdin,
            64,
            json!({"type":"ready","protocol":"rpi","supportedCommands":[]}),
        );
        let session = Session::attach_for_test(rpc).await.unwrap();
        let mut lines = BufReader::new(commands).lines();
        let operation = prepare_session_tools(
            &session,
            "expected",
            std::env::current_dir().unwrap(),
            explicit.then(Vec::new),
        );
        let native = async {
            let command: Value =
                serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
            assert_eq!(command["type"], "get_state");
            events
                .send(json!({"type":"response","id":command["id"],"success":true,
                "data":{"sessionId":"expected"}}))
                .unwrap();
        };
        let (result, _) = tokio::join!(operation, native);
        assert_eq!(result.unwrap()["sessionId"], "expected");
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(20), lines.next_line())
                .await
                .is_err()
        );
        session.kill().await;
    }
}

#[tokio::test]
async fn mcp_live_configuration_omission_same_and_changed_are_mapped_before_replay() {
    let (stdin, commands) = tokio::io::duplex(8192);
    let (rpc, events) = rpi_extcore::host::HostSession::test_connection_with_ready(
        stdin,
        64,
        json!({"type":"ready","protocol":"rpi","supportedCommands":[]}),
    );
    let session = Session::attach_for_test(rpc).await.unwrap();
    let mut lines = BufReader::new(commands).lines();
    let cwd = std::env::current_dir().unwrap();
    // Start with an explicit empty config, then exercise both live restore paths.
    for (method, config, succeeds) in [
        ("session/new", Some(Vec::new()), true),
        ("session/load", None, true),
        ("session/resume", None, true),
        ("session/load", Some(Vec::new()), true),
        ("session/resume", Some(Vec::new()), true),
        (
            "session/load",
            Some(vec![rpi_extcore::external_tools::ToolServer::Stdio(
                rpi_extcore::external_tools::StdioToolServer {
                    name: "changed".into(),
                    command: "must-not-spawn".into(),
                    args: vec![],
                    env: [("TOKEN".into(), "secret-canary".into())].into(),
                },
            )]),
            false,
        ),
        (
            "session/resume",
            Some(vec![rpi_extcore::external_tools::ToolServer::Stdio(
                rpi_extcore::external_tools::StdioToolServer {
                    name: "changed".into(),
                    command: "must-not-spawn".into(),
                    args: vec![],
                    env: [("TOKEN".into(), "secret-canary".into())].into(),
                },
            )]),
            false,
        ),
    ] {
        let operation = prepare_session_tools(&session, "expected", cwd.clone(), config);
        let native = async {
            let command: Value =
                serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
            assert_eq!(command["type"], "get_state");
            events
                .send(json!({"type":"response","id":command["id"],"success":true,
                "data":{"sessionId":"expected"}}))
                .unwrap();
        };
        let (result, _) = tokio::join!(operation, native);
        assert_eq!(result.is_ok(), succeeds, "{method}");
        if let Err(error) = result {
            assert!(error.contains("configuration changed"));
            assert!(!error.contains("secret-canary"));
        }
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(20), lines.next_line())
                .await
                .is_err()
        );
    }
    session.kill().await;
}

#[tokio::test]
async fn mcp_identity_is_checked_before_configuring_and_nonempty_old_host_fails_closed() {
    for matching_id in [false, true] {
        let (stdin, commands) = tokio::io::duplex(8192);
        let (rpc, events) = rpi_extcore::host::HostSession::test_connection_with_ready(
            stdin,
            64,
            json!({"type":"ready","protocol":"rpi","supportedCommands":[]}),
        );
        let session = Session::attach_for_test(rpc).await.unwrap();
        let mut lines = BufReader::new(commands).lines();
        let cwd = std::env::current_dir().unwrap();
        let params = json!({"mcpServers":[{"name":"fixture","command":"must-not-spawn",
            "args":[],"env":[{"name":"TOKEN","value":"secret-canary"}]}]});
        let servers = crate::mcp::parse(params.as_object().unwrap(), &cwd).unwrap();
        let operation = prepare_session_tools(&session, "expected", cwd, servers);
        let native = async {
            let command: Value =
                serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
            assert_eq!(command["type"], "get_state");
            events
                .send(json!({"type":"response","id":command["id"],"success":true,
                "data":{"sessionId":if matching_id {"expected"} else {"wrong"}}}))
                .unwrap();
        };
        let (result, _) = tokio::join!(operation, native);
        let error = result.unwrap_err();
        if !matching_id {
            assert_eq!(error, "backend opened a different sessionId");
        } else {
            assert_ne!(error, "backend opened a different sessionId");
        }
        assert!(!error.contains("secret-canary"));
        // In particular no external-tool configure or history replay RPC was sent.
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(20), lines.next_line())
                .await
                .is_err()
        );
        session.kill().await;
    }
}

struct WorkspaceFixture {
    root: PathBuf,
    saved: PathBuf,
    id: String,
}
impl WorkspaceFixture {
    fn new() -> Self {
        let id = uuid::Uuid::new_v4().to_string();
        let root = std::env::temp_dir().join(format!("cwd-worker-{id}"));
        std::fs::create_dir_all(root.join("A")).unwrap();
        std::fs::create_dir_all(root.join("B")).unwrap();
        let saved = root.join("saved.jsonl");
        Self { root, saved, id }
    }
    fn record(&self, cwd: &std::path::Path) {
        std::fs::write(
            &self.saved,
            format!(
                "{}\n",
                json!({
                    "kind":"header", "version":4, "id":self.id,
                    "createdAt":0, "cwd":cwd, "metadata":{}
                })
            ),
        )
        .unwrap();
    }
}
impl Drop for WorkspaceFixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// Load and resume share open_session_with_tools. Exercise that production
/// entry with an actual v4 header: mismatch returns before Session::spawn,
/// so neither native, MCP discovery nor replay can run. This is a source-level
/// adapter test, not a real-native restoration integration test.
#[tokio::test]
async fn mcp_fresh_load_and_resume_reject_recorded_workspace_mismatch_before_spawn() {
    let fixture = WorkspaceFixture::new();
    fixture.record(&fixture.root.join("A"));
    for method in ["session/load", "session/resume"] {
        let cwd = fixture.root.join("B");
        let params = json!({"mcpServers":[{"name":"fixture",
            "command":"./bin/must-not-spawn", "args":[], "env":[]}]});
        let servers = crate::mcp::parse(params.as_object().unwrap(), &cwd)
            .unwrap()
            .unwrap();
        let rpi_extcore::external_tools::ToolServer::Stdio(server) = &servers[0] else {
            panic!("expected stdio");
        };
        assert_eq!(
            PathBuf::from(&server.command),
            cwd.join("bin/must-not-spawn")
        );
        let result = open_session_with_tools(&fixture.id, cwd, Some(&fixture.saved), servers).await;
        let error = match result {
            Ok(_) => panic!("{method} unexpectedly spawned a session"),
            Err(error) => error,
        };
        assert!(
            error.contains("saved session workspace differs"),
            "{method}: {error}"
        );
        assert!(!error.contains("must-not-spawn"));
    }
}

#[test]
fn mcp_restore_workspace_matching_missing_and_compatibility_preflight() {
    let fixture = WorkspaceFixture::new();
    let a = fixture.root.join("A");
    let b = fixture.root.join("B");
    for method in ["session/load", "session/resume"] {
        for recorded in [
            &b,
            &b.join("..").join("B"),
            &fixture.root.join("missing"),
            std::path::Path::new(""),
            std::path::Path::new("."),
        ] {
            fixture.record(recorded);
            assert!(
                validate_session_workspace(&fixture.id, &b, Some(&fixture.saved), true).is_ok(),
                "{method}: {recorded:?}"
            );
        }
        // A file is not an adoptable native cwd, either.
        let file = fixture.root.join("not-a-directory");
        std::fs::write(&file, "").unwrap();
        fixture.record(&file);
        assert!(validate_session_workspace(&fixture.id, &b, Some(&fixture.saved), true).is_ok());
        fixture.record(&a);
        // Omitted/explicit-empty MCP and brand-new sessions do not opt in.
        assert!(validate_session_workspace(&fixture.id, &b, Some(&fixture.saved), false).is_ok());
        assert!(validate_session_workspace(&fixture.id, &b, None, true).is_ok());
        std::fs::write(&fixture.saved, "legacy synthetic header").unwrap();
        assert!(validate_session_workspace(&fixture.id, &b, Some(&fixture.saved), false).is_ok());
        assert!(validate_session_workspace(&fixture.id, &b, Some(&fixture.saved), true).is_err());
    }
}

#[cfg(windows)]
#[test]
fn mcp_restore_workspace_accepts_windows_case_alias() {
    let fixture = WorkspaceFixture::new();
    let b = fixture.root.join("B");
    fixture.record(&PathBuf::from(b.to_string_lossy().to_uppercase()));
    assert!(validate_session_workspace(&fixture.id, &b, Some(&fixture.saved), true).is_ok());
}

#[cfg(unix)]
#[test]
fn mcp_restore_workspace_accepts_symlink_alias() {
    let fixture = WorkspaceFixture::new();
    let b = fixture.root.join("B");
    let alias = fixture.root.join("alias");
    std::os::unix::fs::symlink(&b, &alias).unwrap();
    fixture.record(&alias);
    assert!(validate_session_workspace(&fixture.id, &b, Some(&fixture.saved), true).is_ok());
}

/// A real loopback HTTP MCP peer, but an in-memory native peer. No model,
/// installed configuration, credentials or external network is involved.
struct HttpMcpFixture {
    url: String,
    requests: Arc<std::sync::Mutex<Vec<Value>>>,
    task: tokio::task::JoinHandle<()>,
}
impl HttpMcpFixture {
    async fn start() -> Self {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!(
            "http://{}/mcp?key=secret-canary",
            listener.local_addr().unwrap()
        );
        let requests = Arc::new(std::sync::Mutex::new(Vec::new()));
        let recorded = requests.clone();
        let task = tokio::spawn(async move {
            loop {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut head = Vec::new();
                while !head.ends_with(b"\r\n\r\n") {
                    assert!(head.len() < 32 * 1024);
                    head.push(stream.read_u8().await.unwrap());
                }
                let head = String::from_utf8(head).unwrap().to_ascii_lowercase();
                assert!(head.contains("authorization: secret-canary\r\n"));
                let len: usize = head
                    .lines()
                    .find_map(|line| line.strip_prefix("content-length:"))
                    .unwrap()
                    .trim()
                    .parse()
                    .unwrap();
                assert!(len < 64 * 1024);
                let mut body = vec![0; len];
                stream.read_exact(&mut body).await.unwrap();
                let request: Value = serde_json::from_slice(&body).unwrap();
                recorded.lock().unwrap().push(request.clone());
                let result = match request["method"].as_str().unwrap() {
                    "initialize" => {
                        json!({"protocolVersion":"2025-06-18", "capabilities":{"tools":{}},
                        "serverInfo":{"name":"loopback", "version":"1"}})
                    }
                    "tools/list" => {
                        json!({"tools":[{"name":"echo", "inputSchema":{"type":"object"}}]})
                    }
                    "notifications/initialized" => {
                        stream.write_all(b"HTTP/1.1 202 Accepted\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await.unwrap();
                        continue;
                    }
                    _ => panic!("unexpected MCP method"),
                };
                let body =
                    json!({"jsonrpc":"2.0", "id":request["id"], "result":result}).to_string();
                let reply = format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
                stream.write_all(reply.as_bytes()).await.unwrap();
            }
        });
        Self {
            url,
            requests,
            task,
        }
    }
    fn params(&self, header: &str) -> Value {
        json!({"mcpServers":[{"type":"http", "name":"loopback", "url":self.url,
            "headers":[{"name":header, "value":"secret-canary"}]}]})
    }
}
impl Drop for HttpMcpFixture {
    fn drop(&mut self) {
        self.task.abort();
    }
}

#[tokio::test]
async fn mcp_http_new_and_live_load_resume_use_real_loopback_discovery() {
    let fixture = HttpMcpFixture::start().await;
    let (stdin, commands) = tokio::io::duplex(65536);
    let (rpc, events) = rpi_extcore::host::HostSession::test_connection_with_ready(
        stdin,
        64,
        json!({"type":"ready","protocol":"rpi",
            "supportedCommands":["configure_external_tools","external_tool_response"],
            "supportedEvents":["external_tool_request","external_tool_request_closed"]}),
    );
    let session = Session::attach_for_test(rpc).await.unwrap();
    let mut lines = BufReader::new(commands).lines();
    let cwd = std::env::current_dir().unwrap();
    let config =
        crate::mcp::parse(fixture.params("Authorization").as_object().unwrap(), &cwd).unwrap();
    let install = prepare_session_tools(&session, "expected", cwd.clone(), config);
    let native = async {
        for expected in ["get_state", "configure_external_tools"] {
            let command: Value =
                serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
            assert_eq!(command["type"], expected);
            let data = if expected == "get_state" {
                json!({"sessionId":"expected"})
            } else {
                assert_eq!(command["tools"].as_array().unwrap().len(), 1);
                assert!(!command.to_string().contains("secret-canary"));
                json!({"tools":[command["tools"][0]["name"]]})
            };
            events
                .send(json!({"type":"response","id":command["id"],"success":true,"data":data}))
                .unwrap();
        }
    };
    let (result, _) = tokio::time::timeout(std::time::Duration::from_secs(15), async {
        tokio::join!(install, native)
    })
    .await
    .unwrap();
    result.unwrap();
    let contacts = fixture.requests.lock().unwrap().len();
    assert!(contacts >= 2);
    for method in ["session/load", "session/resume"] {
        for variant in ["omitted", "same", "changed"] {
            let mut params = fixture.params("aUtHoRiZaTiOn");
            if variant == "omitted" {
                params = json!({});
            }
            if variant == "changed" {
                params["mcpServers"][0]["headers"][0]["value"] = json!("different-secret");
            }
            let config = crate::mcp::parse(params.as_object().unwrap(), &cwd).unwrap();
            let operation = prepare_session_tools(&session, "expected", cwd.clone(), config);
            let native = async {
                let command: Value =
                    serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
                assert_eq!(command["type"], "get_state");
                events
                    .send(json!({"type":"response","id":command["id"],"success":true,
                    "data":{"sessionId":"expected"}}))
                    .unwrap();
            };
            let (result, _) = tokio::time::timeout(std::time::Duration::from_secs(5), async {
                tokio::join!(operation, native)
            })
            .await
            .unwrap();
            assert_eq!(result.is_ok(), variant != "changed", "{method}: {variant}");
            if let Err(error) = result {
                assert!(error.contains("configuration changed"));
                assert!(!error.contains("secret"));
            }
            assert_eq!(fixture.requests.lock().unwrap().len(), contacts);
            assert!(
                tokio::time::timeout(std::time::Duration::from_millis(20), lines.next_line())
                    .await
                    .is_err()
            );
        }
    }
    session.kill().await;
}

#[tokio::test]
async fn mcp_http_invalid_configuration_and_restore_preflights_never_contact() {
    let fixture = HttpMcpFixture::start().await;
    let workspace = WorkspaceFixture::new();
    workspace.record(&workspace.root.join("A"));
    let cwd = workspace.root.join("B");
    for method in ["session/new", "session/load", "session/resume"] {
        for field in ["url", "headers", "command"] {
            let mut params = fixture.params("Authorization");
            params["mcpServers"][0][field] = Value::Null;
            assert!(
                crate::mcp::parse(params.as_object().unwrap(), &cwd).is_err(),
                "{method}"
            );
        }
    }
    for method in ["session/load", "session/resume"] {
        let config = crate::mcp::parse(fixture.params("Authorization").as_object().unwrap(), &cwd)
            .unwrap()
            .unwrap();
        let result =
            open_session_with_tools(&workspace.id, cwd.clone(), Some(&workspace.saved), config)
                .await;
        let error = match result {
            Ok(_) => panic!("{method} unexpectedly spawned"),
            Err(error) => error,
        };
        assert!(error.contains("saved session workspace differs"));
    }
    workspace.record(&cwd);
    let config = crate::mcp::parse(fixture.params("Authorization").as_object().unwrap(), &cwd)
        .unwrap()
        .unwrap();
    assert!(
        open_session_with_tools("different-id", cwd.clone(), Some(&workspace.saved), config)
            .await
            .is_err()
    );
    // Even callers bypassing the wire parser cannot launch native with bad HTTP.
    let invalid = rpi_extcore::external_tools::ToolServer::Http(
        rpi_extcore::external_tools::HttpToolServer {
            name: "invalid".into(),
            url: "http://user:secret-canary@localhost/".into(),
            headers: Default::default(),
        },
    );
    assert!(open_session_with_tools("unused", cwd, None, vec![invalid])
        .await
        .is_err());
    assert!(fixture.requests.lock().unwrap().is_empty());
}
